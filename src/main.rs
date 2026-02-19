mod config;
mod gateway;

use anyhow::Context;
use chrono::Local;
use config::AtomicConfig;
use gateway::{GatewayHandler, handler::proxy_handler};
use salvo::affix_state;
use salvo::prelude::*;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
        "Initial config: api_key={}***, endpoint={}",
        atomic_config
            .get()
            .api_key
            .chars()
            .take(8)
            .collect::<String>(),
        atomic_config.get().endpoint
    );

    // 启动配置文件监听线程
    Arc::clone(&atomic_config).start_watcher();

    // 创建 gateway handler（包含复用的 HTTP 客户端）
    let gateway = GatewayHandler::new()
        .context("failed to initialize gateway handler")?;

    // 构建路由 - 使用 affix_state::inject 注入共享状态
    let router = Router::new()
        .hoop(
            affix_state::inject(atomic_config)
                .inject(Arc::clone(gateway.stats()))
                .inject(Arc::clone(gateway.client())),
        )
        .push(Router::with_path("{**rest}").goal(proxy_handler));

    // 启动服务器
    let acceptor = TcpListener::new("0.0.0.0:9066").bind().await;
    info!("Server listening on 0.0.0.0:9066");

    Server::new(acceptor).serve(router).await;

    Ok(())
}
