pub mod handler;
pub mod service;

use std::{sync::Arc, sync::atomic::AtomicU64};

/// Token 统计
pub struct RequestStats {
    pub total_tokens: AtomicU64,
    pub user_new_tokens: AtomicU64,
    pub user_history_tokens: AtomicU64,
    pub assistant_tokens: AtomicU64,
    pub system_tokens: AtomicU64,
    pub request_count: AtomicU64,
}

impl Default for RequestStats {
    fn default() -> Self {
        Self {
            total_tokens: AtomicU64::new(0),
            user_new_tokens: AtomicU64::new(0),
            user_history_tokens: AtomicU64::new(0),
            assistant_tokens: AtomicU64::new(0),
            system_tokens: AtomicU64::new(0),
            request_count: AtomicU64::new(0),
        }
    }
}

/// Salvo gateway handler
pub struct GatewayHandler {
    pub stats: Arc<RequestStats>,
}

impl GatewayHandler {
    pub fn new() -> Self {
        Self {
            stats: Arc::new(RequestStats::default()),
        }
    }

    pub const fn stats(&self) -> &Arc<RequestStats> {
        &self.stats
    }
}
