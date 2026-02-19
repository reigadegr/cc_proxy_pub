//! API Key 轮询选择器
//!
//! 使用无锁原子 round-robin 策略实现多个 API Key 的负载均衡

use std::sync::atomic::{AtomicUsize, Ordering};

/// API Key 选择器，使用原子 round-robin 策略实现负载均衡
pub struct ApiKeySelector {
    /// 可用的 API keys
    keys: Vec<String>,
    /// 下一个要使用的 key 索引（单调递增，取模后使用）
    next_index: AtomicUsize,
}

impl ApiKeySelector {
    /// 创建新的 API Key 选择器
    pub const fn new(keys: Vec<String>) -> Self {
        Self {
            keys,
            next_index: AtomicUsize::new(0),
        }
    }

    /// 获取下一个要使用的 API Key（round-robin）
    pub fn next_key(&self) -> String {
        let len = self.keys.len();
        if len == 0 {
            return String::new();
        }

        let idx = self.next_index.fetch_add(1, Ordering::Relaxed) % len;
        self.keys[idx].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_robin() {
        let keys = vec!["key1".to_string(), "key2".to_string(), "key3".to_string()];
        let selector = ApiKeySelector::new(keys);

        // round-robin: 依次轮询

        let first = selector.next_key();
        assert_eq!(first, "key1");

        let second = selector.next_key();
        assert_eq!(second, "key2");

        let third = selector.next_key();
        assert_eq!(third, "key3");

        let fourth = selector.next_key();
        assert_eq!(fourth, "key1");
    }

    #[test]
    fn test_empty_keys_returns_empty_string() {
        let selector = ApiKeySelector::new(Vec::new());
        assert_eq!(selector.next_key(), "");
    }
}
