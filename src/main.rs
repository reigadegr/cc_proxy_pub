mod gateway;

use chrono::Local;
use gateway::Gateway;
use pingora::prelude::*;
use std::{fmt, io::IsTerminal};
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
    // Initialize logging (after config is loaded to use configured log level)
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // 检测 stdout 是否是终端，自动启用/禁用颜色
    let is_terminal = std::io::stdout().is_terminal();

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_timer(LoggerFormatter)
        .with_ansi(is_terminal)
        .init();

    let mut my_server = Server::new(None)?;
    my_server.bootstrap();

    let mut proxy_service = http_proxy_service(
        &my_server.configuration,
        <Gateway as std::default::Default>::default(),
    );
    proxy_service.add_tcp("0.0.0.0:9066");

    my_server.add_service(proxy_service);
    my_server.run_forever();
}
