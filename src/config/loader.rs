use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

use tracing::{info, warn};

use super::{Config, format::format_toml, model::enabled_upstream_count};

pub fn resolve_config_path() -> PathBuf {
    env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("config.toml"), PathBuf::from)
}

pub fn load_initial_config(config_path: &Path) -> Config {
    info!("📂 正在加载配置文件: {:?}", config_path);

    let raw_content = fs::read_to_string(config_path).unwrap_or_default();

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

    if let Err(error) = fs::write(config_path, &formatted_content) {
        warn!("❌ 写入格式化配置失败: {:?}, error: {}", config_path, error);
    } else {
        info!("✅ 配置文件格式化结果已写回: {:?}", config_path);
    }

    let config = load_from_file(config_path).unwrap_or_else(|error| {
        warn!("⚠️  配置加载失败: {}，退出中", error);
        process::exit(1);
    });

    log_loaded_config(&config);
    config
}

pub fn load_from_file(path: impl AsRef<Path>) -> Result<Config, String> {
    let content = fs::read_to_string(path.as_ref())
        .map_err(|error| format!("Failed to read config file: {error}"))?;

    toml::from_str(&content).map_err(|error| format!("Failed to parse TOML: {error}"))
}

pub fn log_loaded_config(config: &Config) {
    info!("✅ 配置已加载:");
    info!("listen_port: {}", config.port);
    info!(
        "upstream 数量: {} 个（启用 {} 个）",
        config.upstream.len(),
        enabled_upstream_count(&config.upstream)
    );
    for (index, upstream) in config.upstream.iter().enumerate() {
        info!(
            "  [{}] enable={}, endpoint={}, model={}, mode={:?}, api_keys={} 个",
            index,
            upstream.enable,
            upstream.endpoint,
            upstream.model,
            upstream.mode,
            upstream.api_keys.len()
        );
        for (key_index, key) in upstream.api_keys.iter().enumerate() {
            info!(
                "      api_key[{}]: {}***",
                key_index,
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
}
