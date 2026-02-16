#![warn(
    clippy::nursery,
    clippy::pedantic,
    clippy::style,
    clippy::complexity,
    clippy::perf,
    clippy::correctness,
    clippy::suspicious
)]
use async_trait::async_trait;
use bytes::Bytes;
use chrono::Local;
use pingora::{http::ResponseHeader, prelude::*};
use std::fmt;
use tracing_subscriber::{
    EnvFilter,
    fmt::{format::Writer, time::FormatTime},
};

struct LoggerFormatter;

impl FormatTime for LoggerFormatter {
    fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
        write!(w, "{}", Local::now().format("%Y-%m-%d %H:%M:%S"))
    }
}

pub struct Gateway {}

#[async_trait]
impl ProxyHttp for Gateway {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

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

    /// 关键：设置 Host 头和 API Key，否则 WAF 拦截
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

    /// 关键：透传 body（必须！）
    async fn request_body_filter(
        &self,
        _: &mut Session,
        _: &mut Option<Bytes>,
        _: bool,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        Ok(())
    }

    async fn request_filter(&self, session: &mut Session, _ctx: &mut Self::CTX) -> Result<bool> {
        let req = session.req_header();

        if req.method == "OPTIONS" {
            let mut resp = ResponseHeader::build(204, None)?;
            resp.insert_header("Access-Control-Allow-Origin", "*")?;
            resp.insert_header("Access-Control-Allow-Headers", "*")?;
            resp.insert_header("Access-Control-Allow-Methods", "*")?;
            resp.insert_header("Access-Control-Allow-Credentials", "true")?;

            session.write_response_header(Box::new(resp), false).await?;
            return Ok(true);
        }

        Ok(false)
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
        _upstream_response: &mut ResponseHeader,
        _ctx: &mut Self::CTX,
    ) -> Result<()> {
        Ok(())
    }
}

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> Result<()> {
    // Initialize logging (after config is loaded to use configured log level)
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_timer(LoggerFormatter)
        .init();

    let mut my_server = Server::new(None)?;
    my_server.bootstrap();

    let mut proxy_service = http_proxy_service(&my_server.configuration, Gateway {});
    proxy_service.add_tcp("0.0.0.0:9066");

    my_server.add_service(proxy_service);
    my_server.run_forever();
}
