mod content_tag;
mod request;
mod system_prompt;
mod thinking_patch;
mod tool_desc;
mod utils;

use std::{io::Read, sync::Arc};

use bytes::Bytes;
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use http_body_util::{BodyExt, BodyStream, Full};
use hyper::{
    Request as HyperRequest, Response as HyperResponse,
    body::Incoming,
    header::{HeaderName, HeaderValue},
};
use salvo::{http::ResBody, prelude::*};

use crate::{
    AtomicConfig,
    gateway::{
        handler::{
            content_tag::filter_messages_content,
            request::{filter_req_body, override_model_in_body},
            system_prompt::filter_system_prompts,
            thinking_patch::patch_reasoning_for_thinking_mode,
            tool_desc::filter_tools_by_description,
            utils::setup_handler_state,
        },
        openai_compat,
        optimization::try_local_optimization,
        service::{calculate_tokens, log_full_body, log_full_response, log_request_info},
    },
};

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

/// 代理请求 handler
#[handler]
pub async fn claude_proxy(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let (config, stats, client) = match setup_handler_state(depot) {
        Ok(v) => v,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("Failed to get dependencies from depot: {e}");
            return;
        }
    };

    // 记录请求头
    log_request_info(
        req.method().as_str(),
        req.uri().to_string().as_str(),
        req.headers(),
    );

    let body_bytes = match filter_req_body(req).await {
        Ok(v) => v,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("{e}");
            return;
        }
    };

    // 优先检查本地优化（不需要选择 upstream/key）
    if req_local_intercept(req, res, &body_bytes, config) {
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
        calculate_tokens(stats.as_ref(), body_str);
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
