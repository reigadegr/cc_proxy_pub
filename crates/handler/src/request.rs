use anyhow::{Result, bail};
use bytes::Bytes;
use http::HeaderMap;
use http_body_util::BodyExt;
use hyper::header::{HeaderName, HeaderValue};
use my_config::Config;
use my_optimization::{
    OptimizationResponse, try_local_optimization_from_json, try_local_url_optimization,
};
use salvo::prelude::*;
use serde_json::{Value, from_slice, json, to_vec};
use tracing::info;

use crate::response::log_full_response;

pub async fn get_req_body(req: &mut Request) -> Result<Bytes> {
    // 收集请求体
    let body_bytes = match BodyExt::collect(req.body_mut()).await {
        Ok(body) => body.to_bytes(),
        Err(e) => {
            bail!("Failed to collect request body: {e}");
        }
    };
    Ok(body_bytes)
}

pub fn parse_body_json(body_bytes: &[u8]) -> Result<Value> {
    from_slice::<Value>(body_bytes).map_err(Into::into)
}

pub fn serialize_body_json(json: &Value) -> Result<Bytes> {
    to_vec(json).map(Into::into).map_err(Into::into)
}

/// 尝试覆盖请求体中的 model 字段
pub fn override_model_in_json(json: &mut Value, model: &str) {
    let original_model = json.get("model").and_then(|m| m.as_str());

    if let Some(original) = original_model {
        info!("原始 model: {} -> 覆盖为: {}", original, model);
    }

    json["model"] = json!(model);
}

const BILLING_HEADER_MARKER: &str = "x-anthropic-billing-header: cc_version";

/// 移除 system 数组中 text 包含 x-anthropic-billing-header 的条目
pub fn strip_billing_header_from_system(json: &mut Value) {
    let Some(system) = json.get_mut("system").and_then(|s| s.as_array_mut()) else {
        return;
    };
    system.retain(|entry| {
        entry
            .get("text")
            .and_then(|t| t.as_str())
            .is_none_or(|text| !text.contains(BILLING_HEADER_MARKER))
    });
}

pub fn override_model_in_body(body_bytes: &[u8], model: &str) -> Option<Bytes> {
    let mut modified = from_slice::<Value>(body_bytes).ok()?;
    override_model_in_json(&mut modified, model);
    // modified["stream"] = json!(false); // 注释掉以保留原始请求的 stream 值

    to_vec(&modified).ok().map(Into::into)
}

fn write_local_optimization_response(
    res: &mut Response,
    local_response: OptimizationResponse,
    config: &Config,
) {
    info!("✅ 本地优化命中: {}", local_response.reason);

    res.status_code(StatusCode::OK);
    res.headers_mut().insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("application/json"),
    );

    if let Ok(value) = HeaderValue::from_str(local_response.reason) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-cc-proxy-optimization"), value);
    }

    if let Ok(body_str) = std::str::from_utf8(&local_response.body)
        && config.server.log_res_body
    {
        log_full_response(body_str);
    }

    res.body(local_response.body);
}

pub fn req_local_intercept_by_url(res: &mut Response, request_url: &str, config: &Config) -> bool {
    let Some(local_response) = try_local_url_optimization(request_url, &config.optimizations)
    else {
        return false;
    };

    write_local_optimization_response(res, local_response, config);
    true
}

pub fn req_local_intercept_from_json(
    res: &mut Response,
    request_json: &Value,
    request_url: &str,
    config: &Config,
) -> bool {
    let Some(local_response) =
        try_local_optimization_from_json(request_json, request_url, &config.optimizations)
    else {
        return false;
    };

    write_local_optimization_response(res, local_response, config);
    true
}

pub fn make_proxy_url<'a>(base_url: &'a str, req: &Request) -> (String, &'a str) {
    // 解析 base_url
    let host_str = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .unwrap_or(base_url);

    let (host, base_path) = host_str.split_once('/').unwrap_or((host_str, ""));

    // 构建上游 URL
    let original_path = req.uri().path();
    let query = req.uri().query().unwrap_or("");
    let query_str = if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    };

    let mut new_path = if base_path.is_empty() {
        format!("{original_path}{query_str}")
    } else {
        format!(
            "/{}/{}{}",
            base_path,
            original_path.trim_start_matches('/'),
            query_str
        )
    };

    // 消除重复的 /v1/v1 前缀（base_url 和请求路径都带 /v1 导致）
    while new_path.contains("/v1/v1/") || new_path.ends_with("/v1/v1") {
        new_path = new_path.replacen("/v1/v1", "/v1", 1);
    }

    let scheme = if base_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };

    let mut upstream_url = format!("{host}{new_path}");
    upstream_url = upstream_url.replace("?beta=true", "");

    while upstream_url.contains("//") {
        upstream_url = upstream_url.replace("//", "/");
    }
    upstream_url = format!("{scheme}://{upstream_url}");
    info!("Proxying to: {}", upstream_url);
    (upstream_url, host)
}

/// 打印全部请求头
pub fn log_request_meta(method: &str, uri: &str, headers: &HeaderMap) {
    info!("=== 请求头 ===");
    info!("Method: {}", method);
    info!("URI: {}", uri);

    for (name, value) in headers {
        if let Ok(value_str) = value.to_str() {
            info!("{}: {}", name, value_str);
        }
    }
    info!("=== 请求头结束 ===");
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::uri::Scheme;
    use hyper::Request as HyperRequest;
    use salvo::Request;

    use super::make_proxy_url;

    fn request_from_uri(uri: &str) -> Request {
        let request_result = HyperRequest::builder().uri(uri).body(Bytes::new());
        let Ok(request) = request_result else {
            panic!("failed to build request");
        };
        Request::from_hyper(request, Scheme::HTTP)
    }

    #[test]
    fn make_proxy_url_preserves_anthropic_root_path() {
        let req = request_from_uri("http://localhost/v1/messages");
        let (url, host) = make_proxy_url("https://upstream.example.com", &req);

        assert_eq!(url, "https://upstream.example.com/v1/messages");
        assert_eq!(host, "upstream.example.com");
    }

    #[test]
    fn make_proxy_url_preserves_anthropic_subpath_and_query() {
        let req = request_from_uri("http://localhost/v1/messages/count_tokens?foo=bar");
        let (url, _) = make_proxy_url("https://upstream.example.com", &req);

        assert_eq!(
            url,
            "https://upstream.example.com/v1/messages/count_tokens?foo=bar"
        );
    }

    #[test]
    fn make_proxy_url_joins_base_path_prefixes() {
        let req = request_from_uri("http://localhost/v1/messages?foo=bar");
        let (url, host) = make_proxy_url("https://upstream.example.com/prefix/api", &req);

        assert_eq!(
            url,
            "https://upstream.example.com/prefix/api/v1/messages?foo=bar"
        );
        assert_eq!(host, "upstream.example.com");
    }

    #[test]
    fn make_proxy_url_dedup_v1_in_base_url() {
        // base_url 带 /v1，请求路径也带 /v1，应去重为单个 /v1
        let req = request_from_uri("http://localhost/v1/messages");
        let (url, _) = make_proxy_url("https://upstream.example.com/v1", &req);

        assert_eq!(url, "https://upstream.example.com/v1/messages");
    }

    #[test]
    fn make_proxy_url_dedup_triple_v1() {
        // 极端情况：三重 /v1 应逐步去重为单个
        let req = request_from_uri("http://localhost/v1/messages");
        let (url, _) = make_proxy_url("https://upstream.example.com/v1/v1", &req);

        assert_eq!(url, "https://upstream.example.com/v1/messages");
    }
}
