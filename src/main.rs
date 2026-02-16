use async_trait::async_trait;
use chrono::Local;
use pingora::prelude::*;
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

    /// 关键：设置 Host 头和 API Key，否则 WAF/智谱 拦截
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
