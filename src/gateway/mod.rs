mod service;

use crate::{
    config::AtomicConfig,
    gateway::service::{calculate_tokens, log_full_body, log_full_response, log_request_headers},
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
        let endpoint = &cfg.endpoint;
        let host_str = endpoint.replace("https://", "");
        let host_str = host_str.replace("http://", "");
        let host = host_str
            .split_once('/')
            .map_or(host_str.as_str(), |(h, _)| h);
        let peer = Box::new(HttpPeer::new((host, 443u16), true, host.to_string()));
        Ok(peer)
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        req: &mut pingora::http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        // 1. 只获取一次配置
        let cfg = self.config.get();
        let endpoint = &cfg.endpoint;

        // 2. 只处理一次 endpoint，同时提取 host 和 base_path
        let host_str = endpoint.strip_prefix("https://").unwrap_or(endpoint);
        let (host, base_path) = host_str
            .split_once('/')
            .map_or((host_str, "/"), |(h, p)| (h, p));

        // 3. 打印全部请求头
        log_request_headers(req);

        // 路径重写
        let original_uri = req
            .uri
            .path_and_query()
            .map_or(String::new(), |p| p.as_str().to_string());

        let mut new_path = format!("/{base_path}/{original_uri}");

        // 移除所有连续斜杠
        while new_path.contains("//") {
            new_path = new_path.replace("//", "/");
        }
        match new_path.parse::<http::Uri>() {
            Ok(uri) => {
                req.set_uri(uri);
                info!("路径重写: {} -> {}", original_uri, new_path);
            }
            Err(e) => info!("路径未重写: {}", e),
        }
        // 合并请求头设置

        let auth = format!("Bearer {}", cfg.api_key.as_str());
        req.insert_header("Authorization", auth)?;
        req.insert_header("host", host)?;

        info!("最终 URI: {}", req.uri);

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
