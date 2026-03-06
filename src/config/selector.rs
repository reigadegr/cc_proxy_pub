//! Upstream 轮询选择器
//!
//! 使用双层 round-robin 策略：
//! 1. 外层：遍历每个 upstream
//! 2. 内层：在每个 upstream 内部遍历其 `api_keys`
//!    即：upstream[0].key[0] -> upstream[0].key[1] -> ... -> upstream[1].key[0] -> ...

use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Mode, UpstreamConfig};

/// Upstream 选择器，使用双层 round-robin 策略
pub struct UpstreamSelector {
    /// 上游配置列表
    upstreams: Vec<UpstreamConfig>,
    /// 下一个要使用的 (upstream索引, `api_key索引`) 的全局计数
    next_index: AtomicUsize,
    /// `anthropic` 模式独立轮询计数
    next_index_anthropic: AtomicUsize,
    /// `openai_responses` 模式独立轮询计数
    next_index_openai_responses: AtomicUsize,
    /// `openai_chat` 模式独立轮询计数
    next_index_openai_chat: AtomicUsize,
}

impl UpstreamSelector {
    /// 创建新的 Upstream 选择器
    pub fn new(upstreams: Vec<UpstreamConfig>) -> Option<Self> {
        if upstreams.is_empty() {
            return None;
        }
        Some(Self {
            upstreams,
            next_index: AtomicUsize::new(0),
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

    /// 获取下一个要使用的 upstream 和对应的 `api_key`
    ///
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
    /// 返回 (upstream索引, endpoint, model, `api_key`, `mode`)
    pub fn next(&self) -> Option<(usize, &str, &str, &str, Mode)> {
        if self.upstreams.is_empty() {
            return None;
        }

        let upstream_count = self.upstreams.len();

        // 获取全局计数并递增
        let global_idx = self.next_index.fetch_add(1, Ordering::Relaxed);

        // 计算 upstream 索引和该 upstream 内的 key 索引
        let upstream_idx = global_idx % upstream_count;
        let upstream = &self.upstreams[upstream_idx];

        // 在该 upstream 的 api_keys 中轮询（返回借用，避免克隆）
        let api_key = if upstream.api_keys.is_empty() {
            ""
        } else {
            let key_count = upstream.api_keys.len();
            // 每个 upstream 使用不同的相位偏移，实现交错轮询
            let key_idx = (global_idx / upstream_count) % key_count;
            &upstream.api_keys[key_idx]
        };

        Some((
            upstream_idx,
            &upstream.endpoint,
            &upstream.model,
            api_key,
            upstream.mode,
        ))
    }

    /// 获取下一个匹配指定 mode 的 upstream 和对应的 `api_key`
    ///
    /// 注意：这里使用按 mode 独立的轮询状态，不会推进 `next()` 的全局游标。
    pub fn next_by_mode(&self, expected_mode: Mode) -> Option<(usize, &str, &str, &str, Mode)> {
        if self.upstreams.is_empty() {
            return None;
        }

        let matching_count = self
            .upstreams
            .iter()
            .filter(|upstream| upstream.mode == expected_mode)
            .count();

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
                if upstream.mode != expected_mode {
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
            &upstream.endpoint,
            &upstream.model,
            api_key,
            upstream.mode,
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
                endpoint: "https://upstream1.example.com".to_string(),
                model: "model1".to_string(),
                api_keys: vec!["key1a".to_string(), "key1b".to_string()],
                mode: Mode::AnthropicDirect,
            },
            UpstreamConfig {
                endpoint: "https://upstream2.example.com".to_string(),
                model: "model2".to_string(),
                api_keys: vec![
                    "key2a".to_string(),
                    "key2b".to_string(),
                    "key2c".to_string(),
                ],
                mode: Mode::OpenAIResponses,
            },
        ]
    }

    #[test]
    fn test_double_layer_round_robin() {
        let upstreams = create_test_upstreams();
        // 测试数据已确保非空
        let selector = UpstreamSelector::new(upstreams).expect("测试数据已确保 upstreams 非空");

        // 2个upstream，每个有2-3个key
        // 双层轮询：先每个upstream用key[0]，然后每个upstream用key[1]，依此类推

        // 请求1: upstream[0], key[0]
        let (idx0, _ep0, _, key0, mode0) = selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx0, 0);
        assert_eq!(key0, "key1a");
        assert_eq!(mode0, Mode::AnthropicDirect);

        // 请求2: upstream[1], key[0]
        let (idx1, _ep1, _, key1, mode1) = selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx1, 1);
        assert_eq!(key1, "key2a");
        assert_eq!(mode1, Mode::OpenAIResponses);

        // 请求3: upstream[0], key[1]
        let (idx2, _, _, key2, _) = selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx2, 0);
        assert_eq!(key2, "key1b");

        // 请求4: upstream[1], key[1]
        let (idx3, _, _, key3, _) = selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx3, 1);
        assert_eq!(key3, "key2b");

        // 请求5: upstream[0], 回到key[0] (upstream[0]只有2个key)
        let (idx4, _, _, key4, _) = selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx4, 0);
        assert_eq!(key4, "key1a");

        // 请求6: upstream[1], key[2] (upstream[1]有3个key)
        let (idx5, _, _, key5, _) = selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx5, 1);
        assert_eq!(key5, "key2c");

        // 请求7: upstream[0], key[1]
        let (idx6, _, _, key6, _) = selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx6, 0);
        assert_eq!(key6, "key1b");
    }

    #[test]
    fn test_empty_upstreams_returns_none() {
        let selector = UpstreamSelector::new(Vec::new());
        // new() 返回 None 当输入为空时
        assert!(selector.is_none());
    }

    #[test]
    fn test_next_by_mode_only_returns_matching_upstreams() {
        let upstreams = create_test_upstreams();
        let selector = UpstreamSelector::new(upstreams).expect("测试数据已确保 upstreams 非空");

        let (idx0, _ep0, _model0, key0, mode0) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能选到 openai_responses upstream");
        assert_eq!(idx0, 1);
        assert_eq!(key0, "key2a");
        assert_eq!(mode0, Mode::OpenAIResponses);

        let (idx1, _ep1, _model1, key1, mode1) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能继续选到 openai_responses upstream");
        assert_eq!(idx1, 1);
        assert_eq!(key1, "key2b");
        assert_eq!(mode1, Mode::OpenAIResponses);
    }

    #[test]
    fn test_next_by_mode_does_not_advance_global_round_robin() {
        let upstreams = create_test_upstreams();
        let selector = UpstreamSelector::new(upstreams).expect("测试数据已确保 upstreams 非空");

        let (filtered_idx, _, _, filtered_key, filtered_mode) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能选到 openai_responses upstream");
        assert_eq!(filtered_idx, 1);
        assert_eq!(filtered_key, "key2a");
        assert_eq!(filtered_mode, Mode::OpenAIResponses);

        let (global_idx0, _, _, global_key0, global_mode0) =
            selector.next().expect("全局轮询不应被 next_by_mode 影响");
        assert_eq!(global_idx0, 0);
        assert_eq!(global_key0, "key1a");
        assert_eq!(global_mode0, Mode::AnthropicDirect);

        let (global_idx1, _, _, global_key1, global_mode1) =
            selector.next().expect("全局轮询应继续按原顺序前进");
        assert_eq!(global_idx1, 1);
        assert_eq!(global_key1, "key2a");
        assert_eq!(global_mode1, Mode::OpenAIResponses);
    }

    #[test]
    fn test_next_by_mode_round_robins_across_matching_upstreams() {
        let upstreams = vec![
            UpstreamConfig {
                endpoint: "https://upstream1.example.com".to_string(),
                model: "model1".to_string(),
                api_keys: vec!["key1a".to_string()],
                mode: Mode::AnthropicDirect,
            },
            UpstreamConfig {
                endpoint: "https://upstream2.example.com".to_string(),
                model: "model2".to_string(),
                api_keys: vec!["key2a".to_string(), "key2b".to_string()],
                mode: Mode::OpenAIResponses,
            },
            UpstreamConfig {
                endpoint: "https://upstream3.example.com".to_string(),
                model: "model3".to_string(),
                api_keys: vec!["key3a".to_string(), "key3b".to_string()],
                mode: Mode::OpenAIResponses,
            },
        ];
        let selector = UpstreamSelector::new(upstreams).expect("测试数据已确保 upstreams 非空");

        let (idx0, _, _, key0, mode0) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能选到第一个匹配 upstream");
        assert_eq!(idx0, 1);
        assert_eq!(key0, "key2a");
        assert_eq!(mode0, Mode::OpenAIResponses);

        let (idx1, _, _, key1, mode1) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能选到第二个匹配 upstream");
        assert_eq!(idx1, 2);
        assert_eq!(key1, "key3a");
        assert_eq!(mode1, Mode::OpenAIResponses);

        let (idx2, _, _, key2, mode2) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能继续轮询第一个匹配 upstream 的下一个 key");
        assert_eq!(idx2, 1);
        assert_eq!(key2, "key2b");
        assert_eq!(mode2, Mode::OpenAIResponses);

        let (idx3, _, _, key3, mode3) = selector
            .next_by_mode(Mode::OpenAIResponses)
            .expect("应能继续轮询第二个匹配 upstream 的下一个 key");
        assert_eq!(idx3, 2);
        assert_eq!(key3, "key3b");
        assert_eq!(mode3, Mode::OpenAIResponses);
    }

    #[test]
    fn test_next_by_mode_returns_none_when_no_match() {
        let upstreams = vec![UpstreamConfig {
            endpoint: "https://upstream1.example.com".to_string(),
            model: "model1".to_string(),
            api_keys: vec!["key1a".to_string()],
            mode: Mode::AnthropicDirect,
        }];
        let selector = UpstreamSelector::new(upstreams).expect("测试数据已确保 upstreams 非空");

        assert_eq!(selector.next_by_mode(Mode::OpenAIResponses), None);

        let (idx, _, _, key, mode) = selector.next().expect("无匹配 mode 时不应推进全局轮询状态");
        assert_eq!(idx, 0);
        assert_eq!(key, "key1a");
        assert_eq!(mode, Mode::AnthropicDirect);
    }
}
