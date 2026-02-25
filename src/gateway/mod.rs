pub mod handler;
pub mod openai_compat;
pub mod optimization;
pub mod service;

use std::sync::{Arc, atomic::AtomicU64};

use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

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

/// HTTP 客户端类型别名
pub type HttpClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

/// Salvo gateway handler
pub struct GatewayHandler {
    pub stats: Arc<RequestStats>,
    pub client: Arc<HttpClient>,
}

impl GatewayHandler {
    pub fn new() -> Self {
        // 创建支持 HTTP 和 HTTPS 的连接器
        // 使用 webpki-roots 内置证书，不依赖系统证书，提高跨平台稳定性
        let https = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();

        let client = Client::builder(TokioExecutor::new()).build(https);

        Self {
            stats: Arc::new(RequestStats::default()),
            client: Arc::new(client),
        }
    }

    pub const fn stats(&self) -> &Arc<RequestStats> {
        &self.stats
    }

    pub const fn client(&self) -> &Arc<HttpClient> {
        &self.client
    }
}
