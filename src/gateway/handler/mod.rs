pub mod content_filter;
pub mod content_tag;
pub mod request;
pub mod response;
pub mod system_prompt;
pub mod thinking_patch;
pub mod tool_desc;
pub mod utils;

use salvo::prelude::*;

use crate::{
    config::Mode,
    gateway::{
        handler::utils::setup_handler_state,
        proxy::{handle_anthropic as run_anthropic_proxy, handle_openai as run_openai_proxy},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteTarget {
    Anthropic,
    OpenAIResponses,
    OpenAIChat,
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

#[handler]
pub async fn unified_proxy(req: &mut Request, depot: &mut Depot, res: &mut Response) {
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

#[cfg(test)]
mod tests {
    use super::{RouteTarget, classify_request_path};

    #[test]
    fn classify_request_path_maps_supported_roots() {
        assert_eq!(
            classify_request_path("/v1/messages"),
            Some(RouteTarget::Anthropic)
        );
        assert_eq!(
            classify_request_path("/v1/messages/count_tokens"),
            Some(RouteTarget::Anthropic)
        );
        assert_eq!(
            classify_request_path("/v1/responses"),
            Some(RouteTarget::OpenAIResponses)
        );
        assert_eq!(
            classify_request_path("/v1/chat/completions"),
            Some(RouteTarget::OpenAIChat)
        );
    }

    #[test]
    fn classify_request_path_rejects_unsupported_roots() {
        assert_eq!(classify_request_path("/claude/messages"), None);
        assert_eq!(classify_request_path("/codex/responses"), None);
        assert_eq!(classify_request_path("/foo"), None);
    }
}
