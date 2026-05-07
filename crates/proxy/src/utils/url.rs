use salvo::prelude::*;
use tracing::info;

pub fn make_proxy_url<'a>(base_url: &'a str, req: &Request) -> (String, &'a str) {
    let host_str = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .unwrap_or(base_url);

    let (host, base_path) = host_str.split_once('/').unwrap_or((host_str, ""));

    let original_path = req.uri().path();
    let query = req.uri().query().unwrap_or("");
    let query_str = if query.is_empty() {
        String::new()
    } else {
        format!("?{query}")
    };

    let mut new_path = if base_path.is_empty() {
        format!("{original_path}{query_str}")
    } else {
        format!(
            "/{}/{}{}",
            base_path,
            original_path.trim_start_matches('/'),
            query_str
        )
    };

    // 消除重复的 /v1/v1 前缀（base_url 和请求路径都带 /v1 导致）
    while new_path.contains("/v1/v1/") || new_path.ends_with("/v1/v1") {
        new_path = new_path.replacen("/v1/v1", "/v1", 1);
    }

    let scheme = if base_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };

    let mut upstream_url = format!("{host}{new_path}");
    upstream_url = upstream_url.replace("?beta=true", "");

    while upstream_url.contains("//") {
        upstream_url = upstream_url.replace("//", "/");
    }
    upstream_url = format!("{scheme}://{upstream_url}");
    info!("Proxying to: {}", upstream_url);
    (upstream_url, host)
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::uri::Scheme;
    use hyper::Request as HyperRequest;
    use salvo::Request;

    use super::make_proxy_url;

    fn request_from_uri(uri: &str) -> Request {
        let request_result = HyperRequest::builder().uri(uri).body(Bytes::new());
        let Ok(request) = request_result else {
            panic!("failed to build request");
        };
        Request::from_hyper(request, Scheme::HTTP)
    }

    #[test]
    fn make_proxy_url_preserves_anthropic_root_path() {
        let req = request_from_uri("http://localhost/v1/messages");
        let (url, host) = make_proxy_url("https://upstream.example.com", &req);

        assert_eq!(url, "https://upstream.example.com/v1/messages");
        assert_eq!(host, "upstream.example.com");
    }

    #[test]
    fn make_proxy_url_preserves_anthropic_subpath_and_query() {
        let req = request_from_uri("http://localhost/v1/messages/count_tokens?foo=bar");
        let (url, _) = make_proxy_url("https://upstream.example.com", &req);

        assert_eq!(
            url,
            "https://upstream.example.com/v1/messages/count_tokens?foo=bar"
        );
    }

    #[test]
    fn make_proxy_url_joins_base_path_prefixes() {
        let req = request_from_uri("http://localhost/v1/messages?foo=bar");
        let (url, host) = make_proxy_url("https://upstream.example.com/prefix/api", &req);

        assert_eq!(
            url,
            "https://upstream.example.com/prefix/api/v1/messages?foo=bar"
        );
        assert_eq!(host, "upstream.example.com");
    }

    #[test]
    fn make_proxy_url_dedup_v1_in_base_url() {
        let req = request_from_uri("http://localhost/v1/messages");
        let (url, _) = make_proxy_url("https://upstream.example.com/v1", &req);

        assert_eq!(url, "https://upstream.example.com/v1/messages");
    }

    #[test]
    fn make_proxy_url_dedup_triple_v1() {
        let req = request_from_uri("http://localhost/v1/messages");
        let (url, _) = make_proxy_url("https://upstream.example.com/v1/v1", &req);

        assert_eq!(url, "https://upstream.example.com/v1/messages");
    }
}
