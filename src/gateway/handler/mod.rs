mod content_tag;
mod request;
mod response;
mod system_prompt;
mod thinking_patch;
mod tool_desc;
mod utils;

use futures_util::Stream;
use futures_util::StreamExt;
use http_body_util::{BodyExt, BodyStream, Full};
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use salvo::{http::ResBody, prelude::*};
use std::pin::Pin;

use crate::{
    config::Mode,
    gateway::{
        handler::{
            request::{
                filter_req_body, get_req_body, log_request_meta, make_proxy_url,
                override_model_in_body, req_local_intercept,
            },
            response::decompress_gzip_if_needed,
            system_prompt::{CUSTOM_SYSTEM_PROMPT, insert_custom_system_prompt},
            thinking_patch::patch_reasoning_for_thinking_mode,
            utils::setup_handler_state,
        },
        openai_compat,
        service::{calculate_tokens, log_full_body, log_full_response},
    },
};

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

    let body_bytes = match get_req_body(req).await {
        Ok(v) => v,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("{e}");
            return;
        }
    };

    let cfg = config.get();
    // 优先检查本地优化（不需要选择 upstream/key）
    if req_local_intercept(req, res, &body_bytes, &cfg) {
        return;
    }

    // 记录请求头
    log_request_meta(
        req.method().as_str(),
        req.uri().to_string().as_str(),
        req.headers(),
    );

    // 注入自定义系统提示词
    let body_bytes = if body_bytes.is_empty() {
        body_bytes
    } else {
        insert_custom_system_prompt(&body_bytes, CUSTOM_SYSTEM_PROMPT).unwrap_or(body_bytes)
    };

    // 过滤不必要的提示词（必须在注入系统提示词之后执行）
    let body_bytes = match filter_req_body(&body_bytes).await {
        Ok(v) => v,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("{e}");
            return;
        }
    };

    // 本地优化未命中，选择 upstream 和 api_key
    let (upstream_idx, endpoint, selected_model, api_key, mode) =
        if let Some(selector) = config.get_upstream_selector() {
            if let Some((idx, endpoint, model, key, mode)) = selector.next() {
                (
                    idx,
                    endpoint.to_owned(),
                    model.to_owned(),
                    key.to_owned(),
                    mode,
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
        "🔄 选中的 Upstream[{}]: endpoint={}, model={}, api_key: {}***, mode={:?}",
        upstream_idx,
        endpoint,
        selected_model,
        api_key.chars().take(8).collect::<String>(),
        mode
    );

    // 使用选中 upstream 的 model 覆盖请求体中的 model 字段
    let body_bytes = if !selected_model.is_empty() && !body_bytes.is_empty() {
        override_model_in_body(&body_bytes, &selected_model).unwrap_or(body_bytes)
    } else {
        body_bytes
    };

    // 如果 oai_api 启用，转换请求体格式：Claude → OpenAI Responses
    let body_bytes = if matches!(mode, Mode::OpenAIResponses) && !body_bytes.is_empty() {
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
        if cfg.log_req_body {
            log_full_body(body_str);
        }

        calculate_tokens(stats.as_ref(), body_str);
    }

    let (upstream_url, host) = make_proxy_url(&endpoint, mode, req);

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
    proxy_req_builder = proxy_req_builder.header("host", host.as_ref());

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
                // SSE：流式透传 + 实时日志（仅在配置启用时）
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

                let log_body = cfg.log_res_body;

                // 根据 mode 决定是否需要转换流格式
                let stream = if matches!(mode, Mode::OpenAIResponses) {
                    // 使用流式转换器将 OpenAI Responses 格式转换为 Anthropic 格式
                    tracing::info!("🔄 使用 OpenAI Responses → Anthropic 流式转换器");
                    Box::pin(openai_compat::ResponsesStreamConverter::new(
                        BodyStream::new(body),
                        if selected_model.is_empty() {
                            None
                        } else {
                            Some(selected_model.clone())
                        },
                    ))
                        as Pin<
                            Box<
                                dyn Stream<Item = Result<bytes::Bytes, std::convert::Infallible>>
                                    + Send,
                            >,
                        >
                } else {
                    // 直接透传 Anthropic 格式的 SSE 流
                    tracing::info!("⏭️ 直接透传 Anthropic 格式 SSE 流");
                    Box::pin(
                        BodyStream::new(body)
                            .inspect(move |frame| {
                                if log_body
                                    && let Ok(f) = frame
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
                            .map(Ok::<bytes::Bytes, std::convert::Infallible>),
                    )
                        as Pin<
                            Box<
                                dyn Stream<Item = Result<bytes::Bytes, std::convert::Infallible>>
                                    + Send,
                            >,
                        >
                };

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
            if matches!(mode, Mode::OpenAIResponses) && !body_bytes.is_empty() && cfg.log_res_body {
                let raw_body_str = String::from_utf8_lossy(&body_bytes);
                tracing::info!("=== 原始上游响应 (转换前) ===");
                tracing::info!("{}", raw_body_str);
                tracing::info!("=== 原始上游响应结束 ===");
            }

            // 如果 oai_api 启用，转换响应体格式：OpenAI Responses → Claude
            let body_bytes = if matches!(mode, Mode::OpenAIResponses) && !body_bytes.is_empty() {
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
            if cfg.log_res_body {
                log_full_response(&body_str);
            }

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

/// Codex 代理 handler - 仅转发 mode = "`openai_responses`" 的上游请求
#[handler]
pub async fn codex_proxy(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let (config, _stats, client) = match setup_handler_state(depot) {
        Ok(v) => v,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("Failed to get dependencies from depot: {e}");
            return;
        }
    };

    let cfg = config.get();

    // 检查上游 mode 是否为 "openai_responses"
    let (upstream_idx, endpoint, selected_model, api_key, mode) =
        if let Some(selector) = config.get_upstream_selector() {
            if let Some((idx, endpoint, model, key, mode)) = selector.next() {
                (
                    idx,
                    endpoint.to_owned(),
                    model.to_owned(),
                    key.to_owned(),
                    mode,
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

    // 只处理 mode = "openai_responses" 的上游
    if !matches!(mode, crate::config::Mode::OpenAIResponses) {
        tracing::warn!(
            "Codex 代理拒绝: Upstream[{}] mode 不是 'openai_responses' (当前: {:?})",
            upstream_idx,
            mode
        );
        res.status_code(StatusCode::NOT_FOUND);
        res.render("Not Found");
        return;
    }

    // 获取请求体
    let body_bytes = match get_req_body(req).await {
        Ok(v) => v,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("{e}");
            return;
        }
    };

    // 记录请求头
    log_request_meta(
        req.method().as_str(),
        req.uri().to_string().as_str(),
        req.headers(),
    );

    // 打印选择的 upstream 和 api_key（脱敏显示）
    tracing::info!(
        "🔄 Codex 代理选中的 Upstream[{}]: endpoint={}, model={}, api_key: {}***, mode={:?}",
        upstream_idx,
        endpoint,
        selected_model,
        api_key.chars().take(8).collect::<String>(),
        mode
    );

    // 使用选中 upstream 的 model 覆盖请求体中的 model 字段
    let body_bytes = if !selected_model.is_empty() && !body_bytes.is_empty() {
        override_model_in_body(&body_bytes, &selected_model).unwrap_or(body_bytes)
    } else {
        body_bytes
    };

    // 记录请求体
    if !body_bytes.is_empty()
        && let Ok(body_str) = std::str::from_utf8(&body_bytes)
        && cfg.log_req_body
    {
        log_full_body(body_str);
    }

    let (upstream_url, host) = make_proxy_url(&endpoint, mode, req);

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
    proxy_req_builder = proxy_req_builder.header("host", host.as_ref());

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
                // SSE：流式透传 + 实时日志（仅在配置启用时）
                tracing::info!("=== Codex SSE 流式响应开始 ===");
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

                // Codex CLI 使用 OpenAI Responses 协议，SSE 直通上游数据
                tracing::info!("⏭️ Codex: 直接透传 OpenAI Responses SSE 流");
                let log_body = cfg.log_res_body;
                let stream = BodyStream::new(body)
                    .inspect(move |frame| {
                        if log_body
                            && let Ok(f) = frame
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
                                tracing::error!("Codex SSE 流读取错误: {}", e);
                                None
                            }
                        }
                    })
                    .map(Ok::<bytes::Bytes, std::convert::Infallible>);

                res.body(ResBody::stream(stream));
                tracing::info!("=== Codex SSE 流式响应结束 ===");
                return;
            }

            // 非 SSE：收集完整响应体后处理
            tracing::info!("=== Codex 非 SSE 响应开始 ===");
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
            if !body_bytes.is_empty() && cfg.log_res_body {
                let raw_body_str = String::from_utf8_lossy(&body_bytes);
                tracing::info!("=== Codex 原始上游响应 ===");
                tracing::info!("{}", raw_body_str);
                tracing::info!("=== Codex 原始上游响应结束 ===");
            } else {
                tracing::info!("=== Codex 非 SSE 响应: {} bytes ===", body_bytes.len());
            }

            // Codex 代理不进行格式转换，直接透传响应

            // 构建响应
            res.status_code(
                StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );
            for (name, value) in parts.headers {
                if let Some(name) = name {
                    let name_str = name.as_str();
                    // 跳过 content-length，让 Salvo/hyper 自动计算
                    // 跳过 content-encoding，因为我们已经解压了响应体
                    if name_str != "content-length" && name_str != "content-encoding" {
                        res.headers_mut().insert(name, value);
                    }
                }
            }
            res.body(body_bytes.to_vec());
        }
        Err(e) => {
            tracing::error!("Codex proxy request failed: {}", e);
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render("Bad Gateway");
        }
    }
}
