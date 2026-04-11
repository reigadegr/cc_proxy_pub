mod gateway;

use std::{fmt, io::IsTerminal, sync::Arc};

use chrono::Local;
use gateway::{
    GatewayHandler,
    handler::{responses_alias_proxy, unified_proxy},
};
use my_config::AtomicConfig;
use salvo::{affix_state, prelude::*};
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
    let cfg = atomic_config.get();
    let listen_addr = format!("0.0.0.0:{}", cfg.port);
    info!(
        "Initial config: {} upstream(s), {} enabled",
        cfg.upstream.len(),
        cfg.upstream.iter().filter(|up| up.enable).count()
    );
    for (i, up) in cfg.upstream.iter().enumerate() {
        info!(
            "  [{}] name={}, enable={}, base_url={}, modes={}, api_keys={}",
            i,
            if up.name.is_empty() {
                "-"
            } else {
                up.name.as_str()
            },
            up.enable,
            up.base_url,
            up.mode,
            up.api_keys.len()
        );
    }

    // 启动配置文件监听线程
    Arc::clone(&atomic_config).start_watcher();

    // 创建 gateway handler（包含复用的 HTTP 客户端）
    let gateway = GatewayHandler::new();

    // 构建路由 - 使用 affix_state::inject 注入共享状态
    let router = Router::new()
        .hoop(
            affix_state::inject(atomic_config)
                .inject(Arc::clone(gateway.stats()))
                .inject(Arc::clone(gateway.client())),
        )
        .push(Router::with_path("responses").goal(responses_alias_proxy))
        .push(Router::with_path("v1/{**rest}").goal(unified_proxy));

    // 启动服务器
    info!("Server listening on {}", &listen_addr);

    let doc = OpenApi::new("salvo web api", "0.0.1").merge_router(&router);
    let router = router
        .unshift(doc.into_router("/api-doc/openapi.json"))
        .unshift(Scalar::new("/api-doc/openapi.json").into_router("scalar"));
    info!(
        "📖 Open API Page: http://{}/scalar",
        listen_addr.replace("0.0.0.0", "127.0.0.1")
    );
    let acceptor = TcpListener::new(listen_addr).bind().await;
    Server::new(acceptor).serve(router).await;

    Ok(())
}
