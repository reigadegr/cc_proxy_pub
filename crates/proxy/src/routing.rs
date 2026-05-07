use salvo::http::Request;

/// 代理请求路由目标
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTarget {
    Anthropic,
    OpenAIResponses,
    OpenAIChat,
}

/// 将 `/responses` 等短别名重写为 `/v1/responses` 等标准路径
pub fn rewrite_short_alias(req: &mut Request, short_path: &str, long_path: &str) {
    if req.uri().path() != short_path {
        return;
    }

    let rewritten = req.uri().query().map_or_else(
        || long_path.to_owned(),
        |query| format!("{long_path}?{query}"),
    );
    let Ok(rewritten) = rewritten.parse() else {
        tracing::error!("Failed to parse rewritten {short_path} alias URI: {rewritten}");
        return;
    };
    *req.uri_mut() = rewritten;
}

/// 根据请求路径分类路由目标
#[must_use]
pub fn classify_request_path(path: &str) -> Option<RouteTarget> {
    if path.starts_with("/v1/messages") {
        Some(RouteTarget::Anthropic)
    } else if path == "/v1/responses" {
        Some(RouteTarget::OpenAIResponses)
    } else if path == "/v1/chat/completions" {
        Some(RouteTarget::OpenAIChat)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::uri::Scheme;
    use hyper::Request as HyperRequest;
    use salvo::{Router, Service, http::Request, prelude::*, test::TestClient};

    use super::{RouteTarget, classify_request_path, rewrite_short_alias};

    fn request_from_uri(uri: &str) -> Request {
        let request = match HyperRequest::builder().uri(uri).body(Bytes::new()) {
            Ok(request) => request,
            Err(error) => panic!("request should build: {error}"),
        };
        Request::from_hyper(request, Scheme::HTTP)
    }

    #[test]
    fn classify_request_path_matches_expected_targets() {
        let cases = [
            ("/v1/messages", Some(RouteTarget::Anthropic)),
            ("/v1/messages/count_tokens", Some(RouteTarget::Anthropic)),
            ("/v1/responses", Some(RouteTarget::OpenAIResponses)),
            ("/v1/chat/completions", Some(RouteTarget::OpenAIChat)),
            ("/responses", None),
            ("/claude/messages", None),
            ("/codex/responses", None),
            ("/responses/foo", None),
            ("/foo", None),
        ];

        for (path, expected) in cases {
            assert_eq!(classify_request_path(path), expected, "{path}");
        }
    }

    #[test]
    fn rewrite_short_alias_normalizes_path_before_classification() {
        let mut req = request_from_uri("http://localhost/responses?stream=true");

        rewrite_short_alias(&mut req, "/responses", "/v1/responses");

        assert_eq!(req.uri().path(), "/v1/responses");
        assert_eq!(req.uri().query(), Some("stream=true"));
        assert_eq!(
            classify_request_path(req.uri().path()),
            Some(RouteTarget::OpenAIResponses)
        );
    }

    #[test]
    fn rewrite_short_alias_normalizes_chat_completions_path() {
        let mut req = request_from_uri("http://localhost/chat/completions?stream=true");

        rewrite_short_alias(&mut req, "/chat/completions", "/v1/chat/completions");

        assert_eq!(req.uri().path(), "/v1/chat/completions");
        assert_eq!(req.uri().query(), Some("stream=true"));
        assert_eq!(
            classify_request_path(req.uri().path()),
            Some(RouteTarget::OpenAIChat)
        );
    }

    #[tokio::test]
    async fn route_table_only_adds_exact_responses_short_path() {
        #[endpoint]
        async fn alias_marker() {}

        #[endpoint]
        async fn v1_marker() {}

        let service = Service::new(
            Router::new()
                .push(Router::with_path("responses").goal(alias_marker))
                .push(Router::with_path("v1/{**rest}").goal(v1_marker)),
        );

        let alias = TestClient::get("http://127.0.0.1:5801/responses")
            .send(&service)
            .await;
        assert_eq!(alias.status_code, Some(StatusCode::OK));

        let canonical = TestClient::get("http://127.0.0.1:5801/v1/responses")
            .send(&service)
            .await;
        assert_eq!(canonical.status_code, Some(StatusCode::OK));

        let other_short_path = TestClient::get("http://127.0.0.1:5801/messages")
            .send(&service)
            .await;
        assert_eq!(other_short_path.status_code, Some(StatusCode::NOT_FOUND));

        let unrelated_short_path = TestClient::get("http://127.0.0.1:5801/foo")
            .send(&service)
            .await;
        assert_eq!(
            unrelated_short_path.status_code,
            Some(StatusCode::NOT_FOUND)
        );
    }
}
