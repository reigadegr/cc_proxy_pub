//! Upstream 轮询选择器
//!
//! 使用双层 round-robin 策略：
//! 1. 外层：遍历每个 upstream
//! 2. 内层：在每个 upstream 内部遍历其 `api_keys`
//!    即：upstream[0].key[0] -> upstream[0].key[1] -> ... -> upstream[1].key[0] -> ...

use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Mode, UpstreamConfig, model::GlobalUserAgentConfig};

type UpstreamSelection<'a> = (
    usize,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    Option<&'a str>,
    Mode,
);

/// Upstream 选择器，使用双层 round-robin 策略
pub struct UpstreamSelector {
    /// 上游配置列表
    upstreams: Vec<UpstreamConfig>,
    /// 按接口区分的全局默认 User-Agent
    global_user_agents: GlobalUserAgentConfig,
    /// `anthropic` 模式独立轮询计数
    next_index_anthropic: AtomicUsize,
    /// `openai_responses` 模式独立轮询计数
    next_index_openai_responses: AtomicUsize,
    /// `openai_chat` 模式独立轮询计数
    next_index_openai_chat: AtomicUsize,
}

impl UpstreamSelector {
    /// 创建新的 Upstream 选择器
    #[cfg(test)]
    #[must_use]
    pub fn new(global_user_agent: Option<String>, upstreams: Vec<UpstreamConfig>) -> Option<Self> {
        Self::new_with_global_user_agents(
            GlobalUserAgentConfig {
                claude: global_user_agent,
                codex: None,
            },
            upstreams,
        )
    }

    #[must_use]
    pub fn new_with_global_user_agents(
        global_user_agents: GlobalUserAgentConfig,
        upstreams: Vec<UpstreamConfig>,
    ) -> Option<Self> {
        if upstreams.is_empty() {
            return None;
        }
        Some(Self {
            upstreams,
            global_user_agents,
            next_index_anthropic: AtomicUsize::new(0),
            next_index_openai_responses: AtomicUsize::new(0),
            next_index_openai_chat: AtomicUsize::new(0),
        })
    }

    const fn mode_counter(&self, mode: Mode) -> &AtomicUsize {
        match mode {
            Mode::AnthropicDirect => &self.next_index_anthropic,
            Mode::OpenAIResponses => &self.next_index_openai_responses,
            Mode::OpenAIChat => &self.next_index_openai_chat,
        }
    }

    fn resolve_user_agent<'a>(
        &'a self,
        upstream: &'a UpstreamConfig,
        expected_mode: Mode,
    ) -> Option<&'a str> {
        upstream
            .user_agent_for_mode(expected_mode)
            .or_else(|| self.global_user_agents.resolve_for_mode(expected_mode))
    }

    /// 获取指定 mode 当前可用的 upstream 数量
    pub fn matching_count_by_mode(&self, expected_mode: Mode) -> usize {
        self.upstreams
            .iter()
            .filter(|upstream| upstream.enable && upstream.mode.supports(expected_mode))
            .count()
    }

    /// 获取下一个匹配指定 mode 的 upstream 和对应的 `api_key`
    /// 双层轮询策略：
    /// 1. 外层：按 round-robin 选择 upstream
    /// 2. 内层：在该 upstream 内部按 round-robin 选择 `api_key`
    ///
    /// 例如：2个upstream，每个有3个key
    /// 请求1: upstream[0], key[0]
    /// 请求2: upstream[1], key[0]
    /// 请求3: upstream[0], key[1]
    /// 请求4: upstream[1], key[1]
    /// 请求5: upstream[0], key[2]
    /// 请求6: upstream[1], key[2]
    /// 请求7: upstream[0], key[0]  (循环)
    ///
    /// 返回 (upstream索引, `name`, `base_url`, model, `api_key`, `user_agent`, `mode`)
    ///
    pub fn next_by_mode(&self, expected_mode: Mode) -> Option<UpstreamSelection<'_>> {
        if self.upstreams.is_empty() {
            return None;
        }

        let matching_count = self.matching_count_by_mode(expected_mode);

        if matching_count == 0 {
            return None;
        }

        let mode_idx = self
            .mode_counter(expected_mode)
            .fetch_add(1, Ordering::Relaxed);
        let target_pos = mode_idx % matching_count;

        let mut seen = 0;
        let (upstream_idx, upstream) =
            self.upstreams.iter().enumerate().find(|(_, upstream)| {
                if !upstream.enable || !upstream.mode.supports(expected_mode) {
                    return false;
                }

                let is_target = seen == target_pos;
                seen += 1;
                is_target
            })?;

        let api_key = if upstream.api_keys.is_empty() {
            ""
        } else {
            let key_count = upstream.api_keys.len();
            let key_idx = (mode_idx / matching_count) % key_count;
            &upstream.api_keys[key_idx]
        };

        Some((
            upstream_idx,
            &upstream.name,
            &upstream.base_url,
            &upstream.model,
            api_key,
            self.resolve_user_agent(upstream, expected_mode),
            expected_mode,
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn create_test_upstreams() -> Vec<UpstreamConfig> {
        vec![
            UpstreamConfig {
                enable: true,
                name: "upstream-1".to_string(),
                base_url: "https://upstream1.example.com".to_string(),
                model: "model1".to_string(),
                api_keys: vec!["key1a".to_string(), "key1b".to_string()],
                user_agent_claude: Some("Device-A/1.0".to_string()),
                user_agent_codex: None,
                mode: vec![Mode::AnthropicDirect].into(),
            },
            UpstreamConfig {
                enable: true,
                name: "upstream-2".to_string(),
                base_url: "https://upstream2.example.com".to_string(),
                model: "model2".to_string(),
                api_keys: vec![
                    "key2a".to_string(),
                    "key2b".to_string(),
                    "key2c".to_string(),
                ],
                user_agent_claude: None,
                user_agent_codex: None,
                mode: vec![Mode::OpenAIResponses].into(),
            },
        ]
    }

    #[test]
    fn test_empty_upstreams_returns_none() {
        let selector = UpstreamSelector::new(None, Vec::new());
        // new() 返回 None 当输入为空时
        assert!(selector.is_none());
    }

    #[test]
    fn test_next_by_mode_only_returns_matching_upstreams() {
        let upstreams = create_test_upstreams();
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        let (idx0, _, _, _, key0, user_agent0, mode0) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能选到 openai_responses upstream");
        assert_eq!(idx0, 1);
        assert_eq!(key0, "key2a");
        assert_eq!(user_agent0, None);
        assert_eq!(mode0, Mode::OpenAIResponses);

        let (idx1, _, _, _, key1, user_agent1, mode1) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能继续选到 openai_responses upstream");
        assert_eq!(idx1, 1);
        assert_eq!(key1, "key2b");
        assert_eq!(user_agent1, None);
        assert_eq!(mode1, Mode::OpenAIResponses);
    }

    #[test]
    fn test_next_by_mode_round_robins_across_matching_upstreams() {
        let upstreams = vec![
            UpstreamConfig {
                enable: true,
                name: "upstream-1".to_string(),
                base_url: "https://upstream1.example.com".to_string(),
                model: "model1".to_string(),
                api_keys: vec!["key1a".to_string()],
                user_agent_claude: None,
                user_agent_codex: None,
                mode: vec![Mode::AnthropicDirect].into(),
            },
            UpstreamConfig {
                enable: true,
                name: "upstream-2".to_string(),
                base_url: "https://upstream2.example.com".to_string(),
                model: "model2".to_string(),
                api_keys: vec!["key2a".to_string(), "key2b".to_string()],
                user_agent_claude: None,
                user_agent_codex: Some("Device-B/1.0".to_string()),
                mode: vec![Mode::OpenAIResponses].into(),
            },
            UpstreamConfig {
                enable: true,
                name: "upstream-3".to_string(),
                base_url: "https://upstream3.example.com".to_string(),
                model: "model3".to_string(),
                api_keys: vec!["key3a".to_string(), "key3b".to_string()],
                user_agent_claude: None,
                user_agent_codex: None,
                mode: vec![Mode::OpenAIResponses].into(),
            },
        ];
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        let (idx0, _, _, _, key0, user_agent0, mode0) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能选到第一个匹配 upstream");
        assert_eq!(idx0, 1);
        assert_eq!(key0, "key2a");
        assert_eq!(user_agent0, Some("Device-B/1.0"));
        assert_eq!(mode0, Mode::OpenAIResponses);

        let (idx1, _, _, _, key1, user_agent1, mode1) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能选到第二个匹配 upstream");
        assert_eq!(idx1, 2);
        assert_eq!(key1, "key3a");
        assert_eq!(user_agent1, None);
        assert_eq!(mode1, Mode::OpenAIResponses);

        let (idx2, _, _, _, key2, user_agent2, mode2) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能继续轮询第一个匹配 upstream 的下一个 key");
        assert_eq!(idx2, 1);
        assert_eq!(key2, "key2b");
        assert_eq!(user_agent2, Some("Device-B/1.0"));
        assert_eq!(mode2, Mode::OpenAIResponses);

        let (idx3, _, _, _, key3, user_agent3, mode3) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能继续轮询第二个匹配 upstream 的下一个 key");
        assert_eq!(idx3, 2);
        assert_eq!(key3, "key3b");
        assert_eq!(user_agent3, None);
        assert_eq!(mode3, Mode::OpenAIResponses);
    }

    #[test]
    fn test_next_by_mode_skips_disabled_upstreams() {
        let upstreams = vec![
            UpstreamConfig {
                enable: false,
                name: "disabled-upstream".to_string(),
                base_url: "https://disabled.example.com".to_string(),
                model: "disabled-model".to_string(),
                api_keys: vec!["disabled-key".to_string()],
                user_agent_claude: None,
                user_agent_codex: None,
                mode: vec![Mode::OpenAIResponses].into(),
            },
            UpstreamConfig {
                enable: true,
                name: "enabled-upstream".to_string(),
                base_url: "https://enabled.example.com".to_string(),
                model: "enabled-model".to_string(),
                api_keys: vec!["enabled-key".to_string()],
                user_agent_claude: None,
                user_agent_codex: Some("Device-C/1.0".to_string()),
                mode: vec![Mode::OpenAIResponses].into(),
            },
        ];
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        let (idx, name, base_url, model, key, user_agent, mode) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应跳过禁用 upstream，选择启用项");

        assert_eq!(idx, 1);
        assert_eq!(name, "enabled-upstream");
        assert_eq!(base_url, "https://enabled.example.com");
        assert_eq!(model, "enabled-model");
        assert_eq!(key, "enabled-key");
        assert_eq!(user_agent, Some("Device-C/1.0"));
        assert_eq!(mode, Mode::OpenAIResponses);
    }

    #[test]
    fn test_next_by_mode_returns_none_when_all_matching_upstreams_disabled() {
        let upstreams = vec![UpstreamConfig {
            enable: false,
            name: "disabled-upstream".to_string(),
            base_url: "https://disabled.example.com".to_string(),
            model: "disabled-model".to_string(),
            api_keys: vec!["disabled-key".to_string()],
            user_agent_claude: None,
            user_agent_codex: None,
            mode: vec![Mode::OpenAIResponses].into(),
        }];
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        assert!(selector.next_by_mode(Mode::OpenAIResponses).is_none());
    }

    #[test]
    fn test_next_by_mode_supports_multi_mode_upstream() {
        let upstreams = vec![
            UpstreamConfig {
                enable: true,
                name: "shared-upstream".to_string(),
                base_url: "https://multi.example.com".to_string(),
                model: "shared-model".to_string(),
                api_keys: vec!["shared-key-1".to_string(), "shared-key-2".to_string()],
                user_agent_claude: Some("Claude-Shared-UA/1.0".to_string()),
                user_agent_codex: Some("Codex-Shared-UA/1.0".to_string()),
                mode: vec![Mode::AnthropicDirect, Mode::OpenAIResponses].into(),
            },
            UpstreamConfig {
                enable: true,
                name: "responses-only".to_string(),
                base_url: "https://responses-only.example.com".to_string(),
                model: "responses-model".to_string(),
                api_keys: vec!["responses-key".to_string()],
                user_agent_claude: None,
                user_agent_codex: None,
                mode: vec![Mode::OpenAIResponses].into(),
            },
        ];
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        let (anthropic_idx, _, _, _, anthropic_key, anthropic_user_agent, anthropic_mode) =
            selector
                .next_by_mode(Mode::AnthropicDirect)
                .expect("多协议 upstream 应支持 anthropic");
        assert_eq!(anthropic_idx, 0);
        assert_eq!(anthropic_key, "shared-key-1");
        assert_eq!(anthropic_user_agent, Some("Claude-Shared-UA/1.0"));
        assert_eq!(anthropic_mode, Mode::AnthropicDirect);

        let (responses_idx_0, _, _, _, responses_key_0, responses_user_agent_0, responses_mode_0) =
            selector
                .next_by_mode(Mode::OpenAIResponses)
                .expect("多协议 upstream 应支持 openai_responses");
        assert_eq!(responses_idx_0, 0);
        assert_eq!(responses_key_0, "shared-key-1");
        assert_eq!(responses_user_agent_0, Some("Codex-Shared-UA/1.0"));
        assert_eq!(responses_mode_0, Mode::OpenAIResponses);

        let (responses_idx_1, _, _, _, responses_key_1, responses_user_agent_1, responses_mode_1) =
            selector
                .next_by_mode(Mode::OpenAIResponses)
                .expect("responses 协议应继续轮询其他 upstream");
        assert_eq!(responses_idx_1, 1);
        assert_eq!(responses_key_1, "responses-key");
        assert_eq!(responses_user_agent_1, None);
        assert_eq!(responses_mode_1, Mode::OpenAIResponses);
    }

    #[test]
    fn test_matching_count_by_mode_only_counts_enabled_matching_upstreams() {
        let upstreams = vec![
            UpstreamConfig {
                enable: true,
                name: "anthropic-only".to_string(),
                base_url: "https://anthropic.example.com".to_string(),
                model: "anthropic-model".to_string(),
                api_keys: vec!["anthropic-key".to_string()],
                user_agent_claude: None,
                user_agent_codex: None,
                mode: vec![Mode::AnthropicDirect].into(),
            },
            UpstreamConfig {
                enable: true,
                name: "shared-upstream".to_string(),
                base_url: "https://shared.example.com".to_string(),
                model: "shared-model".to_string(),
                api_keys: vec!["shared-key".to_string()],
                user_agent_claude: Some("Shared-Claude-UA/1.0".to_string()),
                user_agent_codex: Some("Shared-Codex-UA/1.0".to_string()),
                mode: vec![Mode::AnthropicDirect, Mode::OpenAIResponses].into(),
            },
            UpstreamConfig {
                enable: false,
                name: "disabled-upstream".to_string(),
                base_url: "https://disabled.example.com".to_string(),
                model: "disabled-model".to_string(),
                api_keys: vec!["disabled-key".to_string()],
                user_agent_claude: None,
                user_agent_codex: None,
                mode: vec![Mode::OpenAIResponses].into(),
            },
        ];
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        assert_eq!(selector.matching_count_by_mode(Mode::AnthropicDirect), 2);
        assert_eq!(selector.matching_count_by_mode(Mode::OpenAIResponses), 1);
        assert_eq!(selector.matching_count_by_mode(Mode::OpenAIChat), 0);
    }

    #[test]
    fn test_next_by_mode_returns_configured_user_agent() {
        let upstreams = vec![UpstreamConfig {
            enable: true,
            name: "ua-upstream".to_string(),
            base_url: "https://ua.example.com".to_string(),
            model: "ua-model".to_string(),
            api_keys: vec!["ua-key".to_string()],
            user_agent_claude: Some("Device-D/1.0".to_string()),
            user_agent_codex: None,
            mode: vec![Mode::AnthropicDirect].into(),
        }];
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        let (_, _, _, _, _, user_agent, _) = selector
            .next_by_mode(Mode::AnthropicDirect)
            .expect("应能返回 upstream 的 user_agent");

        assert_eq!(user_agent, Some("Device-D/1.0"));
    }

    #[test]
    fn test_next_by_mode_falls_back_to_global_user_agent() {
        let upstreams = vec![UpstreamConfig {
            enable: true,
            name: "global-ua-upstream".to_string(),
            base_url: "https://global-ua.example.com".to_string(),
            model: "ua-model".to_string(),
            api_keys: vec!["ua-key".to_string()],
            user_agent_claude: None,
            user_agent_codex: None,
            mode: vec![Mode::AnthropicDirect].into(),
        }];
        let selector = UpstreamSelector::new_with_global_user_agents(
            GlobalUserAgentConfig {
                claude: Some("Global-UA/1.0".to_string()),
                codex: None,
            },
            upstreams,
        )
        .expect("测试数据已确保 upstreams 非空");

        let (_, _, _, _, _, user_agent, _) = selector
            .next_by_mode(Mode::AnthropicDirect)
            .expect("应能返回全局 user_agent");

        assert_eq!(user_agent, Some("Global-UA/1.0"));
    }

    #[test]
    fn test_next_by_mode_prefers_upstream_user_agent_over_global() {
        let upstreams = vec![UpstreamConfig {
            enable: true,
            name: "override-ua-upstream".to_string(),
            base_url: "https://override-ua.example.com".to_string(),
            model: "ua-model".to_string(),
            api_keys: vec!["ua-key".to_string()],
            user_agent_claude: Some("Upstream-UA/2.0".to_string()),
            user_agent_codex: None,
            mode: vec![Mode::AnthropicDirect].into(),
        }];
        let selector = UpstreamSelector::new_with_global_user_agents(
            GlobalUserAgentConfig {
                claude: Some("Global-UA/1.0".to_string()),
                codex: None,
            },
            upstreams,
        )
        .expect("测试数据已确保 upstreams 非空");

        let (_, _, _, _, _, user_agent, _) = selector
            .next_by_mode(Mode::AnthropicDirect)
            .expect("应能返回渠道 user_agent");

        assert_eq!(user_agent, Some("Upstream-UA/2.0"));
    }

    #[test]
    fn test_next_by_mode_prefers_upstream_user_agent_over_mode_specific_global_user_agent() {
        let upstreams = vec![UpstreamConfig {
            enable: true,
            name: "mode-specific-upstream".to_string(),
            base_url: "https://mode-specific.example.com".to_string(),
            model: "ua-model".to_string(),
            api_keys: vec!["ua-key".to_string()],
            user_agent_claude: Some("Claude-Upstream-UA/1.0".to_string()),
            user_agent_codex: Some("Codex-Upstream-UA/1.0".to_string()),
            mode: vec![Mode::AnthropicDirect, Mode::OpenAIResponses].into(),
        }];
        let selector = UpstreamSelector::new_with_global_user_agents(
            GlobalUserAgentConfig {
                claude: Some("Claude-Global-UA/2.0".to_string()),
                codex: Some("Codex-Global-UA/3.0".to_string()),
            },
            upstreams,
        )
        .expect("测试数据已确保 upstreams 非空");

        let (_, _, _, _, _, claude_user_agent, _) = selector
            .next_by_mode(Mode::AnthropicDirect)
            .expect("应优先返回 upstream 的 user_agent");
        assert_eq!(claude_user_agent, Some("Claude-Upstream-UA/1.0"));

        let (_, _, _, _, _, codex_user_agent, _) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应优先返回 upstream 的 user_agent");
        assert_eq!(codex_user_agent, Some("Codex-Upstream-UA/1.0"));
    }

    #[test]
    fn test_next_by_mode_prefers_mode_specific_global_user_agent() {
        let upstreams = vec![UpstreamConfig {
            enable: true,
            name: "global-mode-specific-upstream".to_string(),
            base_url: "https://global-mode-specific.example.com".to_string(),
            model: "ua-model".to_string(),
            api_keys: vec!["ua-key".to_string()],
            user_agent_claude: None,
            user_agent_codex: None,
            mode: vec![Mode::AnthropicDirect, Mode::OpenAIResponses].into(),
        }];
        let selector = UpstreamSelector::new_with_global_user_agents(
            GlobalUserAgentConfig {
                claude: Some("Claude-Global-UA/2.0".to_string()),
                codex: Some("Codex-Global-UA/3.0".to_string()),
            },
            upstreams,
        )
        .expect("测试数据已确保 upstreams 非空");

        let (_, _, _, _, _, claude_user_agent, _) = selector
            .next_by_mode(Mode::AnthropicDirect)
            .expect("应能返回 Claude 全局专属 user_agent");
        assert_eq!(claude_user_agent, Some("Claude-Global-UA/2.0"));

        let (_, _, _, _, _, codex_user_agent, _) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能返回 Codex 全局专属 user_agent");
        assert_eq!(codex_user_agent, Some("Codex-Global-UA/3.0"));
    }

    #[test]
    fn test_next_by_mode_returns_openai_chat_upstream_only() {
        let upstreams = vec![
            UpstreamConfig {
                enable: true,
                name: "responses-upstream".to_string(),
                base_url: "https://responses.example.com".to_string(),
                model: "responses-model".to_string(),
                api_keys: vec!["responses-key".to_string()],
                user_agent_claude: None,
                user_agent_codex: Some("Codex-Responses-UA/1.0".to_string()),
                mode: vec![Mode::OpenAIResponses].into(),
            },
            UpstreamConfig {
                enable: true,
                name: "chat-upstream".to_string(),
                base_url: "https://chat.example.com".to_string(),
                model: "chat-model".to_string(),
                api_keys: vec!["chat-key".to_string()],
                user_agent_claude: None,
                user_agent_codex: Some("Codex-Chat-UA/1.0".to_string()),
                mode: vec![Mode::OpenAIChat].into(),
            },
        ];
        let selector =
            UpstreamSelector::new(None, upstreams).expect("测试数据已确保 upstreams 非空");

        let (idx, name, base_url, model, key, user_agent, mode) = selector
            .next_by_mode(Mode::OpenAIChat)
            .expect("应只选择 openai_chat upstream");

        assert_eq!(idx, 1);
        assert_eq!(name, "chat-upstream");
        assert_eq!(base_url, "https://chat.example.com");
        assert_eq!(model, "chat-model");
        assert_eq!(key, "chat-key");
        assert_eq!(user_agent, Some("Codex-Chat-UA/1.0"));
        assert_eq!(mode, Mode::OpenAIChat);
    }

    #[test]
    fn test_next_by_mode_uses_codex_global_user_agent_for_openai_chat() {
        let upstreams = vec![UpstreamConfig {
            enable: true,
            name: "chat-upstream".to_string(),
            base_url: "https://chat.example.com".to_string(),
            model: "chat-model".to_string(),
            api_keys: vec!["chat-key".to_string()],
            user_agent_claude: None,
            user_agent_codex: None,
            mode: vec![Mode::OpenAIChat].into(),
        }];
        let selector = UpstreamSelector::new_with_global_user_agents(
            GlobalUserAgentConfig {
                claude: Some("Claude-Global-UA/2.0".to_string()),
                codex: Some("Codex-Global-UA/3.0".to_string()),
            },
            upstreams,
        )
        .expect("测试数据已确保 upstreams 非空");

        let (_, _, _, _, _, user_agent, mode) = selector
            .next_by_mode(Mode::OpenAIChat)
            .expect("应能返回 OpenAI Chat 的全局 codex user_agent");

        assert_eq!(user_agent, Some("Codex-Global-UA/3.0"));
        assert_eq!(mode, Mode::OpenAIChat);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod openai_chat_tests {
    use super::*;

    #[test]
    fn test_next_by_mode_returns_codex_user_agent_for_openai_chat() {
        let upstreams = vec![UpstreamConfig {
            enable: true,
            name: "chat-upstream".to_string(),
            base_url: "https://chat.example.com".to_string(),
            model: "chat-model".to_string(),
            api_keys: vec!["chat-key".to_string()],
            user_agent_claude: None,
            user_agent_codex: Some("Codex-UA/4.0".to_string()),
            mode: vec![Mode::OpenAIChat].into(),
        }];
        let selector = UpstreamSelector::new_with_global_user_agents(
            GlobalUserAgentConfig {
                claude: None,
                codex: Some("Codex-Global-UA/5.0".to_string()),
            },
            upstreams,
        )
        .expect("测试数据已确保 upstreams 非空");

        let (idx, _, _, _, key, user_agent, mode) = selector
            .next_by_mode(Mode::OpenAIChat)
            .expect("应能选到 openai_chat upstream");

        assert_eq!(idx, 0);
        assert_eq!(key, "chat-key");
        assert_eq!(user_agent, Some("Codex-UA/4.0"));
        assert_eq!(mode, Mode::OpenAIChat);
    }
}
