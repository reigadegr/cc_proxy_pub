use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{body::Incoming, http::response::Parts};
use salvo::prelude::*;

use super::{decompress::decompress_gzip_if_needed, forward::copy_response_headers};
use crate::{
    entry::proxy_failure_label,
    types::{FailedUpstreamResponse, ProxyKind},
};

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
