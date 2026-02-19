use super::{
    HttpClient, RequestStats,
    service::{calculate_tokens, log_full_body, log_full_response, log_request_headers},
};
use crate::config::AtomicConfig;
use http_body_util::{BodyExt, Full};
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use salvo::prelude::*;
use std::sync::Arc;

/// 代理请求 handler
#[handler]
pub async fn proxy_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    // 获取配置、统计和 HTTP 客户端
    let Ok(config) = depot.obtain::<Arc<AtomicConfig>>() else {
        tracing::error!("AtomicConfig not found in depot");
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let Ok(stats) = depot.obtain::<Arc<RequestStats>>() else {
        tracing::error!("RequestStats not found in depot");
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let Ok(client) = depot.obtain::<Arc<HttpClient>>() else {
        tracing::error!("HttpClient not found in depot");
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };
    let cfg = config.get();

    // 记录请求头
    log_request_headers(
        req.method().as_str(),
        req.uri().to_string().as_str(),
        req.headers(),
    );

    // 收集请求体
    let body_bytes = match req.body_mut().collect().await {
        Ok(body) => body.to_bytes(),
        Err(e) => {
            tracing::error!("Failed to collect request body: {}", e);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    // 记录请求体并计算 token
    if !body_bytes.is_empty()
        && let Ok(body_str) = std::str::from_utf8(&body_bytes)
    {
        log_full_body(body_str);
        calculate_tokens(stats, body_str);
    }

    // 解析 endpoint
    let endpoint = &cfg.endpoint;
    let host_str = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint);

    let (host, base_path) = host_str.split_once('/').unwrap_or((host_str, ""));

    // 构建上游 URL
    let original_path = req.uri().path();
    let query = req.uri().query().unwrap_or("");
    let query_str = if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    };

    let new_path = if base_path.is_empty() {
        format!("{original_path}{query_str}")
    } else {
        format!(
            "/{}/{}{}",
            base_path,
            original_path.trim_start_matches('/'),
            query_str
        )
    };

    let scheme = if endpoint.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    let upstream_url = format!("{scheme}://{host}{new_path}");

    tracing::info!("Proxying to: {}", upstream_url);

    // 构建代理请求
    let mut proxy_req_builder = HyperRequest::builder()
        .method(req.method())
        .uri(&upstream_url);

    // 复制请求头（跳过 host 和 authorization，会使用配置中的 api_key）
    for (name, value) in req.headers() {
        let name_str = name.as_str();
        if name_str != "host" && name_str != "authorization" {
            proxy_req_builder = proxy_req_builder.header(name, value);
        }
    }

    // 注入 Authorization
    proxy_req_builder =
        proxy_req_builder.header("Authorization", format!("Bearer {}", cfg.api_key));
    proxy_req_builder = proxy_req_builder.header("host", host);

    // 设置请求体
    let proxy_req = match proxy_req_builder.body(Full::new(body_bytes.clone())) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to build proxy request: {}", e);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    // 使用共享的 HTTP 客户端发送请求
    match client.request(proxy_req).await {
        Ok(proxy_resp) => {
            let proxy_resp: HyperResponse<Incoming> = proxy_resp;
            let (parts, body) = proxy_resp.into_parts();
            let status_code = parts.status.as_u16();

            // 收集响应体
            let body_bytes = match body.collect().await {
                Ok(b) => b.to_bytes(),
                Err(e) => {
                    tracing::error!("Failed to collect response body: {}", e);
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                    return;
                }
            };
            let body_str = String::from_utf8_lossy(&body_bytes);

            // 记录响应体
            log_full_response(&body_str);

            // 构建响应
            res.status_code(
                StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );
            for (name, value) in parts.headers {
                if let Some(name) = name {
                    res.headers_mut().insert(name, value);
                }
            }
            res.body(body_bytes.to_vec());
        }
        Err(e) => {
            tracing::error!("Proxy request failed: {}", e);
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render("Bad Gateway");
        }
    }
}
