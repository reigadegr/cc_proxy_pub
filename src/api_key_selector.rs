//! API Key LRU 选择器
//!
//! 使用 LRU 策略实现多个 API Key 的负载均衡

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Mutex;

/// API Key 选择器，使用 LRU 策略实现负载均衡
pub struct ApiKeySelector {
    /// LRU 缓存，key 是索引，value 是一个虚拟标记（用于追踪使用情况）
    lru: Mutex<LruCache<usize, ()>>,
    /// 可用的 API keys
    keys: Vec<String>,
}

impl ApiKeySelector {
    /// 创建新的 API Key 选择器
    pub fn new(keys: Vec<String>) -> Self {
        // NonZeroUsize::new(1) 永远是 Some，所以 unwrap 是安全的
        const ONE: NonZeroUsize = match NonZeroUsize::new(1) {
            Some(n) => n,
            None => unreachable!(),
        };
        let capacity = NonZeroUsize::new(keys.len()).unwrap_or(ONE);
        let mut lru = LruCache::new(capacity);

        // 初始化所有 key 到缓存中
        for i in 0..keys.len() {
            lru.push(i, ());
        }

        Self {
            lru: Mutex::new(lru),
            keys,
        }
    }

    /// 获取下一个要使用的 API Key（LRU 策略：选择最久未使用的）
    pub fn next_key(&self) -> String {
        let key = {
            // 如果 mutex 被污染，尝试获取锁并恢复
            let mut lru = match self.lru.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    // Mutex 被污染，获取内部的 guard
                    e.into_inner()
                }
            };
            if let Some((idx, ())) = lru.pop_lru() {
                let key = self.keys[idx].clone();
                // 重新放回缓存（现在它变成 MRU 了）
                lru.push(idx, ());
                drop(lru); // 尽早释放锁
                Some(key)
            } else {
                drop(lru); // 尽早释放锁
                // 如果缓存为空（不应该发生），返回第一个 key
                self.keys.first().cloned()
            }
        };
        key.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_robin() {
        let keys = vec!["key1".to_string(), "key2".to_string(), "key3".to_string()];
        let selector = ApiKeySelector::new(keys);

        // LRU 策略：每次选择最久未使用的
        // 初始顺序: key1(key3->key2->key1), key2, key3

        let first = selector.next_key();
        assert_eq!(first, "key1"); // key1 是 LRU

        let second = selector.next_key();
        assert_eq!(second, "key2"); // 现在 key2 是 LRU

        let third = selector.next_key();
        assert_eq!(third, "key3"); // 现在 key3 是 LRU

        let fourth = selector.next_key();
        assert_eq!(fourth, "key1"); // key1 又变成 LRU 了
    }
}
