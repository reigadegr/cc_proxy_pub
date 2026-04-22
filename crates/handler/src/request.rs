use std::borrow::Cow;

use anyhow::{Result, bail};
use bytes::Bytes;
use http::HeaderMap;
use http_body_util::BodyExt;
use hyper::header::{HeaderName, HeaderValue};
use my_config::Config;
use salvo::prelude::*;
use serde_json::{Value, from_slice, json, to_vec};
use tracing::info;

use crate::{
    content_filter::filter_content_strings_in_json,
    content_tag::{filter_messages_content_in_json, override_permission_error_in_json},
    system_prompt::filter_system_prompts_in_json,
    tool_desc::prune_tools_by_description_in_json,
};

use my_optimization::{
    OptimizationResponse, try_local_optimization_from_json, try_local_url_optimization,
};

/// 辅助函数：分段打印响应体
fn log_full_response(body: &str) {
    let len = body.len();
    info!("=== 响应体 (共 {} 字节) ===", len);
    info!("{}", body);
    info!("=== 响应体结束 ===");
}

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

pub fn filter_req_body(json: &mut Value) {
    filter_system_prompts_in_json(json);
    filter_messages_content_in_json(json);
    override_permission_error_in_json(json);
    filter_content_strings_in_json(json);
    prune_tools_by_description_in_json(json);
}

/// 尝试覆盖请求体中的 model 字段
pub fn override_model_in_json(json: &mut Value, model: &str) {
    let original_model = json.get("model").and_then(|m| m.as_str());

    if let Some(original) = original_model {
        info!("原始 model: {} -> 覆盖为: {}", original, model);
    }

    json["model"] = json!(model);
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

pub fn make_proxy_url<'a>(base_url: &'a str, req: &Request) -> (String, Cow<'a, str>) {
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
    (upstream_url, Cow::Borrowed(host))
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
}
