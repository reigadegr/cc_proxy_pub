pub mod handler;

use std::sync::Arc;

use my_proxy::{HttpClient, RequestStats, create_http_client};

/// Salvo gateway handler
pub struct GatewayHandler {
    pub stats: Arc<RequestStats>,
    pub client: Arc<HttpClient>,
}

impl GatewayHandler {
    pub fn new() -> Self {
        Self {
            stats: Arc::new(RequestStats::default()),
            client: create_http_client(),
        }
    }

    pub const fn stats(&self) -> &Arc<RequestStats> {
        &self.stats
    }

    pub const fn client(&self) -> &Arc<HttpClient> {
        &self.client
    }
}
