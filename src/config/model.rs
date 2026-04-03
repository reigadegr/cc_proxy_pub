use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, IntoDeserializer, Visitor},
};

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

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicDirect => "anthropic",
            Self::OpenAIResponses => "openai_responses",
            Self::OpenAIChat => "openai_chat",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamModes(Vec<Mode>);

impl UpstreamModes {
    pub fn supports(&self, mode: Mode) -> bool {
        self.0.contains(&mode)
    }

    fn normalize(modes: Vec<Mode>) -> Self {
        let mut normalized = Vec::with_capacity(modes.len());
        for mode in modes {
            if !normalized.contains(&mode) {
                normalized.push(mode);
            }
        }

        if normalized.is_empty() {
            return Self::default();
        }

        Self(normalized)
    }
}

impl Default for UpstreamModes {
    fn default() -> Self {
        Self(vec![Mode::AnthropicDirect])
    }
}

impl From<Vec<Mode>> for UpstreamModes {
    fn from(modes: Vec<Mode>) -> Self {
        Self::normalize(modes)
    }
}

impl Serialize for UpstreamModes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.0.len() == 1 {
            return self.0[0].serialize(serializer);
        }

        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UpstreamModes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UpstreamModesVisitor;

        impl<'de> Visitor<'de> for UpstreamModesVisitor {
            type Value = UpstreamModes;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a mode string or a non-empty mode array")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let mode = Mode::deserialize(value.into_deserializer())?;
                Ok(UpstreamModes::normalize(vec![mode]))
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let modes =
                    Vec::<Mode>::deserialize(serde::de::value::SeqAccessDeserializer::new(seq))?;
                if modes.is_empty() {
                    return Err(de::Error::custom("mode array must not be empty"));
                }

                Ok(UpstreamModes::normalize(modes))
            }
        }

        deserializer.deserialize_any(UpstreamModesVisitor)
    }
}

impl fmt::Display for UpstreamModes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.len() == 1 {
            return write!(f, "{}", self.0[0]);
        }

        let joined = self
            .0
            .iter()
            .map(|mode| mode.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        write!(f, "[{joined}]")
    }
}

/// 上游提供商配置
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpstreamConfig {
    /// 是否启用该上游；关闭后会在选择阶段被跳过
    #[serde(default = "default_true")]
    pub enable: bool,
    /// 上游主机地址+路径
    #[serde(alias = "endpoint")]
    pub base_url: String,
    /// 模型名称（覆盖请求体中的 model 字段）
    #[serde(default = "default_model")]
    pub model: String,
    /// API 密钥列表（支持多个 key 进行负载均衡）
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// 上游协议类型，支持单值或数组
    #[serde(default)]
    pub mode: UpstreamModes,
}

/// 配置结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 服务监听端口
    #[serde(default = "default_port")]
    pub port: u16,
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
            base_url: String::new(),
            model: default_model(),
            api_keys: Vec::new(),
            mode: UpstreamModes::default(),
        }
    }
}

pub const fn default_true() -> bool {
    true
}

pub const fn default_port() -> u16 {
    9066
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
    use super::{Config, Mode, UpstreamModes, default_port};

    #[test]
    fn upstream_enable_defaults_to_true() {
        let config: Config = toml::from_str(
            r#"
                [[upstream]]
                base_url = "https://example.com"
                model = "test-model"
                api_keys = ["test-key"]
                mode = "anthropic"
            "#,
        )
        .unwrap();

        assert_eq!(config.upstream.len(), 1);
        assert!(config.upstream[0].enable);
        assert_eq!(config.upstream[0].mode, UpstreamModes::default());
    }

    #[test]
    fn upstream_mode_accepts_array_and_deduplicates() {
        let config: Config = toml::from_str(
            r#"
                [[upstream]]
                base_url = "https://example.com"
                model = "test-model"
                api_keys = ["test-key"]
                mode = ["anthropic", "openai_responses", "anthropic"]
            "#,
        )
        .unwrap();

        assert_eq!(
            config.upstream[0].mode,
            UpstreamModes::normalize(vec![Mode::AnthropicDirect, Mode::OpenAIResponses])
        );
    }

    #[test]
    fn upstream_endpoint_alias_still_deserializes() {
        let config: Config = toml::from_str(
            r#"
                [[upstream]]
                endpoint = "https://legacy.example.com"
                model = "test-model"
                api_keys = ["test-key"]
            "#,
        )
        .unwrap();

        assert_eq!(config.upstream[0].base_url, "https://legacy.example.com");
    }

    #[test]
    fn upstream_mode_rejects_empty_array() {
        let result = toml::from_str::<Config>(
            r#"
                [[upstream]]
                base_url = "https://example.com"
                model = "test-model"
                api_keys = ["test-key"]
                mode = []
            "#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn port_defaults_to_9066() {
        let config: Config = toml::from_str(
            r#"
                [[upstream]]
                base_url = "https://example.com"
                model = "test-model"
                api_keys = ["test-key"]
            "#,
        )
        .unwrap();

        assert_eq!(config.port, default_port());
    }

    #[test]
    fn port_can_be_overridden() {
        let config: Config = toml::from_str(
            r#"
                port = 19066

                [[upstream]]
                base_url = "https://example.com"
                model = "test-model"
                api_keys = ["test-key"]
            "#,
        )
        .unwrap();

        assert_eq!(config.port, 19066);
    }
}
