mod config;
mod gateway;

use chrono::Local;
use config::AtomicConfig;
use gateway::Gateway;
use pingora::prelude::*;
use std::fmt;
use std::io::IsTerminal;
use std::sync::Arc;
use tracing::info;
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

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> Result<()> {
    // 初始化日志
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let is_terminal = std::io::stdout().is_terminal();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_timer(LoggerFormatter)
        .with_ansi(is_terminal)
        .init();

    // 初始化配置
    let atomic_config = Arc::new(AtomicConfig::init());
    info!(
        "Initial config: api_key={}***, host={}",
        atomic_config
            .get()
            .api_key
            .chars()
            .take(8)
            .collect::<String>(),
        atomic_config.get().host
    );

    // 启动配置文件监听线程
    Arc::clone(&atomic_config).start_watcher();

    let mut my_server = Server::new(None)?;
    my_server.bootstrap();

    let mut proxy_service =
        http_proxy_service(&my_server.configuration, Gateway::new(atomic_config));
    proxy_service.add_tcp("0.0.0.0:9066");

    my_server.add_service(proxy_service);
    my_server.run_forever();
}
