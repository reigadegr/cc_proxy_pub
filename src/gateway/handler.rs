use super::{
    HttpClient, RequestStats, openai_compat,
    optimization::try_local_optimization,
    service::{calculate_tokens, log_full_body, log_full_response, log_request_headers},
};
use crate::config::AtomicConfig;
use bytes::Bytes;
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use http_body_util::{BodyExt, BodyStream, Full};
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use salvo::{http::ResBody, prelude::*};
use std::{io::Read, sync::Arc};

/// 需要从 system 数组中移除的文本特征（多个标记，匹配任意一个即过滤）
const SYSTEM_PROMPT_FILTER_MARKERS: &[&str] = &[
    // Claude CLI 的主要提示词
    "You are an interactive CLI tool that helps users with soft",
    // Claude Code 身份标识
    "You are Claude Code",
    // Claude Code 查找文件标识
    "You are a file search specialist for Claude Code",
    // Claude Code 无意义版本信息
    "x-anthropic-billing-header: cc_version=",
];

/// 需要从 messages[].content[] 中移除的标签（成对匹配）
const CONTENT_TAG_FILTERS: &[(&str, &str)] = &[
    ("<system-reminder>", "</system-reminder>"),
    ("<local-command-stdout>", "</local-command-stdout>"),
    ("<command-name>", "</command-name>"),
    ("<local-command-caveat>", "</local-command-caveat>"),
];

/// 缺省的 `reasoning_content` 占位符
const REASONING_PLACEHOLDER: &str = "[Previous reasoning not available in context]";

/// 检查文本是否应该从 content 中移除
fn should_remove_content(text: &str) -> bool {
    let trimmed = text.trim();
    for (start, end) in CONTENT_TAG_FILTERS {
        if trimmed.starts_with(start) && trimmed.ends_with(end) {
            return true;
        }
    }
    false
}

/// 从 message.content 中提取 type=thinking 的 thinking 文本
fn extract_thinking_text(message: &serde_json::Value) -> Option<&str> {
    message
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|content| {
            content
                .iter()
                .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("thinking"))
        })
        .and_then(|block| block.get("thinking").and_then(|t| t.as_str()))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// 尝试解压 gzip 编码的响应体
///
/// 检查 content-encoding 头部，如果是 gzip 则自动解压。
/// 返回解压后的字节和是否进行了解压的标志。
fn decompress_gzip_if_needed(body_bytes: &Bytes, content_encoding: Option<&str>) -> Bytes {
    // 检查是否为 gzip 编码
    let is_gzip = content_encoding.is_some_and(|enc| enc.to_lowercase().contains("gzip"));

    if !is_gzip {
        return body_bytes.clone();
    }

    // 尝试解压 gzip 数据
    let mut decoder = GzDecoder::new(&body_bytes[..]);
    let mut decompressed = Vec::new();
    match decoder.read_to_end(&mut decompressed) {
        Ok(_) => {
            tracing::debug!(
                "📦 gzip 解压成功: {} bytes → {} bytes",
                body_bytes.len(),
                decompressed.len()
            );
            decompressed.into()
        }
        Err(e) => {
            tracing::warn!("gzip 解压失败: {}，使用原始响应体", e);
            body_bytes.clone()
        }
    }
}

/// 过滤请求体中的 system 数组，移除包含特定文本的元素
///
/// Claude CLI 发送的请求中，system 数组包含很长的提示词文本，
/// 这些文本会占用大量 tokens。此函数移除包含任意标记文本的元素。
fn filter_system_prompts(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = serde_json::from_slice::<serde_json::Value>(body_bytes).ok()?;

    // 获取 system 数组
    let system = json.get_mut("system")?.as_array_mut()?;

    let original_len = system.len();

    // 过滤掉包含任意标记文本的元素
    system.retain(|item| {
        item.get("text")
            .and_then(|t| t.as_str())
            .is_none_or(|text| {
                !SYSTEM_PROMPT_FILTER_MARKERS
                    .iter()
                    .any(|marker| text.contains(marker))
            })
    });

    // 如果有元素被移除，记录日志
    if system.len() < original_len {
        tracing::info!(
            "🧹 已过滤 system 数组: {} 个元素 → {} 个元素 (移除了 {} 个)",
            original_len,
            system.len(),
            original_len - system.len()
        );
    }

    serde_json::to_vec(&json).ok().map(Into::into)
}

/// 为 Kimi Thinking 模式补全缺失的 `reasoning_content`
///
/// 在 thinking 启用时：
/// - 优先从 message.content[type=thinking].thinking 提取文本
/// - 给 `assistant` 消息补上/替换 `reasoning_content`（缺失或为占位符时）
/// - 给 `messages` 最后一个元素补上/替换 `reasoning_content`（缺失或为占位符时），不区分 role
fn patch_reasoning_for_thinking_mode(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = serde_json::from_slice::<serde_json::Value>(body_bytes).ok()?;

    // 检查是否启用了 thinking 模式
    let thinking_enabled = json
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|t| t.as_str())
        == Some("enabled");

    if !thinking_enabled {
        return None;
    }

    let messages = json.get_mut("messages")?.as_array_mut()?;
    let mut patched = false;

    // 用于兜底：取最后一个可用的 thinking 文本
    let latest_thinking = messages
        .iter()
        .rev()
        .find_map(extract_thinking_text)
        .map(str::to_string);

    for message in messages.iter_mut() {
        if message.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }

        let should_patch = message
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .is_none_or(|value| value == REASONING_PLACEHOLDER);

        if !should_patch {
            continue;
        }

        let reasoning_value = extract_thinking_text(message)
            .or(latest_thinking.as_deref())
            .unwrap_or(REASONING_PLACEHOLDER)
            .to_string();

        let Some(object) = message.as_object_mut() else {
            continue;
        };

        let should_update = object
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .is_none_or(|current| current != reasoning_value);

        if should_update {
            object.insert(
                "reasoning_content".to_string(),
                serde_json::json!(reasoning_value),
            );
            patched = true;
        }
    }

    if let Some(last_message) = messages.last_mut() {
        let should_patch = last_message
            .get("reasoning_content")
            .and_then(|v| v.as_str())
            .is_none_or(|value| value == REASONING_PLACEHOLDER);

        if should_patch {
            let reasoning_value = extract_thinking_text(last_message)
                .or(latest_thinking.as_deref())
                .unwrap_or(REASONING_PLACEHOLDER)
                .to_string();

            if let Some(object) = last_message.as_object_mut() {
                let should_update = object
                    .get("reasoning_content")
                    .and_then(|v| v.as_str())
                    .is_none_or(|current| current != reasoning_value);

                if should_update {
                    object.insert(
                        "reasoning_content".to_string(),
                        serde_json::json!(reasoning_value),
                    );
                    patched = true;
                }
            }
        }
    }

    if patched {
        tracing::debug!("Patched missing reasoning_content for thinking mode messages");
        serde_json::to_vec(&json).ok().map(Into::into)
    } else {
        None
    }
}

/// 过滤 messages[].content[] 数组，移除无用标签内容
///
/// Claude CLI 发送的请求中，content 数组可能包含大量无用的标签内容：
/// - <system-reminder>...</system-reminder>
/// - <local-command-stdout>...</local-command-stdout>
/// - <command-name>...</command-name>
/// - <local-command-caveat>...</local-command-caveat>
///
/// 这些内容占用大量 tokens 但对模型无用，此函数将其移除。
fn filter_messages_content(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = serde_json::from_slice::<serde_json::Value>(body_bytes).ok()?;

    let messages = json.get_mut("messages")?.as_array_mut()?;

    let mut total_removed = 0usize;
    let mut total_chars = 0usize;

    for message in messages.iter_mut() {
        let Some(content) = message.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };

        // 统计移除前的信息
        for item in content.iter() {
            if let Some(text) = item.get("text").and_then(|t| t.as_str())
                && should_remove_content(text)
            {
                total_removed += 1;
                total_chars += text.len();
            }
        }

        // 过滤掉需要移除的内容
        content.retain(|item| {
            item.get("text")
                .and_then(|t| t.as_str())
                .is_none_or(|text| !should_remove_content(text))
        });
    }

    if total_removed > 0 {
        tracing::info!(
            "🧹 已过滤 messages.content: 移除 {} 项, 节省约 {} 字符 (~{} tokens)",
            total_removed,
            total_chars,
            total_chars / 4
        );
    }

    serde_json::to_vec(&json).ok().map(Into::into)
}

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
    let mut body_bytes = match BodyExt::collect(req.body_mut()).await {
        Ok(body) => body.to_bytes(),
        Err(e) => {
            tracing::error!("Failed to collect request body: {}", e);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
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

    // 优先检查本地优化（不需要选择 upstream/key）
    if let Some(local_response) = try_local_optimization(
        &body_bytes,
        req.uri().to_string().as_str(),
        &cfg.optimizations,
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
        return;
    }

    // 本地优化未命中，选择 upstream 和 api_key
    let (upstream_idx, endpoint, selected_model, api_key, oai_api) =
        if let Some(selector) = config.get_upstream_selector() {
            if let Some((idx, endpoint, model, key, oai_api)) = selector.next() {
                (
                    idx,
                    endpoint.to_owned(),
                    model.to_owned(),
                    key.to_owned(),
                    oai_api,
                )
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
        "🔄 选中的 Upstream[{}]: endpoint={}, model={}, api_key: {}***, oai_api={}",
        upstream_idx,
        endpoint,
        selected_model,
        api_key.chars().take(8).collect::<String>(),
        oai_api
    );

    // 使用选中 upstream 的 model 覆盖请求体中的 model 字段
    let body_bytes = if !selected_model.is_empty() && !body_bytes.is_empty() {
        override_model_in_body(&body_bytes, &selected_model).unwrap_or(body_bytes)
    } else {
        body_bytes
    };

    // 如果 oai_api 启用，转换请求体格式：Claude → OpenAI Responses
    let body_bytes = if oai_api && !body_bytes.is_empty() {
        match openai_compat::anthropic_request_to_responses(&body_bytes) {
            Ok(converted) => {
                tracing::debug!(
                    "🔄 请求体格式转换: Claude → OpenAI Responses ({} bytes → {} bytes)",
                    body_bytes.len(),
                    converted.len()
                );
                converted
            }
            Err(e) => {
                tracing::warn!("请求体格式转换失败: {}，使用原始请求体", e);
                body_bytes
            }
        }
    } else {
        // 直接转发 Anthropic 格式时，为 Kimi 等支持 Thinking 的模型补全 reasoning_content
        if body_bytes.is_empty() {
            body_bytes
        } else if let Some(patched) = patch_reasoning_for_thinking_mode(&body_bytes) {
            tracing::debug!("🩹 修补 thinking 模式缺失的 reasoning_content");
            patched
        } else {
            body_bytes
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

    // Content-Length 由 hyper 自动设置，无需手动设置

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

            // 在 collect() 之前判断是否为 SSE，避免将整个流缓冲到内存
            let is_sse = parts
                .headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.contains("text/event-stream"));

            if is_sse {
                // SSE：流式透传 + 实时日志
                tracing::info!("=== SSE 流式响应开始 ===");
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
                let stream = BodyStream::new(body)
                    .inspect(|frame| {
                        if let Ok(f) = frame
                            && let Some(data) = f.data_ref()
                            && let Ok(s) = std::str::from_utf8(data)
                        {
                            tracing::info!("{}", s);
                        }
                    })
                    .filter_map(|frame| async move {
                        match frame {
                            Ok(f) => f.into_data().ok(),
                            Err(e) => {
                                tracing::error!("SSE 流读取错误: {}", e);
                                None
                            }
                        }
                    })
                    .map(Ok::<bytes::Bytes, std::convert::Infallible>);
                res.body(ResBody::stream(stream));
                return;
            }

            // 非 SSE：收集完整响应体后处理
            let body_bytes = match BodyExt::collect(body).await {
                Ok(b) => b.to_bytes(),
                Err(e) => {
                    tracing::error!("Failed to collect response body: {}", e);
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                    return;
                }
            };

            // 检查并解压 gzip 编码的响应体
            let content_encoding = parts
                .headers
                .get("content-encoding")
                .and_then(|v| v.to_str().ok());
            let body_bytes = decompress_gzip_if_needed(&body_bytes, content_encoding);

            // 记录原始上游响应（用于调试）
            if oai_api && !body_bytes.is_empty() {
                let raw_body_str = String::from_utf8_lossy(&body_bytes);
                tracing::info!("=== 原始上游响应 (转换前) ===");
                tracing::info!("{}", raw_body_str);
                tracing::info!("=== 原始上游响应结束 ===");
            }

            // 如果 oai_api 启用，转换响应体格式：OpenAI Responses → Claude
            let body_bytes = if oai_api && !body_bytes.is_empty() {
                match openai_compat::responses_response_to_anthropic(
                    &body_bytes,
                    if selected_model.is_empty() {
                        None
                    } else {
                        Some(&selected_model)
                    },
                ) {
                    Ok(converted) => {
                        tracing::debug!(
                            "🔄 响应体格式转换: OpenAI Responses → Claude ({} bytes → {} bytes)",
                            body_bytes.len(),
                            converted.len()
                        );
                        converted
                    }
                    Err(e) => {
                        tracing::warn!("响应体格式转换失败: {}，使用原始响应体", e);
                        body_bytes
                    }
                }
            } else {
                body_bytes
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
                    let name_str = name.as_str();
                    // 跳过 content-length，让 Salvo/hyper 自动计算
                    // 因为响应体可能经过格式转换，大小会改变
                    // 跳过 content-encoding，因为我们已经解压了响应体
                    if name_str != "content-length" && name_str != "content-encoding" {
                        res.headers_mut().insert(name, value);
                    }
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
