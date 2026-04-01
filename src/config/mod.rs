pub mod format;
pub mod selector;

use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::Duration,
};

use arc_swap::{ArcSwap, Guard};
use format::format_toml;
use notify::{
    EventKind, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode},
};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use self::selector::UpstreamSelector;

/// 工作模式枚举
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Mode {
    /// Anthropic 接口，供 `claude_proxy` 直通转发
    #[serde(rename = "anthropic")]
    #[default]
    AnthropicDirect,
    /// `OpenAI` Responses 接口，供 `codex_proxy` 直通转发
    #[serde(rename = "openai_responses")]
    OpenAIResponses,
    /// `OpenAI` Chat Completions 接口（预留）
    #[serde(rename = "openai_chat")]
    OpenAIChat,
}

/// 全局原子配置，支持热重载
pub struct AtomicConfig {
    inner: ArcSwap<Config>,
    config_path: PathBuf,
    /// Upstream `选择器（双层轮询：先upstream，后api_keys`）
    upstream_selector: ArcSwap<Option<Arc<UpstreamSelector>>>,
}

/// 上游提供商配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpstreamConfig {
    /// 是否启用该上游；关闭后会在选择阶段被跳过
    #[serde(default = "default_true")]
    pub enable: bool,
    /// 上游主机地址+路径
    pub endpoint: String,
    /// 模型名称（覆盖请求体中的 model 字段）
    #[serde(default = "default_model")]
    pub model: String,
    /// API 密钥列表（支持多个 key 进行负载均衡）
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// 上游协议类型
    #[serde(default)]
    pub mode: Mode,
}

/// 配置结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 是否打印请求体
    #[serde(default)]
    pub log_req_body: bool,
    /// 是否打印响应体
    #[serde(default)]
    pub log_res_body: bool,
    /// 上游提供商配置列表（支持多个上游负载均衡）
    #[serde(default)]
    pub upstream: Vec<UpstreamConfig>,
    /// 本地优化拦截开关
    #[serde(default)]
    pub optimizations: OptimizationConfig,
}

/// 本地优化配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OptimizationConfig {
    #[serde(default = "default_true")]
    pub enable_network_probe_mock: bool,
    #[serde(default = "default_true")]
    pub enable_fast_prefix_detection: bool,
    #[serde(default = "default_true")]
    pub enable_historical_analysis_mock: bool,
    #[serde(default = "default_true")]
    pub enable_title_generation_skip: bool,
    #[serde(default = "default_true")]
    pub enable_suggestion_mode_skip: bool,
    #[serde(default = "default_true")]
    pub enable_filepath_extraction_mock: bool,
}

impl Default for OptimizationConfig {
    fn default() -> Self {
        Self {
            enable_network_probe_mock: default_true(),
            enable_fast_prefix_detection: default_true(),
            enable_historical_analysis_mock: default_true(),
            enable_title_generation_skip: default_true(),
            enable_suggestion_mode_skip: default_true(),
            enable_filepath_extraction_mock: default_true(),
        }
    }
}

const fn default_model() -> String {
    String::new()
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            enable: default_true(),
            endpoint: String::new(),
            model: default_model(),
            api_keys: Vec::new(),
            mode: Mode::AnthropicDirect,
        }
    }
}

const fn default_true() -> bool {
    true
}

impl AtomicConfig {
    /// 初始化配置，从指定路径或默认路径加载
    pub fn init() -> Self {
        let config_path = env::args()
            .nth(1)
            .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from);

        info!("📂 正在加载配置文件: {:?}", config_path);

        let raw_content = fs::read_to_string(&config_path).unwrap_or_default();

        info!(
            "🧹 开始格式化配置文件: {:?} ({} 字节)",
            config_path,
            raw_content.len()
        );

        let formatted_content = format_toml(&raw_content);
        let formatting_changed = raw_content != formatted_content;
        if formatting_changed {
            info!(
                "✨ 配置文件格式化后有变化: {:?} ({} -> {} 字节)",
                config_path,
                raw_content.len(),
                formatted_content.len()
            );
        } else {
            info!("ℹ️ 配置文件格式化后无变化: {:?}", config_path);
        }

        if let Err(e) = fs::write(&config_path, &formatted_content) {
            warn!("❌ 写入格式化配置失败: {:?}, error: {}", config_path, e);
        } else {
            info!("✅ 配置文件格式化结果已写回: {:?}", config_path);
        }

        let config = Self::load_from_file(&config_path).unwrap_or_else(|e| {
            warn!("⚠️  配置加载失败: {}，退出中", e);
            process::exit(1); // 非零退出码表示异常退出
        });

        info!("✅ 配置已加载:");
        info!(
            "upstream 数量: {} 个（启用 {} 个）",
            config.upstream.len(),
            enabled_upstream_count(&config.upstream)
        );
        for (i, up) in config.upstream.iter().enumerate() {
            info!(
                "  [{}] enable={}, endpoint={}, model={}, mode={:?}, api_keys={} 个",
                i,
                up.enable,
                up.endpoint,
                up.model,
                up.mode,
                up.api_keys.len()
            );
            for (j, key) in up.api_keys.iter().enumerate() {
                info!(
                    "      api_key[{}]: {}***",
                    j,
                    key.chars().take(8).collect::<String>()
                );
            }
        }
        info!(
            "optimizations: quota={}, prefix={}, title={}, suggestion={}, filepath={}",
            config.optimizations.enable_network_probe_mock,
            config.optimizations.enable_fast_prefix_detection,
            config.optimizations.enable_title_generation_skip,
            config.optimizations.enable_suggestion_mode_skip,
            config.optimizations.enable_filepath_extraction_mock,
        );
        info!("log_req_body: {}", config.log_req_body);
        info!("log_res_body: {}", config.log_res_body);

        // 创建 Upstream 选择器（双层轮询）
        let upstream_selector = UpstreamSelector::new(config.upstream.clone()).map(Arc::new);

        Self {
            inner: ArcSwap::from(Arc::new(config)),
            config_path,
            upstream_selector: ArcSwap::from(Arc::new(upstream_selector)),
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

    /// 获取 Upstream 选择器（双层轮询）
    pub fn get_upstream_selector(&self) -> Option<Arc<UpstreamSelector>> {
        (**self.upstream_selector.load()).clone()
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
                let upstream_changed = old.upstream != new_config.upstream;
                let optimizations_changed = old.optimizations != new_config.optimizations;
                let log_req_body_changed = old.log_req_body != new_config.log_req_body;
                let log_res_body_changed = old.log_res_body != new_config.log_res_body;
                self.inner.store(Arc::new(new_config.clone()));

                // 更新 Upstream 选择器
                if upstream_changed {
                    let new_selector =
                        UpstreamSelector::new(new_config.upstream.clone()).map(Arc::new);
                    self.upstream_selector.store(Arc::new(new_selector));
                }

                if upstream_changed
                    || optimizations_changed
                    || log_req_body_changed
                    || log_res_body_changed
                {
                    info!("✅ 配置已更新:");
                    if upstream_changed {
                        info!(
                            "upstream: {} 个（启用 {} 个） -> {} 个（启用 {} 个）",
                            old.upstream.len(),
                            enabled_upstream_count(&old.upstream),
                            new_config.upstream.len(),
                            enabled_upstream_count(&new_config.upstream)
                        );
                        for (i, up) in new_config.upstream.iter().enumerate() {
                            info!(
                                "  [{}] enable={}, endpoint={}, model={}, mode={:?}, api_keys={} 个",
                                i,
                                up.enable,
                                up.endpoint,
                                up.model,
                                up.mode,
                                up.api_keys.len()
                            );
                        }
                    }

                    if optimizations_changed {
                        info!(
                            "optimizations: quota {}→{}, prefix {}→{}, title {}→{}, suggestion {}→{}, filepath {}→{}",
                            old.optimizations.enable_network_probe_mock,
                            new_config.optimizations.enable_network_probe_mock,
                            old.optimizations.enable_fast_prefix_detection,
                            new_config.optimizations.enable_fast_prefix_detection,
                            old.optimizations.enable_title_generation_skip,
                            new_config.optimizations.enable_title_generation_skip,
                            old.optimizations.enable_suggestion_mode_skip,
                            new_config.optimizations.enable_suggestion_mode_skip,
                            old.optimizations.enable_filepath_extraction_mock,
                            new_config.optimizations.enable_filepath_extraction_mock,
                        );
                    }

                    if log_req_body_changed {
                        info!(
                            "log_req_body: {}→{}",
                            old.log_req_body, new_config.log_req_body,
                        );
                    }

                    if log_res_body_changed {
                        info!(
                            "log_res_body: {}→{}",
                            old.log_res_body, new_config.log_res_body,
                        );
                    }
                } else {
                    info!("ℹ️ 配置文件内容未变化");
                }

                info!(
                    "📋 当前配置: upstream={} 个（启用 {} 个）",
                    new_config.upstream.len(),
                    enabled_upstream_count(&new_config.upstream)
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

fn enabled_upstream_count(upstreams: &[UpstreamConfig]) -> usize {
    upstreams.iter().filter(|upstream| upstream.enable).count()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{Config, Mode};

    #[test]
    fn upstream_enable_defaults_to_true() {
        let config: Config = toml::from_str(
            r#"
                [[upstream]]
                endpoint = "https://example.com"
                model = "test-model"
                api_keys = ["test-key"]
                mode = "anthropic"
            "#,
        )
        .unwrap();

        assert_eq!(config.upstream.len(), 1);
        assert!(config.upstream[0].enable);
        assert_eq!(config.upstream[0].mode, Mode::AnthropicDirect);
    }
}
