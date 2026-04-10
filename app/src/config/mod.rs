pub mod format;
mod loader;
mod runtime;
mod watcher;

pub use self::runtime::AtomicConfig;
// 纯配置核心已下沉到独立 crate，这里仅保留 app runtime 的桥接导出。
pub use cli_req_refiner_config::{
    Config, Mode, OptimizationConfig, UpstreamConfig, UpstreamSelector, enabled_upstream_count,
};
