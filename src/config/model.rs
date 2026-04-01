use serde::{Deserialize, Serialize};

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

pub const fn default_true() -> bool {
    true
}

pub const fn default_model() -> String {
    String::new()
}

pub fn enabled_upstream_count(upstreams: &[UpstreamConfig]) -> usize {
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
