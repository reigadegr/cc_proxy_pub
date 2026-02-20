use super::{
    HttpClient, RequestStats,
    optimization::try_local_optimization,
    service::{calculate_tokens, log_full_body, log_full_response, log_request_headers},
};
use crate::config::AtomicConfig;
use http_body_util::{BodyExt, Full};
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use salvo::prelude::*;
use std::sync::Arc;

/// 尝试覆盖请求体中的 model 字段
fn override_model_in_body(body_bytes: &[u8], model: &str) -> Option<bytes::Bytes> {
    let json = serde_json::from_slice::<serde_json::Value>(body_bytes).ok()?;
    let original_model = json.get("model").and_then(|m| m.as_str());

    if let Some(original) = original_model {
        tracing::info!("原始 model: {} -> 覆盖为: {}", original, model);
    }

    let mut modified = json;
    modified["model"] = serde_json::json!(model);

    serde_json::to_vec(&modified).ok().map(Into::into)
}

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

    // 修改请求体中的 model 字段（如果配置中有设置）
    // 这里使用第一个 upstream 的 model 作为默认
    let default_model = cfg.upstream.first().map_or("", |u| u.model.as_str());
    let body_bytes = if !default_model.is_empty() && !body_bytes.is_empty() {
        override_model_in_body(&body_bytes, default_model).unwrap_or(body_bytes)
    } else {
        body_bytes
    };

    if let Some(local_response) =
        try_local_optimization(&body_bytes, &cfg.optimizations, default_model)
    {
        tracing::info!("✅ 本地优化命中: {}", local_response.reason);

        res.status_code(StatusCode::OK);
        res.headers_mut().insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );

        if let Ok(value) = HeaderValue::from_str(local_response.reason) {
            res.headers_mut()
                .insert(HeaderName::from_static("x-cc-proxy-optimization"), value);
        }

        if let Ok(body_str) = std::str::from_utf8(&local_response.body) {
            log_full_response(body_str);
        }

        res.body(local_response.body);
        return;
    }

    // 记录请求体并计算 token
    if !body_bytes.is_empty()
        && let Ok(body_str) = std::str::from_utf8(&body_bytes)
    {
        log_full_body(body_str);
        calculate_tokens(stats, body_str);
    }

    // 使用双层轮询选择器：先选 upstream，再选该 upstream 的 api_key
    let (upstream_idx, endpoint, _model, api_key) =
        if let Some(selector) = config.get_upstream_selector() {
            if let Some((idx, endpoint, model, key)) = selector.next() {
                (idx, endpoint, model, key)
            } else {
                tracing::error!("No upstream configured");
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                return;
            }
        } else {
            tracing::error!("UpstreamSelector not initialized");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        };

    // 打印选择的 upstream 和 api_key（脱敏显示）
    tracing::info!(
        "🔄 选中的 Upstream[{}]: endpoint={}, api_key: {}***",
        upstream_idx,
        endpoint,
        api_key.chars().take(8).collect::<String>()
    );

    // 解析 endpoint
    let host_str = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(&endpoint);

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
    // let upstream_url = upstream_url.replace("messages", "responses");
    let upstream_url = upstream_url.replace("?beta=true", "");
    tracing::info!("Proxying to: {}", upstream_url);

    // 构建代理请求
    let mut proxy_req_builder = HyperRequest::builder()
        .method(req.method())
        .uri(&upstream_url);

    // 复制请求头（跳过 host、authorization 和 content-length，会重新计算）
    for (name, value) in req.headers() {
        let name_str = name.as_str();
        if name_str != "host" && name_str != "authorization" && name_str != "content-length" {
            proxy_req_builder = proxy_req_builder.header(name, value);
        }
    }

    // 注入 Authorization
    proxy_req_builder = proxy_req_builder.header("Authorization", format!("Bearer {api_key}"));
    proxy_req_builder = proxy_req_builder.header("host", host);

    // 设置正确的 Content-Length（基于修改后的 body 大小）
    proxy_req_builder = proxy_req_builder.header("content-length", body_bytes.len());

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
