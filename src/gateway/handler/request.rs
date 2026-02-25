use std::{borrow::Cow, sync::Arc};

use anyhow::{Result, bail};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::header::{HeaderName, HeaderValue};
use salvo::prelude::*;
use serde_json::{Value, from_slice, json, to_vec};

use crate::{
    AtomicConfig,
    gateway::{
        handler::{
            content_tag::filter_messages_content, system_prompt::filter_system_prompts,
            tool_desc::filter_tools_by_description,
        },
        optimization::try_local_optimization,
        service::log_full_response,
    },
};

/// 尝试覆盖请求体中的 model 字段
pub fn override_model_in_body(body_bytes: &[u8], model: &str) -> Option<Bytes> {
    let json = from_slice::<Value>(body_bytes).ok()?;
    let original_model = json.get("model").and_then(|m| m.as_str());

    if let Some(original) = original_model {
        tracing::info!("原始 model: {} -> 覆盖为: {}", original, model);
    }

    let mut modified = json;
    modified["model"] = json!(model);

    to_vec(&modified).ok().map(Into::into)
}

pub async fn filter_req_body(req: &mut Request) -> Result<Bytes> {
    // 收集请求体
    let mut body_bytes = match BodyExt::collect(req.body_mut()).await {
        Ok(body) => body.to_bytes(),
        Err(e) => {
            bail!("Failed to collect request body: {e}");
        }
    };

    // 过滤 system 数组中占用大量 tokens 的提示词
    if !body_bytes.is_empty()
        && let Some(filtered) = filter_system_prompts(&body_bytes)
    {
        body_bytes = filtered;
    }

    // 过滤 messages.content 中占用大量 tokens 的无用标签
    if !body_bytes.is_empty()
        && let Some(filtered) = filter_messages_content(&body_bytes)
    {
        body_bytes = filtered;
    }

    // 过滤 tools.description 命中关键词的工具定义
    if !body_bytes.is_empty()
        && let Some(filtered) = filter_tools_by_description(&body_bytes)
    {
        body_bytes = filtered;
    }
    Ok(body_bytes)
}

pub fn req_local_intercept(
    req: &Request,
    res: &mut Response,
    body_bytes: &Bytes,
    config: &Arc<AtomicConfig>,
) -> bool {
    if let Some(local_response) = try_local_optimization(
        body_bytes,
        req.uri().to_string().as_str(),
        &config.get().optimizations,
    ) {
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
        return true;
    }
    false
}

pub fn make_proxy_url<'a>(
    endpoint: &'a str,
    oai_api: bool,
    req: &Request,
) -> (String, Cow<'a, str>) {
    // 解析 endpoint
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

    let mut upstream_url = format!("{host}{new_path}");
    upstream_url = upstream_url.replace("?beta=true", "");

    // 只有当 oai_api=true 时才将 messages 替换为 responses
    if oai_api {
        upstream_url = upstream_url.replace("messages", "responses");
    }
    upstream_url = upstream_url.replace("claude/", "");
    while upstream_url.contains("//") {
        upstream_url = upstream_url.replace("//", "/");
    }
    upstream_url = format!("{scheme}://{upstream_url}");
    tracing::info!("Proxying to: {}", upstream_url);
    (upstream_url, Cow::Borrowed(host))
}
