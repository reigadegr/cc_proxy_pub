mod service;

use crate::gateway::service::{calculate_tokens, log_full_body};
use async_trait::async_trait;
use bytes::Bytes;
use pingora::prelude::*;
use std::sync::atomic::AtomicU64;
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
    total_tokens: AtomicU64,
    user_new_tokens: AtomicU64,
    user_history_tokens: AtomicU64,
    assistant_tokens: AtomicU64,
    system_tokens: AtomicU64,
    request_count: AtomicU64,
}

impl Default for Gateway {
    fn default() -> Self {
        Self {
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
        let peer = Box::new(HttpPeer::new(
            ("open.bigmodel.cn", 443),
            true,
            "open.bigmodel.cn".to_string(),
        ));
        Ok(peer)
    }

    async fn upstream_request_filter(
        &self,
        _session: &mut Session,
        req: &mut pingora::http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        req.insert_header("host", "open.bigmodel.cn")?;
        req.insert_header(
            "Authorization",
            "35d84af820c343659f5abe82389bea60.f9PeMuu8jgqo1Z4M",
        )?;

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
}
