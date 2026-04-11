use std::{path::Path, sync::Arc, time::Duration};

use arc_swap::{ArcSwap, Guard};
use tracing::{error, info};

use crate::{
    Config, UpstreamConfig, UpstreamSelector, enabled_upstream_count,
    loader::{load_from_file, load_initial_config, resolve_config_path},
    watcher::start_config_watcher,
};

/// 全局原子配置，支持热重载
pub struct AtomicConfig {
    inner: ArcSwap<Config>,
    config_path: std::path::PathBuf,
    /// Upstream 选择器（双层轮询：先 upstream，后 `api_keys`）
    upstream_selector: ArcSwap<Option<Arc<UpstreamSelector>>>,
}

impl AtomicConfig {
    /// 初始化配置，从指定路径或默认路径加载
    #[must_use]
    pub fn init() -> Self {
        let config_path = resolve_config_path();
        let config = load_initial_config(&config_path);
        let upstream_selector = UpstreamSelector::new_with_global_user_agents(
            config.global_user_agent_config(),
            config.upstream.clone(),
        )
        .map(Arc::new);

        Self {
            inner: ArcSwap::from(Arc::new(config)),
            config_path,
            upstream_selector: ArcSwap::from(Arc::new(upstream_selector)),
        }
    }

    /// 从文件加载配置
    pub fn get(&self) -> Guard<Arc<Config>> {
        self.inner.load()
    }

    /// 获取 Upstream 选择器（双层轮询）
    pub fn get_upstream_selector(&self) -> Option<Arc<UpstreamSelector>> {
        (**self.upstream_selector.load()).clone()
    }

    /// 重新加载配置
    pub fn reload(&self) {
        std::thread::sleep(Duration::from_millis(50));
        info!("🔄 检测到配置文件变更，正在重新加载...");

        match load_from_file(&self.config_path) {
            Ok(new_config) => {
                let old = self.inner.load();
                let old_global_user_agents = old.global_user_agent_config();
                let new_global_user_agents = new_config.global_user_agent_config();

                let port_changed = old.server.port != new_config.server.port;
                let upstream_changed = old.upstream != new_config.upstream;
                let user_agent_global_changed = old_global_user_agents != new_global_user_agents;
                let optimizations_changed = old.optimizations != new_config.optimizations;
                let log_req_body_changed =
                    old.server.log_req_body != new_config.server.log_req_body;
                let log_res_body_changed =
                    old.server.log_res_body != new_config.server.log_res_body;
                self.inner.store(Arc::new(new_config.clone()));

                if upstream_changed || user_agent_global_changed {
                    let new_selector = UpstreamSelector::new_with_global_user_agents(
                        new_global_user_agents.clone(),
                        new_config.upstream.clone(),
                    )
                    .map(Arc::new);
                    self.upstream_selector.store(Arc::new(new_selector));
                }

                if port_changed
                    || upstream_changed
                    || user_agent_global_changed
                    || optimizations_changed
                    || log_req_body_changed
                    || log_res_body_changed
                {
                    info!("✅ 配置已更新:");
                    if port_changed {
                        info!(
                            "listen_port: {}→{}（重启服务后生效）",
                            old.server.port, new_config.server.port
                        );
                    }

                    if upstream_changed {
                        log_upstream_change(&old.upstream, &new_config.upstream);
                    }

                    if user_agent_global_changed {
                        info!(
                            "global_user_agents: {:?}→{:?}",
                            old_global_user_agents, new_global_user_agents
                        );
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
                            old.server.log_req_body, new_config.server.log_req_body,
                        );
                    }

                    if log_res_body_changed {
                        info!(
                            "log_res_body: {}→{}",
                            old.server.log_res_body, new_config.server.log_res_body,
                        );
                    }
                } else {
                    info!("ℹ️ 配置文件内容未变化");
                }

                info!(
                    "📋 当前配置: upstream={} 个（启用 {} 个）, global_user_agent_configured={}",
                    new_config.upstream.len(),
                    enabled_upstream_count(&new_config.upstream),
                    new_global_user_agents.is_any_configured()
                );
            }
            Err(error) => {
                error!("❌ 配置重载失败: {}", error);
            }
        }
    }

    /// 启动配置文件监听（跨平台）
    pub fn start_watcher(self: Arc<Self>) {
        start_config_watcher(self);
    }

    pub(crate) fn config_path(&self) -> &Path {
        &self.config_path
    }
}

fn log_upstream_change(old_upstream: &[UpstreamConfig], new_upstream: &[UpstreamConfig]) {
    info!(
        "upstream: {} 个（启用 {} 个） -> {} 个（启用 {} 个）",
        old_upstream.len(),
        enabled_upstream_count(old_upstream),
        new_upstream.len(),
        enabled_upstream_count(new_upstream)
    );
    for (index, upstream) in new_upstream.iter().enumerate() {
        info!(
            "  [{}] name={}, enable={}, base_url={}, model={}, modes={}, api_keys={} 个, user_agent_configured={}",
            index,
            if upstream.name.is_empty() {
                "-"
            } else {
                upstream.name.as_str()
            },
            upstream.enable,
            upstream.base_url,
            upstream.model,
            upstream.mode,
            upstream.api_keys.len(),
            upstream.is_any_user_agent_configured()
        );
    }
}
