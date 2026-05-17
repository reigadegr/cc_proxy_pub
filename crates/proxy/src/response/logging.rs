/// 打印请求体
pub fn log_full_body(body: &str) {
    let len = body.len();
    let kb = len as f64 / 1024.0;
    tracing::info!("=== 请求体 (共 {} 字节) ===", len);
    tracing::info!("\n{}", body);
    tracing::info!("=== 请求体结束 ({} 字节 / {:.2} KB) ===", len, kb);
}

/// 打印响应体
pub fn log_full_response(body: &str) {
    let len = body.len();
    let kb = len as f64 / 1024.0;
    tracing::info!("=== 响应体 (共 {} 字节) ===", len);
    tracing::info!("{}", body);
    tracing::info!("=== 响应体结束 ({} 字节 / {:.2} KB) ===", len, kb);
}

/// 打印全部请求头
pub fn log_request_meta(method: &str, uri: &str, headers: &http::HeaderMap) {
    tracing::info!("=== 请求头 ===");
    tracing::info!("Method: {}", method);
    tracing::info!("URI: {}", uri);

    for (name, value) in headers {
        if let Ok(value_str) = value.to_str() {
            tracing::info!("{}: {}", name, value_str);
        }
    }
    tracing::info!("=== 请求头结束 ===");
}
