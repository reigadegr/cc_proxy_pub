mod service;

use crate::config::AtomicConfig;
use crate::gateway::service::{calculate_tokens, log_full_body, log_full_response};
use async_trait::async_trait;
use bytes::Bytes;
use pingora::prelude::*;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tracing::info;

// 每个请求的上下文，用来攒 body chunks
pub struct RequestContext {
    body_buffer: Vec<u8>,
}

impl Default for RequestContext {
    fn default() -> Self {
        Self {
            body_buffer: Vec::with_capacity(1024 * 1024),
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
    type CTX = RequestContext;

    fn new_ctx(&self) -> Self::CTX {
        RequestContext::default()
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
        req.insert_header("host", cfg.host.as_str())?;
        req.insert_header("Authorization", cfg.api_key.as_str())?;

        Ok(())
    }

    /// 收集 body chunks，只在最后一个 chunk 时统计
    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if let Some(b) = body {
            ctx.body_buffer.extend_from_slice(b);
        }

        // 只有收到最后一个 chunk 时才处理和统计
        if end_of_stream {
            let body_str = if let Ok(s) = std::str::from_utf8(&ctx.body_buffer) {
                s.to_string()
            } else {
                info!("请求体 (二进制, {} 字节)", ctx.body_buffer.len());
                return Ok(());
            };

            log_full_body(&body_str);
            calculate_tokens(&self, &body_str);
        }

        Ok(())
    }

    /// 收集响应体 chunks 并打印
    fn response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        _ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>>
    where
        Self::CTX: Send + Sync,
    {
        // 响应体需要单独的 buffer，因为没有请求生命周期那么长
        // 这里用 thread_local 或每次创建新 buffer
        thread_local! {
            static RESPONSE_BUFFER: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::with_capacity(1024 * 1024));
        }

        if let Some(b) = body {
            RESPONSE_BUFFER.with_borrow_mut(|buf| buf.extend_from_slice(b.as_ref()));
        }

        if end_of_stream {
            RESPONSE_BUFFER.with_borrow(|buf| {
                if let Ok(body_str) = std::str::from_utf8(buf) {
                    log_full_response(body_str);
                } else {
                    info!("响应体 (二进制, {} 字节)", buf.len());
                }
            });
            // 清空 buffer 为下次请求准备
            RESPONSE_BUFFER.with_borrow_mut(std::vec::Vec::clear);
        }

        Ok(None)
    }
}
