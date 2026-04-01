use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use http::Error as HttpError;
use http_body_util::{BodyExt, BodyStream, Full};
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use salvo::{http::ResBody, prelude::*};

use crate::{
    config::{AtomicConfig, Config, Mode},
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
            missing_upstream_message: "No upstream configured with mode = 'anthropic'",
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
            missing_upstream_message: "No upstream configured with mode = 'openai_responses'",
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

    let body_bytes = prepare_request_body(plan, body_bytes, &cfg, stats, req, res).await;
    let Some(body_bytes) = body_bytes else {
        return;
    };

    let Some(selected_upstream) = select_upstream(config, plan) else {
        tracing::error!("{}", plan.missing_upstream_message);
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    log_selected_upstream(plan.kind, &selected_upstream);

    let body_bytes = apply_upstream_model(body_bytes, &selected_upstream.model);
    let (upstream_url, host) =
        make_proxy_url(&selected_upstream.endpoint, selected_upstream.mode, req);

    let proxy_req = match build_proxy_request(
        req,
        &upstream_url,
        host.as_ref(),
        &selected_upstream.api_key,
        body_bytes,
    ) {
        Ok(request) => request,
        Err(error) => {
            tracing::error!("Failed to build proxy request: {}", error);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    match client.request(proxy_req).await {
        Ok(proxy_resp) => {
            forward_proxy_response(plan.kind, proxy_resp, res, &cfg).await;
        }
        Err(error) => {
            tracing::error!("{}: {}", proxy_failure_label(plan.kind), error);
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render("Bad Gateway");
        }
    }
}

async fn prepare_request_body(
    plan: ProxyPlan,
    body_bytes: Bytes,
    cfg: &Config,
    stats: Option<&Arc<RequestStats>>,
    _req: &Request,
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

fn select_upstream(config: &Arc<AtomicConfig>, plan: ProxyPlan) -> Option<SelectedUpstream> {
    let selector = config.get_upstream_selector()?;
    let (index, endpoint, model, api_key, mode) = selector.next_by_mode(plan.upstream_mode)?;

    Some(SelectedUpstream {
        index,
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        api_key: api_key.to_owned(),
        mode,
    })
}

fn log_selected_upstream(kind: ProxyKind, upstream: &SelectedUpstream) {
    let prefix = match kind {
        ProxyKind::Claude => "🔄 选中的",
        ProxyKind::Codex => "🔄 Codex 代理选中的",
    };

    tracing::info!(
        "{} Upstream[{}]: endpoint={}, model={}, api_key: {}***, mode={:?}",
        prefix,
        upstream.index,
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
) {
    let (parts, body) = proxy_resp.into_parts();
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

        for (name, value) in parts.headers {
            if let Some(name) = name
                && name.as_str() != "content-length"
            {
                res.headers_mut().insert(name, value);
            }
        }

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
        return;
    }

    if matches!(kind, ProxyKind::Codex) {
        tracing::info!("=== Codex 非 SSE 响应开始 ===");
    }

    let body_bytes = match BodyExt::collect(body).await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            tracing::error!("Failed to collect response body: {}", error);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
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
    for (name, value) in parts.headers {
        if let Some(name) = name {
            let name_str = name.as_str();
            if name_str != "content-length" && name_str != "content-encoding" {
                res.headers_mut().insert(name, value);
            }
        }
    }
    res.body(body_bytes.to_vec());
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
