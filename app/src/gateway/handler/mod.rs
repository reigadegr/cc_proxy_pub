// Re-export my_handler crate modules used by proxy.rs
pub use my_handler::{request, response, system_prompt, thinking_patch};

pub mod utils;

use my_config::Mode;
use salvo::prelude::*;

use crate::gateway::{
    handler::utils::setup_handler_state,
    proxy::{handle_anthropic as run_anthropic_proxy, handle_openai as run_openai_proxy},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteTarget {
    Anthropic,
    OpenAIResponses,
    OpenAIChat,
}

fn rewrite_responses_alias(req: &mut Request) {
    if req.uri().path() != "/responses" {
        return;
    }

    let rewritten = req.uri().query().map_or_else(
        || "/v1/responses".to_owned(),
        |query| format!("/v1/responses?{query}"),
    );
    let Ok(rewritten) = rewritten.parse() else {
        tracing::error!("Failed to parse rewritten /responses alias URI: {rewritten}");
        return;
    };
    *req.uri_mut() = rewritten;
}

fn classify_request_path(path: &str) -> Option<RouteTarget> {
    if path == "/v1/messages" || path.starts_with("/v1/messages/") {
        Some(RouteTarget::Anthropic)
    } else if path == "/v1/responses" {
        Some(RouteTarget::OpenAIResponses)
    } else if path == "/v1/chat/completions" {
        Some(RouteTarget::OpenAIChat)
    } else {
        None
    }
}

async fn dispatch_proxy(req: &mut Request, depot: &Depot, res: &mut Response) {
    let Some(target) = classify_request_path(req.uri().path()) else {
        tracing::info!("Rejecting unsupported proxy path: {}", req.uri().path());
        res.status_code(StatusCode::NOT_FOUND);
        return;
    };

    let (config, stats, client) = match setup_handler_state(depot) {
        Ok(state) => state,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("Failed to get dependencies from depot: {e}");
            return;
        }
    };

    match target {
        RouteTarget::Anthropic => run_anthropic_proxy(req, res, config, stats, client).await,
        RouteTarget::OpenAIResponses => {
            run_openai_proxy(req, res, config, client, Mode::OpenAIResponses).await;
        }
        RouteTarget::OpenAIChat => {
            run_openai_proxy(req, res, config, client, Mode::OpenAIChat).await;
        }
    }
}

#[endpoint]
pub async fn responses_alias_proxy(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    rewrite_responses_alias(req);
    dispatch_proxy(req, depot, res).await;
}

#[endpoint]
pub async fn unified_proxy(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    dispatch_proxy(req, depot, res).await;
}

#[cfg(test)]
mod tests {
    use salvo::{
        Request, Router, Service,
        http::{StatusCode, uri::Scheme},
        test::TestClient,
    };

    use super::{RouteTarget, classify_request_path, rewrite_responses_alias};

    fn request_from_uri(uri: &str) -> Request {
        let request = match hyper::Request::builder().uri(uri).body(bytes::Bytes::new()) {
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
    fn rewrite_responses_alias_normalizes_path_before_classification() {
        let mut req = request_from_uri("http://localhost/responses?stream=true");

        rewrite_responses_alias(&mut req);

        assert_eq!(req.uri().path(), "/v1/responses");
        assert_eq!(req.uri().query(), Some("stream=true"));
        assert_eq!(
            classify_request_path(req.uri().path()),
            Some(RouteTarget::OpenAIResponses)
        );
    }

    #[tokio::test]
    async fn route_table_only_adds_exact_responses_short_path() {
        use salvo::prelude::endpoint;
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
