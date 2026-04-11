use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use http::{Error as HttpError, HeaderName, HeaderValue};
use http_body_util::{BodyExt, BodyStream, Full};
use hyper::{
    Request as HyperRequest, Response as HyperResponse, body::Incoming, http::response::Parts,
};
use my_config::{AtomicConfig, Config, Mode, UpstreamSelector};
use salvo::{http::ResBody, prelude::*};

use crate::gateway::{
    HttpClient, RequestStats,
    handler::{
        request::{
            filter_req_body, get_req_body, log_request_meta, make_proxy_url,
            override_model_in_body, parse_body_json, req_local_intercept_by_url,
            req_local_intercept_from_json, serialize_body_json,
        },
        response::decompress_gzip_if_needed,
        system_prompt::{CUSTOM_SYSTEM_PROMPT, insert_custom_system_prompt_in_json},
        thinking_patch::patch_reasoning_for_thinking_mode_in_json,
    },
    service::{calculate_tokens, calculate_tokens_from_json, log_full_body, log_full_response},
};

#[derive(Clone, Copy)]
enum ProxyKind {
    Anthropic,
    OpenAI,
}

#[derive(Clone, Copy)]
struct ProxyPlan {
    kind: ProxyKind,
    upstream_mode: Mode,
    missing_upstream_message: &'static str,
}

struct SelectedUpstream {
    index: usize,
    name: String,
    base_url: String,
    model: String,
    api_key: String,
    user_agent: Option<String>,
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

pub async fn handle_anthropic(
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    stats: &Arc<RequestStats>,
    client: &Arc<HttpClient>,
) {
    run_proxy(
        ProxyPlan {
            kind: ProxyKind::Anthropic,
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

pub async fn handle_openai(
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    client: &Arc<HttpClient>,
    mode: Mode,
) {
    run_proxy(proxy_plan_for_mode(mode), req, res, config, None, client).await;
}

const fn proxy_plan_for_mode(mode: Mode) -> ProxyPlan {
    match mode {
        Mode::AnthropicDirect => ProxyPlan {
            kind: ProxyKind::Anthropic,
            upstream_mode: Mode::AnthropicDirect,
            missing_upstream_message: "No upstream configured with mode including 'anthropic'",
        },
        Mode::OpenAIResponses => ProxyPlan {
            kind: ProxyKind::OpenAI,
            upstream_mode: Mode::OpenAIResponses,
            missing_upstream_message: "No upstream configured with mode including 'openai_responses'",
        },
        Mode::OpenAIChat => ProxyPlan {
            kind: ProxyKind::OpenAI,
            upstream_mode: Mode::OpenAIChat,
            missing_upstream_message: "No upstream configured with mode including 'openai_chat'",
        },
    }
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
    let request_url = req.uri().to_string();

    log_request_meta(req.method().as_str(), &request_url, req.headers());

    let body_bytes = prepare_request_body(plan, body_bytes, &request_url, &cfg, stats, res);
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
        let (upstream_url, host) = make_proxy_url(&selected_upstream.base_url, ctx.req);

        let proxy_req = match build_proxy_request(
            ctx.req,
            &upstream_url,
            host.as_ref(),
            &selected_upstream.api_key,
            selected_upstream.user_agent.as_deref(),
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
                            ctx.cfg.server.log_res_body,
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

fn prepare_request_body(
    plan: ProxyPlan,
    body_bytes: Bytes,
    request_url: &str,
    cfg: &Config,
    stats: Option<&Arc<RequestStats>>,
    res: &mut Response,
) -> Option<Bytes> {
    let mut current = body_bytes;
    let mut token_stats_recorded = false;

    if matches!(plan.kind, ProxyKind::Anthropic)
        && req_local_intercept_by_url(res, request_url, cfg)
    {
        return None;
    }

    if matches!(plan.kind, ProxyKind::Anthropic)
        && !current.is_empty()
        && let Ok(body_str) = std::str::from_utf8(&current)
        && cfg.server.log_req_body
    {
        log_full_body(body_str);
    }

    if matches!(plan.kind, ProxyKind::Anthropic) && !current.is_empty() {
        if let Ok(mut request_json) = parse_body_json(&current) {
            if req_local_intercept_from_json(res, &request_json, request_url, cfg) {
                return None;
            }

            insert_custom_system_prompt_in_json(&mut request_json, CUSTOM_SYSTEM_PROMPT);
            filter_req_body(&mut request_json);
            if patch_reasoning_for_thinking_mode_in_json(&mut request_json) {
                tracing::debug!("🩹 修补 thinking 模式缺失的 reasoning_content");
            }

            if let Some(stats) = stats {
                calculate_tokens_from_json(stats.as_ref(), &request_json);
                token_stats_recorded = true;
            }

            current = match serialize_body_json(&request_json) {
                Ok(body) => body,
                Err(error) => {
                    tracing::error!("Failed to serialize Claude request body: {}", error);
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                    return None;
                }
            };
        } else {
            tracing::debug!("Skipping Claude body refinement because request JSON parsing failed");
        }
    }

    if !current.is_empty()
        && let Ok(body_str) = std::str::from_utf8(&current)
    {
        if cfg.server.log_req_body {
            log_full_body(body_str);
        }

        if matches!(plan.kind, ProxyKind::Anthropic)
            && !token_stats_recorded
            && let Some(stats) = stats
        {
            calculate_tokens(stats.as_ref(), body_str);
        }
    }

    Some(current)
}

fn select_upstream(selector: &UpstreamSelector, plan: ProxyPlan) -> Option<SelectedUpstream> {
    let (index, name, base_url, model, api_key, user_agent, mode) =
        selector.next_by_mode(plan.upstream_mode)?;

    Some(SelectedUpstream {
        index,
        name: name.to_owned(),
        base_url: base_url.to_owned(),
        model: model.to_owned(),
        api_key: api_key.to_owned(),
        user_agent: user_agent.map(str::to_owned),
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
        ProxyKind::Anthropic => "🔄 选中的",
        ProxyKind::OpenAI => "🔄 OpenAI 代理选中的",
    };

    tracing::info!(
        "{} Upstream[{}] name={} (attempt {}/{}): base_url={}, model={}, api_key: {}***, mode={:?}",
        prefix,
        upstream.index,
        if upstream.name.is_empty() {
            "-"
        } else {
            upstream.name.as_str()
        },
        attempt,
        total_attempts,
        upstream.base_url,
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
    upstream_user_agent: Option<&str>,
    body_bytes: Bytes,
) -> Result<HyperRequest<Full<Bytes>>, HttpError> {
    let override_user_agent = resolve_upstream_user_agent(upstream_user_agent, host);
    let mut proxy_req_builder = HyperRequest::builder()
        .method(req.method())
        .uri(upstream_url);

    for (name, value) in req.headers() {
        let name_str = name.as_str();
        let should_skip_original_user_agent =
            override_user_agent.is_some() && name == http::header::USER_AGENT;
        if name_str != "host"
            && name_str != "authorization"
            && name_str != "content-length"
            && !should_skip_original_user_agent
        {
            proxy_req_builder = proxy_req_builder.header(name, value);
        }
    }

    proxy_req_builder = proxy_req_builder.header("Authorization", format!("Bearer {api_key}"));
    proxy_req_builder = proxy_req_builder.header("host", host);
    if let Some(user_agent) = override_user_agent {
        proxy_req_builder = proxy_req_builder.header(http::header::USER_AGENT, user_agent);
    }

    proxy_req_builder.body(Full::new(body_bytes))
}

fn resolve_upstream_user_agent(
    upstream_user_agent: Option<&str>,
    host: &str,
) -> Option<HeaderValue> {
    let upstream_user_agent = upstream_user_agent?.trim();
    if upstream_user_agent.is_empty() {
        return None;
    }

    match HeaderValue::from_str(upstream_user_agent) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::error!(
                "Ignoring invalid upstream user_agent for host {}: {}",
                host,
                error
            );
            None
        }
    }
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

        let log_body = cfg.server.log_res_body;
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
                        tracing::info!("{}: {}", sse_error_label(kind), error);
                        None
                    }
                }
            })
            .map(Ok::<bytes::Bytes, std::convert::Infallible>);

        res.body(ResBody::stream(stream));

        if matches!(kind, ProxyKind::OpenAI) {
            tracing::info!("=== OpenAI SSE 流式响应结束 ===");
        }
        return Ok(());
    }

    if matches!(kind, ProxyKind::OpenAI) {
        tracing::info!("=== OpenAI 非 SSE 响应开始 ===");
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

    if cfg.server.log_res_body {
        if matches!(kind, ProxyKind::OpenAI) {
            tracing::info!("=== OpenAI 原始上游响应 ===");
            tracing::info!("{}", body_str);
            tracing::info!("=== OpenAI 原始上游响应结束 ===");
        } else {
            log_full_response(&body_str);
        }
    } else if matches!(kind, ProxyKind::OpenAI) {
        tracing::info!("=== OpenAI 非 SSE 响应: {} bytes ===", body_bytes.len());
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
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, retrying next upstream; base_url={}, model={}, body={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                upstream.base_url,
                upstream.model,
                body
            );
        } else {
            tracing::warn!(
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, retrying next upstream; base_url={}, model={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                upstream.base_url,
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
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, no upstream left; base_url={}, model={}, body={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                upstream.base_url,
                upstream.model,
                body
            );
        } else {
            tracing::error!(
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, no upstream left; base_url={}, model={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                upstream.base_url,
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
            "{}: upstream[{}] name={} attempt {}/{} transport error, retrying next upstream; base_url={}, model={}, error={}",
            proxy_failure_label(kind),
            upstream.index,
            if upstream.name.is_empty() {
                "-"
            } else {
                upstream.name.as_str()
            },
            attempt,
            total_attempts,
            upstream.base_url,
            upstream.model,
            error_message
        );
    } else {
        tracing::error!(
            "{}: upstream[{}] name={} attempt {}/{} transport error, no upstream left; base_url={}, model={}, error={}",
            proxy_failure_label(kind),
            upstream.index,
            if upstream.name.is_empty() {
                "-"
            } else {
                upstream.name.as_str()
            },
            attempt,
            total_attempts,
            upstream.base_url,
            upstream.model,
            error_message
        );
    }
}

const fn sse_passthrough_log(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Anthropic => "⏭️ 直接透传 Anthropic 格式 SSE 流",
        ProxyKind::OpenAI => "⏭️ 直接透传 OpenAI 格式 SSE 流",
    }
}

const fn sse_start_log(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Anthropic => "=== SSE 流式响应开始 ===",
        ProxyKind::OpenAI => "=== OpenAI SSE 流式响应开始 ===",
    }
}

const fn sse_error_label(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Anthropic => "SSE 流读取错误",
        ProxyKind::OpenAI => "OpenAI SSE 流读取错误",
    }
}

const fn proxy_failure_label(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Anthropic => "Proxy request failed",
        ProxyKind::OpenAI => "OpenAI proxy request failed",
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::uri::Scheme;
    use http_body_util::Full;
    use hyper::Request as HyperRequest;
    use my_config::{Config, Mode, OptimizationConfig, ServerConfig};
    use salvo::{Request, http::StatusCode};

    use super::{
        ProxyKind, ProxyPlan, build_proxy_request, prepare_request_body, proxy_plan_for_mode,
        should_retry_upstream_status,
    };

    fn make_request(user_agent: &str) -> Request {
        let req_result = HyperRequest::builder()
            .method("POST")
            .uri("http://localhost/v1/messages")
            .header(http::header::USER_AGENT, user_agent)
            .header("x-test-header", "keep-me")
            .body(Bytes::from_static(br#"{"model":"demo"}"#));
        let Ok(req) = req_result else {
            panic!("failed to build test request");
        };

        Request::from_hyper(req, Scheme::HTTP)
    }

    fn build_proxy_request_for_test(
        req: &Request,
        upstream_user_agent: Option<&str>,
    ) -> HyperRequest<Full<Bytes>> {
        let proxy_req_result = build_proxy_request(
            req,
            "https://upstream.example.com/v1/messages",
            "upstream.example.com",
            "secret",
            upstream_user_agent,
            Bytes::new(),
        );
        let Ok(proxy_req) = proxy_req_result else {
            panic!("failed to build proxy request");
        };
        proxy_req
    }

    fn header_value_as_str(
        request: &HyperRequest<Full<Bytes>>,
        header_name: http::header::HeaderName,
    ) -> Option<&str> {
        request.headers().get(header_name)?.to_str().ok()
    }

    fn named_header_value_as_str<'a>(
        request: &'a HyperRequest<Full<Bytes>>,
        header_name: &str,
    ) -> Option<&'a str> {
        request.headers().get(header_name)?.to_str().ok()
    }

    fn test_config() -> Config {
        Config {
            server: ServerConfig::default(),
            upstream: Vec::new(),
            optimizations: OptimizationConfig::default(),
        }
    }

    #[test]
    fn forced_upstream_mode_limits_retry_attempts_to_one() {
        let selector = my_config::UpstreamSelector::new_with_global_user_agents(
            my_config::GlobalUserAgentConfig::default(),
            1,
            vec![
                my_config::UpstreamConfig {
                    enable: true,
                    name: "first".to_string(),
                    base_url: "https://first.example.com".to_string(),
                    model: "model-1".to_string(),
                    api_keys: vec!["key-1".to_string()],
                    user_agent_claude: None,
                    user_agent_codex: None,
                    mode: vec![Mode::AnthropicDirect].into(),
                },
                my_config::UpstreamConfig {
                    enable: false,
                    name: "forced".to_string(),
                    base_url: "https://forced.example.com".to_string(),
                    model: "model-2".to_string(),
                    api_keys: vec!["key-2".to_string()],
                    user_agent_claude: None,
                    user_agent_codex: None,
                    mode: vec![Mode::AnthropicDirect].into(),
                },
            ],
        );
        let Some(selector) = selector else {
            panic!("测试数据已确保 upstreams 非空");
        };

        assert_eq!(selector.matching_count_by_mode(Mode::AnthropicDirect), 1);
    }

    fn anthropic_plan() -> ProxyPlan {
        proxy_plan_for_mode(Mode::AnthropicDirect)
    }

    #[test]
    fn proxy_plan_for_mode_maps_supported_modes() {
        let anthropic = proxy_plan_for_mode(Mode::AnthropicDirect);
        assert!(matches!(anthropic.kind, ProxyKind::Anthropic));
        assert_eq!(anthropic.upstream_mode, Mode::AnthropicDirect);
        assert!(anthropic.missing_upstream_message.contains("anthropic"));

        let responses = proxy_plan_for_mode(Mode::OpenAIResponses);
        assert!(matches!(responses.kind, ProxyKind::OpenAI));
        assert_eq!(responses.upstream_mode, Mode::OpenAIResponses);
        assert!(
            responses
                .missing_upstream_message
                .contains("openai_responses")
        );

        let chat = proxy_plan_for_mode(Mode::OpenAIChat);
        assert!(matches!(chat.kind, ProxyKind::OpenAI));
        assert_eq!(chat.upstream_mode, Mode::OpenAIChat);
        assert!(chat.missing_upstream_message.contains("openai_chat"));
    }

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

    #[test]
    fn build_proxy_request_preserves_original_user_agent_without_override() {
        let req = make_request("Original-UA/1.0");
        let proxy_req = build_proxy_request_for_test(&req, None);

        assert_eq!(
            header_value_as_str(&proxy_req, http::header::USER_AGENT),
            Some("Original-UA/1.0")
        );
        assert_eq!(
            named_header_value_as_str(&proxy_req, "x-test-header"),
            Some("keep-me")
        );
    }

    #[test]
    fn build_proxy_request_overrides_user_agent_when_configured() {
        let req = make_request("Original-UA/1.0");
        let proxy_req = build_proxy_request_for_test(&req, Some("Configured-UA/2.0"));

        assert_eq!(
            header_value_as_str(&proxy_req, http::header::USER_AGENT),
            Some("Configured-UA/2.0")
        );
        assert_eq!(
            named_header_value_as_str(&proxy_req, "x-test-header"),
            Some("keep-me")
        );
    }

    #[test]
    fn build_proxy_request_keeps_original_user_agent_for_blank_override() {
        let req = make_request("Original-UA/1.0");
        let proxy_req = build_proxy_request_for_test(&req, Some("   "));

        assert_eq!(
            header_value_as_str(&proxy_req, http::header::USER_AGENT),
            Some("Original-UA/1.0")
        );
    }

    #[test]
    fn build_proxy_request_keeps_original_user_agent_for_invalid_override() {
        let req = make_request("Original-UA/1.0");
        let proxy_req = build_proxy_request_for_test(&req, Some("bad\r\nua"));

        assert_eq!(
            header_value_as_str(&proxy_req, http::header::USER_AGENT),
            Some("Original-UA/1.0")
        );
    }

    #[test]
    fn prepare_request_body_intercepts_count_tokens_url_with_invalid_json_body() {
        let mut res = salvo::Response::new();

        let body = prepare_request_body(
            anthropic_plan(),
            Bytes::from_static(b"not json"),
            "/v1/messages/count_tokens?foo=bar",
            &test_config(),
            None,
            &mut res,
        );

        assert!(body.is_none());
        assert_eq!(res.status_code, Some(StatusCode::OK));
        assert_eq!(
            res.headers()
                .get("x-cc-proxy-optimization")
                .and_then(|value| value.to_str().ok()),
            Some("max_tokens_mock")
        );
    }

    #[test]
    fn prepare_request_body_intercepts_count_tokens_url_with_empty_body() {
        let mut res = salvo::Response::new();

        let body = prepare_request_body(
            anthropic_plan(),
            Bytes::new(),
            "/v1/messages/count_tokens?foo=bar",
            &test_config(),
            None,
            &mut res,
        );

        assert!(body.is_none());
        assert_eq!(res.status_code, Some(StatusCode::OK));
        assert_eq!(
            res.headers()
                .get("x-cc-proxy-optimization")
                .and_then(|value| value.to_str().ok()),
            Some("max_tokens_mock")
        );
    }
}
