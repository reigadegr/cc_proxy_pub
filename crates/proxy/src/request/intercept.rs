use http::{HeaderName, HeaderValue};
use my_config::Config;
use my_optimization::{
    OptimizationResponse, try_local_optimization_from_json, try_local_url_optimization,
};
use salvo::prelude::*;

use crate::response::log_full_response;

fn write_local_optimization_response(
    res: &mut Response,
    local_response: OptimizationResponse,
    config: &Config,
) {
    tracing::info!("✅ 本地优化命中: {}", local_response.reason);

    res.status_code(StatusCode::OK);
    res.headers_mut().insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("application/json"),
    );

    if let Ok(value) = HeaderValue::from_str(local_response.reason) {
        res.headers_mut()
            .insert(HeaderName::from_static("x-cc-proxy-optimization"), value);
    }

    if let Ok(body_str) = std::str::from_utf8(&local_response.body)
        && config.server.log_res_body
    {
        log_full_response(body_str);
    }

    res.body(local_response.body);
}

pub fn req_local_intercept_by_url(res: &mut Response, request_url: &str, config: &Config) -> bool {
    let Some(local_response) = try_local_url_optimization(request_url, &config.optimizations)
    else {
        return false;
    };

    write_local_optimization_response(res, local_response, config);
    true
}

pub fn req_local_intercept_from_json(
    res: &mut Response,
    request_json: &serde_json::Value,
    request_url: &str,
    config: &Config,
) -> bool {
    let Some(local_response) =
        try_local_optimization_from_json(request_json, request_url, &config.optimizations)
    else {
        return false;
    };

    write_local_optimization_response(res, local_response, config);
    true
}
