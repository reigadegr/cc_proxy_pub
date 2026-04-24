use bytes::Bytes;
use futures_util::StreamExt;
use http::{HeaderName, HeaderValue};
use http_body_util::{BodyExt, BodyStream};
use hyper::{Response as HyperResponse, body::Incoming, http::response::Parts};
use my_config::Config;
use salvo::{http::ResBody, prelude::*};

use my_handler::response::decompress_gzip_if_needed;

use super::entry::proxy_failure_label;
use super::service::log_full_response;
use super::types::{FailedUpstreamResponse, ProxyKind, UpstreamAttemptFailure};

pub fn should_retry_upstream_status(status: StatusCode) -> bool {
    !status.is_success()
}

pub async fn forward_proxy_response(
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

pub async fn collect_failed_upstream_response(
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

pub fn render_failed_upstream_response(
    res: &mut Response,
    failed_response: FailedUpstreamResponse,
) {
    res.status_code(failed_response.status);
    copy_response_headers(res, failed_response.headers, true);
    res.body(failed_response.body);
}

pub fn copy_response_headers<I>(res: &mut Response, headers: I, strip_content_encoding: bool)
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
