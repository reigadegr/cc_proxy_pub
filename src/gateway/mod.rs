mod service;

use crate::{
    config::AtomicConfig,
    gateway::service::{calculate_tokens, log_full_body, log_full_response},
};
use async_trait::async_trait;
use bytes::Bytes;
use pingora::prelude::*;
use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};
use tracing::info;

// 每个请求的上下文，用来攒 body chunks
pub struct BodyBuffers {
    request_buffer: Vec<u8>,
    response_buffer: Vec<u8>,
}

impl Default for BodyBuffers {
    fn default() -> Self {
        Self {
            request_buffer: Vec::with_capacity(1024 * 1024),
            response_buffer: Vec::with_capacity(1024 * 1024),
        }
    }
}

pub struct Gateway {
    config: Arc<AtomicConfig>,
    total_tokens: AtomicU64,
    user_new_tokens: AtomicU64,
    user_history_tokens: AtomicU64,
    assistant_tokens: AtomicU64,
    system_tokens: AtomicU64,
    request_count: AtomicU64,
}

impl Gateway {
    pub const fn new(config: Arc<AtomicConfig>) -> Self {
        Self {
            config,
            total_tokens: AtomicU64::new(0),
            user_new_tokens: AtomicU64::new(0),
            user_history_tokens: AtomicU64::new(0),
            assistant_tokens: AtomicU64::new(0),
            system_tokens: AtomicU64::new(0),
            request_count: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl ProxyHttp for Gateway {
    type CTX = BodyBuffers;

    fn new_ctx(&self) -> Self::CTX {
        BodyBuffers::default()
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let cfg = self.config.get();
        let host = cfg.host.as_str();
        let peer = Box::new(HttpPeer::new((host, 443), true, host.to_string()));
        Ok(peer)
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        req: &mut pingora::http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        let cfg = self.config.get();

        // 打印原始请求路径
        info!("🔍 原始请求路径: {}", req.uri);

        // 打印请求头
        info!("🔍 请求方法: {}", req.method);
        info!("🔍 请求完整 URI: {:?}", req.uri);
        info!("🔍 URI path: {}", req.uri.path());
        if let Some(query) = req.uri.query() {
            info!("🔍 URI query: {}", query);
        }

        // 重写请求路径为配置中的 path
        let uri: http::Uri = cfg.path.parse().unwrap_or_else(|_| {
            tracing::warn!("Invalid path URI: {}", cfg.path);
            http::Uri::default()
        });
        info!("🔍 配置中的目标 path: {}", cfg.path);
        info!("🔍 解析后的 URI: {}", uri);
        let original_uri = &req.uri;
        let original_path = original_uri
            .path_and_query()
            .map_or("", http::uri::PathAndQuery::as_str);

        // 拼接新路径：配置的 path + 原始路径
        // 例如: /api/anthropic + /v1/messages = /api/anthropic/v1/messages
        let mut new_path = format!("{}/{}", cfg.path.as_str(), original_path);

        // 移除所有连续斜杠
        while new_path.contains("//") {
            new_path = new_path.replace("//", "/");
        }

        info!("路径重写: {} -> {}", original_path, new_path);

        // 设置新的 URI
        req.set_uri(new_path.parse().unwrap_or_else(|_| original_uri.clone()));

        req.insert_header("host", cfg.host.as_str())?;
        req.insert_header("Authorization", cfg.api_key.as_str())?;

        // 打印修改后的 URI
        info!("🔍 修改后 URI: {}", req.uri);

        Ok(())
    }

    /// 收集请求 body chunks
    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if let Some(b) = body {
            ctx.request_buffer.extend_from_slice(b);
        }

        if end_of_stream {
            let body_str = if let Ok(s) = std::str::from_utf8(&ctx.request_buffer) {
                s.to_string()
            } else {
                info!("请求体 (二进制, {} 字节)", ctx.request_buffer.len());
                return Ok(());
            };

            log_full_body(&body_str);
            calculate_tokens(self, &body_str);
        }

        Ok(())
    }

    /// 收集响应体 chunks 并打印 - 修复：使用 ctx 替代 `thread_local`
    fn response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>>
    where
        Self::CTX: Send + Sync,
    {
        if let Some(b) = body {
            ctx.response_buffer.extend_from_slice(b.as_ref());
        }

        if end_of_stream {
            if let Ok(body_str) = std::str::from_utf8(&ctx.response_buffer) {
                log_full_response(body_str);
            } else {
                info!("响应体 (二进制, {} 字节)", ctx.response_buffer.len());
            }
        }
        Ok(None)
    }
}
