pub mod content_filter;
pub mod content_tag;
pub mod request;
pub mod response;
pub mod system_prompt;
pub mod thinking_patch;
pub mod tool_desc;
pub mod utils;

use salvo::prelude::*;

use crate::gateway::{
    handler::utils::setup_handler_state,
    proxy::{handle_claude as run_claude_proxy, handle_codex as run_codex_proxy},
};

#[handler]
pub async fn claude_proxy(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let (config, stats, client) = match setup_handler_state(depot) {
        Ok(state) => state,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("Failed to get dependencies from depot: {e}");
            return;
        }
    };

    run_claude_proxy(req, res, config, stats, client).await;
}

#[handler]
pub async fn codex_proxy(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let (config, _, client) = match setup_handler_state(depot) {
        Ok(state) => state,
        Err(e) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            tracing::error!("Failed to get dependencies from depot: {e}");
            return;
        }
    };

    run_codex_proxy(req, res, config, client).await;
}
