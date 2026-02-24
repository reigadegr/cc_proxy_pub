//! Upstream 轮询选择器
//!
//! 使用双层 round-robin 策略：
//! 1. 外层：遍历每个 upstream
//! 2. 内层：在每个 upstream 内部遍历其 `api_keys`
//!    即：upstream[0].key[0] -> upstream[0].key[1] -> ... -> upstream[1].key[0] -> ...

use crate::config::UpstreamConfig;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Upstream 选择器，使用双层 round-robin 策略
pub struct UpstreamSelector {
    /// 上游配置列表
    upstreams: Vec<UpstreamConfig>,
    /// 下一个要使用的 (upstream索引, `api_key索引`) 的全局计数
    next_index: AtomicUsize,
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
        })
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
    /// 返回 (upstream索引, endpoint, model, `api_key`, `oai_api`)
    pub fn next(&self) -> Option<(usize, &str, &str, &str, bool)> {
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
            upstream.oai_api,
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
                oai_api: false,
            },
            UpstreamConfig {
                endpoint: "https://upstream2.example.com".to_string(),
                model: "model2".to_string(),
                api_keys: vec![
                    "key2a".to_string(),
                    "key2b".to_string(),
                    "key2c".to_string(),
                ],
                oai_api: true,
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
        let (idx0, _ep0, _, key0, oai_api0) =
            selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx0, 0);
        assert_eq!(key0, "key1a");
        assert!(!oai_api0);

        // 请求2: upstream[1], key[0]
        let (idx1, _ep1, _, key1, oai_api1) =
            selector.next().expect("测试数据确保 next() 返回有效值");
        assert_eq!(idx1, 1);
        assert_eq!(key1, "key2a");
        assert!(oai_api1);

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
}
