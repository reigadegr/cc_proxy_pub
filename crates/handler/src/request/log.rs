use http::HeaderMap;
use tracing::info;

/// 打印全部请求头
pub fn log_request_meta(method: &str, uri: &str, headers: &HeaderMap) {
    info!("=== 请求头 ===");
    info!("Method: {}", method);
    info!("URI: {}", uri);

    for (name, value) in headers {
        if let Ok(value_str) = value.to_str() {
            info!("{}: {}", name, value_str);
        }
    }
    info!("=== 请求头结束 ===");
}
