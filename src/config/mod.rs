pub mod format;

use arc_swap::{ArcSwap, Guard};
use format::format_toml;
use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::process;
use std::{env, fs, path::Path, path::PathBuf, sync::Arc, time::Duration};
use tracing::{error, info, warn};

/// 全局原子配置，支持热重载
pub struct AtomicConfig {
    inner: ArcSwap<Config>,
    config_path: PathBuf,
}

/// 配置结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 上游主机地址+路径
    pub endpoint: String,
    /// API 密钥
    pub api_key: String,
}

impl AtomicConfig {
    /// 初始化配置，从指定路径或默认路径加载
    pub fn init() -> Self {
        let config_path = env::args()
            .nth(1)
            .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from);

        info!("📂 正在加载配置文件: {:?}", config_path);

        let raw_content = fs::read_to_string(&config_path).unwrap_or_default();

        // 格式化TOML并写回文件
        let formatted_content = format_toml(&raw_content);
        if let Err(e) = fs::write(&config_path, formatted_content) {
            warn!("写入格式化配置失败: {}", e);
        }

        let config = Self::load_from_file(&config_path).unwrap_or_else(|e| {
            warn!("⚠️  配置加载失败: {}，退出中", e);
            process::exit(1); // 非零退出码表示异常退出
        });

        info!("✅ 配置已加载:");
        info!(
            "api_key = {}***",
            config.api_key.chars().take(8).collect::<String>()
        );
        info!("endpoint = {}", config.endpoint);

        Self {
            inner: ArcSwap::from(Arc::new(config)),
            config_path,
        }
    }

    /// 从文件加载配置
    fn load_from_file(path: impl AsRef<Path>) -> Result<Config, String> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("Failed to read config file: {e}"))?;

        let config: Config =
            toml::from_str(&content).map_err(|e| format!("Failed to parse TOML: {e}"))?;

        Ok(config)
    }

    /// 获取当前配置的 Guard（读操作）
    pub fn get(&self) -> Guard<Arc<Config>> {
        self.inner.load()
    }

    /// 重新加载配置
    pub fn reload(&self) {
        // 添加短暂延迟，确保文件写入完成
        std::thread::sleep(Duration::from_millis(50));

        info!("🔄 检测到配置文件变更，正在重新加载...");

        // 读取原始内容并格式化
        let _raw_content = match fs::read_to_string(&self.config_path) {
            Ok(content) => content,
            Err(e) => {
                error!("❌ 读取配置文件失败: {}", e);
                return;
            }
        };

        match Self::load_from_file(&self.config_path) {
            Ok(new_config) => {
                let old = self.inner.load();

                // 检测配置是否真的发生了变化
                let api_key_changed = old.api_key != new_config.api_key;
                let endpoint_changed = old.endpoint != new_config.endpoint;

                self.inner.store(Arc::new(new_config.clone()));

                if api_key_changed || endpoint_changed {
                    info!("✅ 配置已更新:");
                    if api_key_changed {
                        info!(
                            "api_key: {}*** → {}***",
                            old.api_key.chars().take(8).collect::<String>(),
                            new_config.api_key.chars().take(8).collect::<String>()
                        );
                    }

                    if endpoint_changed {
                        info!("endpoint: {} → {}", old.endpoint, new_config.endpoint);
                    }
                } else {
                    info!("ℹ️ 配置文件内容未变化");
                }

                info!(
                    "📋 当前配置: api_key={}***, endpoint={}",
                    new_config.api_key.chars().take(8).collect::<String>(),
                    new_config.endpoint
                );
            }
            Err(e) => {
                error!("❌ 配置重载失败: {}", e);
            }
        }
    }

    /// 启动配置文件监听（跨平台）
    ///
    /// 使用 `notify` crate 实现跨平台文件监听，支持 Windows/Linux/macOS
    /// 当文件被修改时自动重载配置
    pub fn start_watcher(self: Arc<Self>) {
        std::thread::spawn(move || {
            let config_path = self.config_path.clone();

            // 创建跨平台 watcher
            let mut watcher =
                match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    match res {
                        Ok(event) => {
                            if matches!(
                                event.kind,
                                EventKind::Access(AccessKind::Close(AccessMode::Write))
                            ) {
                                std::thread::sleep(Duration::from_millis(50));
                                self.reload();
                            }
                        }
                        Err(e) => error!("Config watch error: {}", e),
                    }
                }) {
                    Ok(w) => w,
                    Err(e) => {
                        error!("Failed to initialize watcher: {}", e);
                        return;
                    }
                };

            // 添加监听
            if let Err(e) = watcher.watch(&config_path, RecursiveMode::NonRecursive) {
                error!("Failed to add watch for config file: {}", e);
                return;
            }

            info!("👁️  配置文件监听已启动: {:?}", config_path);

            // 永久挂起线程，保 watcher 不被 drop
            std::thread::park();
        });
    }
}
