pub mod format;

use crate::api_key_selector::ApiKeySelector;
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
    /// API Key 选择器（使用 round-robin 策略实现负载均衡）
    api_key_selector: ArcSwap<Option<Arc<ApiKeySelector>>>,
}

/// 配置结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 上游主机地址+路径
    pub endpoint: String,
    /// API 密钥列表（支持多个 key 进行负载均衡）
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// 模型名称（覆盖请求体中的 model 字段）
    #[serde(default = "default_model")]
    pub model: String,
}

// 向后兼容：如果只有一个 api_key 字段，自动转换为 api_keys 数组
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigLegacy {
    pub endpoint: String,
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
}

impl From<ConfigLegacy> for Config {
    fn from(legacy: ConfigLegacy) -> Self {
        Self {
            endpoint: legacy.endpoint,
            api_keys: vec![legacy.api_key],
            model: legacy.model,
        }
    }
}

const fn default_model() -> String {
    String::new()
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
        info!("api_keys: {} 个", config.api_keys.len());
        for (i, key) in config.api_keys.iter().enumerate() {
            info!("  [{}] {}***", i, key.chars().take(8).collect::<String>());
        }
        info!("endpoint = {}", config.endpoint);
        info!("model = {}", config.model);

        // 创建 API Key 选择器
        let api_key_selector = if config.api_keys.is_empty() {
            None
        } else {
            Some(Arc::new(ApiKeySelector::new(config.api_keys.clone())))
        };

        Self {
            inner: ArcSwap::from(Arc::new(config)),
            config_path,
            api_key_selector: ArcSwap::from(Arc::new(api_key_selector)),
        }
    }

    /// 从文件加载配置
    fn load_from_file(path: impl AsRef<Path>) -> Result<Config, String> {
        let content = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("Failed to read config file: {e}"))?;

        // 首先尝试加载新格式，如果失败则尝试旧格式
        let config: Config = if let Ok(cfg) = toml::from_str(&content) {
            cfg
        } else {
            // 尝试加载旧格式并转换
            let legacy: ConfigLegacy =
                toml::from_str(&content).map_err(|e| format!("Failed to parse TOML: {e}"))?;
            warn!("⚠️  检测到旧配置格式（api_key），已自动转换为新格式（api_keys）");
            Config::from(legacy)
        };

        Ok(config)
    }

    /// 获取当前配置的 Guard（读操作）
    pub fn get(&self) -> Guard<Arc<Config>> {
        self.inner.load()
    }

    /// 获取 API Key 选择器
    pub fn get_api_key_selector(&self) -> Option<Arc<ApiKeySelector>> {
        (**self.api_key_selector.load()).clone()
    }

    /// 重新加载配置
    pub fn reload(&self) {
        // 添加短暂延迟，确保文件写入完成
        std::thread::sleep(Duration::from_millis(50));

        info!("🔄 检测到配置文件变更，正在重新加载...");

        match Self::load_from_file(&self.config_path) {
            Ok(new_config) => {
                let old = self.inner.load();

                // 检测配置是否真的发生了变化
                let api_keys_changed = old.api_keys != new_config.api_keys;
                let endpoint_changed = old.endpoint != new_config.endpoint;
                let model_changed = old.model != new_config.model;

                self.inner.store(Arc::new(new_config.clone()));

                // 更新 API Key 选择器（如果 api_keys 发生了变化）
                if api_keys_changed {
                    let new_selector = if new_config.api_keys.is_empty() {
                        None
                    } else {
                        Some(Arc::new(ApiKeySelector::new(new_config.api_keys.clone())))
                    };
                    self.api_key_selector.store(Arc::new(new_selector));
                }

                if api_keys_changed || endpoint_changed || model_changed {
                    info!("✅ 配置已更新:");
                    if api_keys_changed {
                        info!(
                            "api_keys: {} 个 -> {} 个",
                            old.api_keys.len(),
                            new_config.api_keys.len()
                        );
                    }

                    if endpoint_changed {
                        info!("endpoint: {} → {}", old.endpoint, new_config.endpoint);
                    }
                    if model_changed {
                        info!("model: {} → {}", old.model, new_config.model);
                    }
                } else {
                    info!("ℹ️ 配置文件内容未变化");
                }

                info!(
                    "📋 当前配置: api_keys={} 个, endpoint={}",
                    new_config.api_keys.len(),
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
