use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use my_config::{AtomicConfig, Mode, UpstreamSelector};
use my_handler::request::{get_req_body, log_request_meta, make_proxy_url, override_model_in_body};
use salvo::prelude::*;

use super::{
    request::prepare_request_body,
    response::{forward_proxy_response, render_failed_upstream_response},
    service::RequestStats,
    types::{
        FailedUpstreamResponse, HttpClient, ProxyKind, ProxyPlan, RetryContext, RetryLoopResult,
        SelectedUpstream, UpstreamAttemptFailure,
    },
};

pub const fn proxy_plan_for_mode(mode: Mode) -> ProxyPlan {
    match mode {
        Mode::AnthropicDirect => ProxyPlan {
            kind: ProxyKind::Anthropic,
            upstream_mode: Mode::AnthropicDirect,
            missing_upstream_message: "No upstream configured with mode including 'anthropic'",
        },
        Mode::OpenAIResponses => ProxyPlan {
            kind: ProxyKind::OpenAI,
            upstream_mode: Mode::OpenAIResponses,
            missing_upstream_message: "No upstream configured with mode including 'openai_responses'",
        },
        Mode::OpenAIChat => ProxyPlan {
            kind: ProxyKind::OpenAI,
            upstream_mode: Mode::OpenAIChat,
            missing_upstream_message: "No upstream configured with mode including 'openai_chat'",
        },
    }
}

pub async fn handle_anthropic(
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    stats: &Arc<RequestStats>,
    client: &Arc<HttpClient>,
) {
    run_proxy(
        ProxyPlan {
            kind: ProxyKind::Anthropic,
            upstream_mode: Mode::AnthropicDirect,
            missing_upstream_message: "No upstream configured with mode including 'anthropic'",
        },
        req,
        res,
        config,
        Some(stats),
        client,
    )
    .await;
}

pub async fn handle_openai(
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    client: &Arc<HttpClient>,
    mode: Mode,
) {
    run_proxy(proxy_plan_for_mode(mode), req, res, config, None, client).await;
}

async fn run_proxy(
    plan: ProxyPlan,
    req: &mut Request,
    res: &mut Response,
    config: &Arc<AtomicConfig>,
    stats: Option<&Arc<RequestStats>>,
    client: &Arc<HttpClient>,
) {
    let body_bytes = match get_req_body(req).await {
        Ok(body) => body,
        Err(error) => {
            tracing::error!("{error}");
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let cfg = config.get();
    let request_url = req.uri().to_string();

    log_request_meta(req.method().as_str(), &request_url, req.headers());

    let body_bytes = prepare_request_body(plan, body_bytes, &request_url, &cfg, stats, res);
    let Some(body_bytes) = body_bytes else {
        return;
    };

    let Some(selector) = config.get_upstream_selector() else {
        tracing::error!("{}", plan.missing_upstream_message);
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    };

    let force_index = cfg.server.force_upstream_index.clone();
    let max_attempts = if force_index.is_empty() {
        selector.matching_count_by_mode(plan.upstream_mode)
    } else {
        10
    };
    if max_attempts == 0 {
        tracing::error!("{}", plan.missing_upstream_message);
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    }

    match try_upstreams(
        plan,
        RetryContext {
            req,
            res,
            client,
            cfg: &cfg,
            selector: selector.as_ref(),
            body_bytes: &body_bytes,
            max_attempts,
            force_upstream_index: force_index,
        },
    )
    .await
    {
        RetryLoopResult::Forwarded => {}
        RetryLoopResult::Failed(UpstreamAttemptFailure::Response(failed_response)) => {
            tracing::error!(
                "{} after exhausting {} upstream attempt(s); returning last upstream response",
                proxy_failure_label(plan.kind),
                max_attempts
            );
            render_failed_upstream_response(res, failed_response);
        }
        RetryLoopResult::Failed(UpstreamAttemptFailure::Transport(error_message)) => {
            tracing::error!(
                "{} after exhausting {} upstream attempt(s): {}",
                proxy_failure_label(plan.kind),
                max_attempts,
                error_message
            );
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render("Bad Gateway");
        }
        RetryLoopResult::NoSelection => {
            tracing::error!(
                "{}: selector returned no upstream during retry loop",
                proxy_failure_label(plan.kind)
            );
            res.status_code(StatusCode::BAD_GATEWAY);
            res.render("Bad Gateway");
        }
    }
}

async fn try_upstreams(plan: ProxyPlan, ctx: RetryContext<'_>) -> RetryLoopResult {
    let mut last_failure = None;
    let forced = !ctx.force_upstream_index.is_empty();

    for attempt in 1..=ctx.max_attempts {
        if forced && attempt > 1 {
            let backoff_secs = 2 * u64::try_from(attempt).unwrap_or(10);
            tracing::info!(
                "{}: force_upstream_index={:?} 模式，第 {} 次重试，指数退避休眠 {} 秒",
                proxy_failure_label(plan.kind),
                ctx.force_upstream_index,
                attempt,
                backoff_secs
            );
            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        }
        let Some(selected_upstream) = select_upstream(ctx.selector, plan) else {
            break;
        };

        log_selected_upstream(plan.kind, &selected_upstream, attempt, ctx.max_attempts);

        let attempt_body = apply_upstream_model(ctx.body_bytes.clone(), &selected_upstream.model);
        let (upstream_url, host) = make_proxy_url(&selected_upstream.base_url, ctx.req);

        let proxy_req = match super::request::build_proxy_request(
            ctx.req,
            &upstream_url,
            host,
            &selected_upstream.api_key,
            selected_upstream.user_agent.as_deref(),
            attempt_body,
        ) {
            Ok(request) => request,
            Err(error) => {
                tracing::error!("Failed to build proxy request: {}", error);
                ctx.res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                return RetryLoopResult::Forwarded;
            }
        };

        match ctx.client.request(proxy_req).await {
            Ok(proxy_resp) => {
                match forward_proxy_response(plan.kind, proxy_resp, ctx.res, ctx.cfg).await {
                    Ok(()) => return RetryLoopResult::Forwarded,
                    Err(UpstreamAttemptFailure::Response(failed_response)) => {
                        log_failed_upstream_response(
                            plan.kind,
                            &selected_upstream,
                            attempt,
                            ctx.max_attempts,
                            ctx.cfg.server.log_res_body,
                            &failed_response,
                            forced,
                        );
                        last_failure = Some(UpstreamAttemptFailure::Response(failed_response));
                    }
                    Err(UpstreamAttemptFailure::Transport(error_message)) => {
                        log_transport_failure(
                            plan.kind,
                            &selected_upstream,
                            attempt,
                            ctx.max_attempts,
                            &error_message,
                            forced,
                        );
                        last_failure = Some(UpstreamAttemptFailure::Transport(error_message));
                    }
                }
            }
            Err(error) => {
                let error_message = error.to_string();
                log_transport_failure(
                    plan.kind,
                    &selected_upstream,
                    attempt,
                    ctx.max_attempts,
                    &error_message,
                    forced,
                );
                last_failure = Some(UpstreamAttemptFailure::Transport(error_message));
            }
        }
    }

    last_failure.map_or_else(|| RetryLoopResult::NoSelection, RetryLoopResult::Failed)
}

fn select_upstream(selector: &UpstreamSelector, plan: ProxyPlan) -> Option<SelectedUpstream> {
    let (index, name, base_url, model, api_key, user_agent, mode) =
        selector.next_by_mode(plan.upstream_mode)?;

    Some(SelectedUpstream {
        index,
        name: name.to_owned(),
        base_url: base_url.to_owned(),
        model: model.to_owned(),
        api_key: api_key.to_owned(),
        user_agent: user_agent.map(str::to_owned),
        mode,
    })
}

fn apply_upstream_model(body_bytes: Bytes, model: &str) -> Bytes {
    if model.is_empty() || body_bytes.is_empty() {
        return body_bytes;
    }

    override_model_in_body(&body_bytes, model).unwrap_or(body_bytes)
}

pub const fn proxy_failure_label(kind: ProxyKind) -> &'static str {
    match kind {
        ProxyKind::Anthropic => "Proxy request failed",
        ProxyKind::OpenAI => "OpenAI proxy request failed",
    }
}

fn log_selected_upstream(
    kind: ProxyKind,
    upstream: &SelectedUpstream,
    attempt: usize,
    total_attempts: usize,
) {
    let prefix = match kind {
        ProxyKind::Anthropic => "🔄 选中的",
        ProxyKind::OpenAI => "🔄 OpenAI 代理选中的",
    };

    tracing::info!(
        "{} Upstream[{}] name={} (attempt {}/{}): base_url={}, model={}, api_key: {}***, mode={:?}",
        prefix,
        upstream.index,
        if upstream.name.is_empty() {
            "-"
        } else {
            upstream.name.as_str()
        },
        attempt,
        total_attempts,
        upstream.base_url,
        upstream.model,
        upstream.api_key.chars().take(8).collect::<String>(),
        upstream.mode
    );
}

fn log_failed_upstream_response(
    kind: ProxyKind,
    upstream: &SelectedUpstream,
    attempt: usize,
    total_attempts: usize,
    log_response_body: bool,
    failed_response: &FailedUpstreamResponse,
    forced: bool,
) {
    let retry_hint = if forced {
        "重试"
    } else {
        "重试下一个 upstream"
    };
    if attempt < total_attempts {
        if log_response_body {
            let body = if failed_response.body_text.is_empty() {
                "<empty body>"
            } else {
                failed_response.body_text.as_str()
            };
            tracing::warn!(
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, {}; base_url={}, model={}, body={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                retry_hint,
                upstream.base_url,
                upstream.model,
                body
            );
        } else {
            tracing::warn!(
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, {}; base_url={}, model={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                retry_hint,
                upstream.base_url,
                upstream.model
            );
        }
    } else {
        if log_response_body {
            let body = if failed_response.body_text.is_empty() {
                "<empty body>"
            } else {
                failed_response.body_text.as_str()
            };
            tracing::error!(
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, no upstream left; base_url={}, model={}, body={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                upstream.base_url,
                upstream.model,
                body
            );
        } else {
            tracing::error!(
                "{}: upstream[{}] name={} attempt {}/{} returned status {}, no upstream left; base_url={}, model={}",
                proxy_failure_label(kind),
                upstream.index,
                if upstream.name.is_empty() {
                    "-"
                } else {
                    upstream.name.as_str()
                },
                attempt,
                total_attempts,
                failed_response.status,
                upstream.base_url,
                upstream.model
            );
        }
    }
}

fn log_transport_failure(
    kind: ProxyKind,
    upstream: &SelectedUpstream,
    attempt: usize,
    total_attempts: usize,
    error_message: &str,
    forced: bool,
) {
    let retry_hint = if forced {
        "重试"
    } else {
        "重试下一个 upstream"
    };
    if attempt < total_attempts {
        tracing::warn!(
            "{}: upstream[{}] name={} attempt {}/{} transport error, {}; base_url={}, model={}, error={}",
            proxy_failure_label(kind),
            upstream.index,
            if upstream.name.is_empty() {
                "-"
            } else {
                upstream.name.as_str()
            },
            attempt,
            total_attempts,
            retry_hint,
            upstream.base_url,
            upstream.model,
            error_message
        );
    } else {
        tracing::error!(
            "{}: upstream[{}] name={} attempt {}/{} transport error, no upstream left; base_url={}, model={}, error={}",
            proxy_failure_label(kind),
            upstream.index,
            if upstream.name.is_empty() {
                "-"
            } else {
                upstream.name.as_str()
            },
            attempt,
            total_attempts,
            upstream.base_url,
            upstream.model,
            error_message
        );
    }
}
