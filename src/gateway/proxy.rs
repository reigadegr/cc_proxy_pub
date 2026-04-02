use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use http::{Error as HttpError, HeaderName, HeaderValue};
use http_body_util::{BodyExt, BodyStream, Full};
use hyper::{
    Request as HyperRequest, Response as HyperResponse, body::Incoming, http::response::Parts,
};
use salvo::{http::ResBody, prelude::*};

use crate::{
    config::{AtomicConfig, Config, Mode, selector::UpstreamSelector},
    gateway::{
        HttpClient, RequestStats,
        handler::{
            request::{
                filter_req_body, get_req_body, log_request_meta, make_proxy_url,
                override_model_in_body, req_local_intercept,
            },
            response::decompress_gzip_if_needed,
            system_prompt::{CUSTOM_SYSTEM_PROMPT, insert_custom_system_prompt},
            thinking_patch::patch_reasoning_for_thinking_mode,
        },
        service::{calculate_tokens, log_full_body, log_full_response},
    },
};

#[derive(Clone, Copy)]
enum ProxyKind {
    Claude,
    Codex,
}

#[derive(Clone, Copy)]
struct ProxyPlan {
    kind: ProxyKind,
    upstream_mode: Mode,
    missing_upstream_message: &'static str,
}

struct SelectedUpstream {
    index: usize,
    endpoint: String,
    model: String,
    api_key: String,
    mode: Mode,
}

struct FailedUpstreamResponse {
    status: StatusCode,
    headers: Vec<(Option<HeaderName>, HeaderValue)>,
    body: Vec<u8>,
    body_text: String,
}

enum UpstreamAttemptFailure {
    Response(FailedUpstreamResponse),
    Transport(String),
}

enum RetryLoopResult {
    Forwarded,
    Failed(UpstreamAttemptFailure),
    NoSelection,
}

struct RetryContext<'a> {
    req: &'a Request,
    res: &'a mut Response,
    client: &'a Arc<HttpClient>,
    cfg: &'a Config,
    selector: &'a UpstreamSelector,
    body_bytes: &'a Bytes,
    max_attempts: usize,
}

pub async fn handle_claude(
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    stats: &Arc<RequestStats>,
    client: &Arc<HttpClient>,
) {
    run_proxy(
        ProxyPlan {
            kind: ProxyKind::Claude,
            upstream_mode: Mode::AnthropicDirect,
            missing_upstream_message: "No upstream configured with mode including 'anthropic'",
        },
        req,
        res,
        config,
        Some(stats),
        client,
    )
    .await;
}

pub async fn handle_codex(
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    client: &Arc<HttpClient>,
) {
    run_proxy(
        ProxyPlan {
            kind: ProxyKind::Codex,
            upstream_mode: Mode::OpenAIResponses,
            missing_upstream_message: "No upstream configured with mode including 'openai_responses'",
        },
        req,
        res,
        config,
        None,
        client,
    )
    .await;
}

async fn run_proxy(
    plan: ProxyPlan,
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    stats: Option<&Arc<RequestStats>>,
    client: &Arc<HttpClient>,
) {
    let body_bytes = match get_req_body(req).await {
        Ok(body) => body,
        Err(error) => {
            tracing::error!("{error}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let cfg = config.get();

    if matches!(plan.kind, ProxyKind::Claude) && req_local_intercept(req, res, &body_bytes, &cfg) {
        return;
    }

    log_request_meta(
        req.method().as_str(),
        req.uri().to_string().as_str(),
        req.headers(),
    );

    let body_bytes = prepare_request_body(plan, body_bytes, &cfg, stats, res).await;
    let Some(body_bytes) = body_bytes else {
        return;
    };

    let Some(selector) = config.get_upstream_selector() else {
        tracing::error!("{}", plan.missing_upstream_message);
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let max_attempts = selector.matching_count_by_mode(plan.upstream_mode);
    if max_attempts == 0 {
        tracing::error!("{}", plan.missing_upstream_message);
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    }

    match try_upstreams(
        plan,
        RetryContext {
            req,
            res,
            client,
            cfg: &cfg,
            selector: selector.as_ref(),
            body_bytes: &body_bytes,
            max_attempts,
        },
    )
    .await
    {
        RetryLoopResult::Forwarded => {}
        RetryLoopResult::Failed(UpstreamAttemptFailure::Response(failed_response)) => {
            tracing::error!(
                "{} after exhausting {} upstream attempt(s); returning last upstream response",
                proxy_failure_label(plan.kind),
                max_attempts
            );
            render_failed_upstream_response(res, failed_response);
        }
        RetryLoopResult::Failed(UpstreamAttemptFailure::Transport(error_message)) => {
            tracing::error!(
                "{} after exhausting {} upstream attempt(s): {}",
                proxy_failure_label(plan.kind),
                max_attempts,
                error_message
            );
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render("Bad Gateway");
        }
        RetryLoopResult::NoSelection => {
            tracing::error!(
                "{}: selector returned no upstream during retry loop",
                proxy_failure_label(plan.kind)
            );
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render("Bad Gateway");
        }
    }
}

async fn try_upstreams(plan: ProxyPlan, ctx: RetryContext<'_>) -> RetryLoopResult {
    let mut last_failure = None;

    for attempt in 1..=ctx.max_attempts {
        let Some(selected_upstream) = select_upstream(ctx.selector, plan) else {
            break;
        };

        log_selected_upstream(plan.kind, &selected_upstream, attempt, ctx.max_attempts);

        let attempt_body = apply_upstream_model(ctx.body_bytes.clone(), &selected_upstream.model);
        let (upstream_url, host) =
            make_proxy_url(&selected_upstream.endpoint, selected_upstream.mode, ctx.req);

        let proxy_req = match build_proxy_request(
            ctx.req,
            &upstream_url,
            host.as_ref(),
            &selected_upstream.api_key,
            attempt_body,
        ) {
            Ok(request) => request,
            Err(error) => {
                tracing::error!("Failed to build proxy request: {}", error);
                ctx.res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                return RetryLoopResult::Forwarded;
            }
        };

        match ctx.client.request(proxy_req).await {
            Ok(proxy_resp) => {
                match forward_proxy_response(plan.kind, proxy_resp, ctx.res, ctx.cfg).await {
                    Ok(()) => return RetryLoopResult::Forwarded,
                    Err(UpstreamAttemptFailure::Response(failed_response)) => {
                        log_failed_upstream_response(
                            plan.kind,
                            &selected_upstream,
                            attempt,
                            ctx.max_attempts,
                            ctx.cfg.log_res_body,
                            &failed_response,
                        );
                        last_failure = Some(UpstreamAttemptFailure::Response(failed_response));
                    }
                    Err(UpstreamAttemptFailure::Transport(error_message)) => {
                        log_transport_failure(
                            plan.kind,
                            &selected_upstream,
                            attempt,
                            ctx.max_attempts,
                            &error_message,
                        );
                        last_failure = Some(UpstreamAttemptFailure::Transport(error_message));
                    }
                }
            }
            Err(error) => {
                let error_message = error.to_string();
                log_transport_failure(
                    plan.kind,
                    &selected_upstream,
                    attempt,
                    ctx.max_attempts,
                    &error_message,
                );
                last_failure = Some(UpstreamAttemptFailure::Transport(error_message));
            }
        }
    }

    last_failure.map_or_else(|| RetryLoopResult::NoSelection, RetryLoopResult::Failed)
}

async fn prepare_request_body(
    plan: ProxyPlan,
    body_bytes: Bytes,
    cfg: &Config,
    stats: Option<&Arc<RequestStats>>,
    res: &mut Response,
) -> Option<Bytes> {
    let mut current = body_bytes;

    if matches!(plan.kind, ProxyKind::Claude)
        && !current.is_empty()
        && let Ok(body_str) = std::str::from_utf8(&current)
        && cfg.log_req_body
    {
        log_full_body(body_str);
    }

    if matches!(plan.kind, ProxyKind::Claude) && !current.is_empty() {
        current = insert_custom_system_prompt(&current, CUSTOM_SYSTEM_PROMPT).unwrap_or(current);
        current = match filter_req_body(&current).await {
            Ok(body) => body,
            Err(error) => {
                tracing::error!("{error}");
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                return None;
            }
        };
        if let Some(patched) = patch_reasoning_for_thinking_mode(&current) {
            tracing::debug!("🩹 修补 thinking 模式缺失的 reasoning_content");
            current = patched;
        }
    }

    if !current.is_empty()
        && let Ok(body_str) = std::str::from_utf8(&current)
    {
        if cfg.log_req_body {
            log_full_body(body_str);
        }

        if matches!(plan.kind, ProxyKind::Claude)
            && let Some(stats) = stats
        {
            calculate_tokens(stats.as_ref(), body_str);
        }
    }

    Some(current)
}

fn select_upstream(selector: &UpstreamSelector, plan: ProxyPlan) -> Option<SelectedUpstream> {
    let (index, endpoint, model, api_key, mode) = selector.next_by_mode(plan.upstream_mode)?;

    Some(SelectedUpstream {
        index,
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        api_key: api_key.to_owned(),
        mode,
    })
}

fn log_selected_upstream(
    kind: ProxyKind,
    upstream: &SelectedUpstream,
    attempt: usize,
    total_attempts: usize,
) {
    let prefix = match kind {
        ProxyKind::Claude => "🔄 选中的",
        ProxyKind::Codex => "🔄 Codex 代理选中的",
    };

    tracing::info!(
        "{} Upstream[{}] (attempt {}/{}): endpoint={}, model={}, api_key: {}***, mode={:?}",
        prefix,
        upstream.index,
        attempt,
        total_attempts,
        upstream.endpoint,
        upstream.model,
        upstream.api_key.chars().take(8).collect::<String>(),
        upstream.mode
    );
}

fn apply_upstream_model(body_bytes: Bytes, model: &str) -> Bytes {
    if model.is_empty() || body_bytes.is_empty() {
        return body_bytes;
    }

    override_model_in_body(&body_bytes, model).unwrap_or(body_bytes)
}

fn build_proxy_request(
    req: &Request,
    upstream_url: &str,
    host: &str,
    api_key: &str,
    body_bytes: Bytes,
) -> Result<HyperRequest<Full<Bytes>>, HttpError> {
    let mut proxy_req_builder = HyperRequest::builder()
        .method(req.method())
        .uri(upstream_url);

    for (name, value) in req.headers() {
        let name_str = name.as_str();
        if name_str != "host" && name_str != "authorization" && name_str != "content-length" {
            proxy_req_builder = proxy_req_builder.header(name, value);
        }
    }

    proxy_req_builder = proxy_req_builder.header("Authorization", format!("Bearer {api_key}"));
    proxy_req_builder = proxy_req_builder.header("host", host);

    proxy_req_builder.body(Full::new(body_bytes))
}

async fn forward_proxy_response(
    kind: ProxyKind,
    proxy_resp: HyperResponse<Incoming>,
    res: &mut Response,
    cfg: &Config,
) -> Result<(), UpstreamAttemptFailure> {
    let (parts, body) = proxy_resp.into_parts();
    if should_retry_upstream_status(parts.status) {
        return Err(UpstreamAttemptFailure::Response(
            collect_failed_upstream_response(kind, parts, body).await,
        ));
    }

    let status_code = parts.status.as_u16();
    let is_sse = parts
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/event-stream"));

    if is_sse {
        tracing::info!("{}", sse_start_log(kind));
        res.status_code(
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        );

        copy_response_headers(res, parts.headers, false);

        let log_body = cfg.log_res_body;
        tracing::info!("{}", sse_passthrough_log(kind));

        let stream = BodyStream::new(body)
            .inspect(move |frame| {
                if log_body
                    && let Ok(frame) = frame
                    && let Some(data) = frame.data_ref()
                    && let Ok(text) = std::str::from_utf8(data)
                {
                    tracing::info!("{}", text);
                }
            })
            .filter_map(move |frame| async move {
                match frame {
                    Ok(frame) => frame.into_data().ok(),
                    Err(error) => {
                        tracing::error!("{}: {}", sse_error_label(kind), error);
                        None
                    }
                }
            })
            .map(Ok::<bytes::Bytes, std::convert::Infallible>);

        res.body(ResBody::stream(stream));

        if matches!(kind, ProxyKind::Codex) {
            tracing::info!("=== Codex SSE 流式响应结束 ===");
        }
        return Ok(());
    }

    if matches!(kind, ProxyKind::Codex) {
        tracing::info!("=== Codex 非 SSE 响应开始 ===");
    }

    let body_bytes = match BodyExt::collect(body).await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            return Err(UpstreamAttemptFailure::Transport(format!(
                "Failed to collect response body: {error}"
            )));
        }
    };

    let content_encoding = parts
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok());
    let body_bytes = decompress_gzip_if_needed(&body_bytes, content_encoding);
    let body_str = String::from_utf8_lossy(&body_bytes);

    if cfg.log_res_body {
        if matches!(kind, ProxyKind::Codex) {
            tracing::info!("=== Codex 原始上游响应 ===");
            tracing::info!("{}", body_str);
            tracing::info!("=== Codex 原始上游响应结束 ===");
        } else {
            log_full_response(&body_str);
        }
    } else if matches!(kind, ProxyKind::Codex) {
        tracing::info!("=== Codex 非 SSE 响应: {} bytes ===", body_bytes.len());
    }

    res.status_code(StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    copy_response_headers(res, parts.headers, true);
    res.body(body_bytes.to_vec());
    Ok(())
}

fn should_retry_upstream_status(status: StatusCode) -> bool {
    !status.is_success()
}

async fn collect_failed_upstream_response(
    kind: ProxyKind,
    parts: Parts,
    body: Incoming,
) -> FailedUpstreamResponse {
    let content_encoding = parts
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let status = StatusCode::from_u16(parts.status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = parts.headers.into_iter().collect();

    let body_bytes = match BodyExt::collect(body).await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            tracing::error!(
                "{}: failed to collect upstream error body: {}",
                proxy_failure_label(kind),
                error
            );
            Bytes::new()
        }
    };
    let body_bytes = decompress_gzip_if_needed(&body_bytes, content_encoding.as_deref());
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();

    FailedUpstreamResponse {
        status,
        headers,
        body: body_bytes.to_vec(),
        body_text,
    }
}

fn render_failed_upstream_response(res: &mut Response, failed_response: FailedUpstreamResponse) {
    res.status_code(failed_response.status);
    copy_response_headers(res, failed_response.headers, true);
    res.body(failed_response.body);
}

fn copy_response_headers<I>(res: &mut Response, headers: I, strip_content_encoding: bool)
where
    I: IntoIterator<Item = (Option<HeaderName>, HeaderValue)>,
{
    for (name, value) in headers {
        if let Some(name) = name {
            let name_str = name.as_str();
            if name_str != "content-length"
                && (!strip_content_encoding || name_str != "content-encoding")
            {
                res.headers_mut().insert(name, value);
            }
        }
    }
}

fn log_failed_upstream_response(
    kind: ProxyKind,
    upstream: &SelectedUpstream,
    attempt: usize,
    total_attempts: usize,
    log_response_body: bool,
    failed_response: &FailedUpstreamResponse,
) {
    if attempt < total_attempts {
        if log_response_body {
            let body = if failed_response.body_text.is_empty() {
                "<empty body>"
            } else {
                failed_response.body_text.as_str()
            };
            tracing::warn!(
                "{}: upstream[{}] attempt {}/{} returned status {}, retrying next upstream; endpoint={}, model={}, body={}",
                proxy_failure_label(kind),
                upstream.index,
                attempt,
                total_attempts,
                failed_response.status,
                upstream.endpoint,
                upstream.model,
                body
            );
        } else {
            tracing::warn!(
                "{}: upstream[{}] attempt {}/{} returned status {}, retrying next upstream; endpoint={}, model={}",
                proxy_failure_label(kind),
                upstream.index,
                attempt,
                total_attempts,
                failed_response.status,
                upstream.endpoint,
                upstream.model
            );
        }
    } else {
        if log_response_body {
            let body = if failed_response.body_text.is_empty() {
                "<empty body>"
            } else {
                failed_response.body_text.as_str()
            };
            tracing::error!(
                "{}: upstream[{}] attempt {}/{} returned status {}, no upstream left; endpoint={}, model={}, body={}",
                proxy_failure_label(kind),
                upstream.index,
                attempt,
                total_attempts,
                failed_response.status,
                upstream.endpoint,
                upstream.model,
                body
            );
        } else {
            tracing::error!(
                "{}: upstream[{}] attempt {}/{} returned status {}, no upstream left; endpoint={}, model={}",
                proxy_failure_label(kind),
                upstream.index,
                attempt,
                total_attempts,
                failed_response.status,
                upstream.endpoint,
                upstream.model
            );
        }
    }
}

fn log_transport_failure(
    kind: ProxyKind,
    upstream: &SelectedUpstream,
    attempt: usize,
    total_attempts: usize,
    error_message: &str,
) {
    if attempt < total_attempts {
        tracing::warn!(
            "{}: upstream[{}] attempt {}/{} transport error, retrying next upstream; endpoint={}, model={}, error={}",
            proxy_failure_label(kind),
            upstream.index,
            attempt,
            total_attempts,
            upstream.endpoint,
            upstream.model,
            error_message
        );
    } else {
        tracing::error!(
            "{}: upstream[{}] attempt {}/{} transport error, no upstream left; endpoint={}, model={}, error={}",
            proxy_failure_label(kind),
            upstream.index,
            attempt,
            total_attempts,
            upstream.endpoint,
            upstream.model,
            error_message
        );
    }
}

const fn sse_passthrough_log(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Claude => "⏭️ 直接透传 Anthropic 格式 SSE 流",
        ProxyKind::Codex => "⏭️ Codex: 直接透传 OpenAI Responses SSE 流",
    }
}

const fn sse_start_log(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Claude => "=== SSE 流式响应开始 ===",
        ProxyKind::Codex => "=== Codex SSE 流式响应开始 ===",
    }
}

const fn sse_error_label(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Claude => "SSE 流读取错误",
        ProxyKind::Codex => "Codex SSE 流读取错误",
    }
}

const fn proxy_failure_label(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Claude => "Proxy request failed",
        ProxyKind::Codex => "Codex proxy request failed",
    }
}

#[cfg(test)]
mod tests {
    use salvo::http::StatusCode;

    use super::should_retry_upstream_status;

    #[test]
    fn should_retry_when_upstream_status_is_not_success() {
        assert!(should_retry_upstream_status(StatusCode::BAD_REQUEST));
        assert!(should_retry_upstream_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry_upstream_status(
            StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!should_retry_upstream_status(StatusCode::OK));
        assert!(!should_retry_upstream_status(StatusCode::CREATED));
    }
}
