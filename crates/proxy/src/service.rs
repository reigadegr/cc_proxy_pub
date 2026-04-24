use std::{
    borrow::Cow,
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::Value;
use tracing::{info, warn};

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

fn estimate_tokens(text: &str) -> u64 {
    // 整数运算避免浮点精度损失: (len * 2 + 6) / 7 ≈ len / 3.5
    // 使用 checked_mul 防止溢出
    let len = text.len();
    // 在 usize 空间内计算，然后转换为 u64
    let result = len
        .checked_mul(2)
        .and_then(|x| x.checked_add(6))
        .map_or(usize::MAX, |x| x / 7);
    result as u64
}

// 从 content 字段提取实际文本（处理字符串或数组格式）
fn extract_text(content: &Value) -> Cow<'_, str> {
    match content {
        Value::String(s) => Cow::Borrowed(s.as_str()),
        Value::Array(arr) => Cow::Owned(
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => Cow::Owned(content.to_string()),
    }
}

// 检查内容是否是 Claude Code 的 system-reminder（被放在 user message 里的系统提示）
fn is_system_reminder(content: &str) -> bool {
    content.contains("<system-reminder>")
        || content.contains("The following skills are available")
        || content.contains("=== MANDATORY: META-COGNITION ROUTING ===")
        || content.contains("CRITICAL: Use for")
        || content.starts_with("You are Claude Code")
}

// 返回: (total, user_new, user_history, assistant, system)
fn analyze_request_body(body: &str) -> (u64, u64, u64, u64, u64) {
    if let Ok(json) = serde_json::from_str::<Value>(body) {
        return analyze_request_json(&json);
    }

    // JSON 解析失败，可能是二进制或非标准格式
    let user_new_tokens = estimate_tokens(body);
    (user_new_tokens, user_new_tokens, 0, 0, 0)
}

fn analyze_request_json(json: &Value) -> (u64, u64, u64, u64, u64) {
    let mut system_tokens = 0;
    let mut user_new_tokens = 0;
    let mut user_history_tokens = 0;
    let mut assistant_tokens = 0;

    if let Some(system) = json.get("system") {
        system_tokens += estimate_tokens(&system.to_string());
    }

    if let Some(instructions) = json.get("instructions") {
        system_tokens += estimate_tokens(&instructions.to_string());
    }

    if let Some(tools) = json.get("tools") {
        system_tokens += estimate_tokens(&tools.to_string());
    }

    if let Some(messages) = json.get("messages").and_then(Value::as_array) {
        let last_real_user_idx = messages.iter().enumerate().rev().find_map(|(idx, msg)| {
            let role = msg.get("role").and_then(Value::as_str)?;
            if role != "user" {
                return None;
            }

            let text = extract_text(msg.get("content")?);
            (!is_system_reminder(text.as_ref())).then_some(idx)
        });

        for (idx, msg) in messages.iter().enumerate() {
            let Some(role) = msg.get("role").and_then(Value::as_str) else {
                continue;
            };
            let Some(content) = msg.get("content") else {
                continue;
            };

            let text = extract_text(content);
            let tokens = estimate_tokens(text.as_ref());
            match role {
                "user" => {
                    if is_system_reminder(text.as_ref()) {
                        system_tokens += tokens;
                    } else if Some(idx) == last_real_user_idx {
                        user_new_tokens += tokens;
                    } else {
                        user_history_tokens += tokens;
                    }
                }
                "assistant" => assistant_tokens += tokens,
                "system" => system_tokens += tokens,
                _ => {}
            }
        }
    }

    let total = system_tokens + user_new_tokens + user_history_tokens + assistant_tokens;
    (
        total,
        user_new_tokens,
        user_history_tokens,
        assistant_tokens,
        system_tokens,
    )
}

// 辅助函数：分段打印大字符串，避免日志截断和字符边界 panic
pub fn log_full_body(body: &str) {
    let len = body.len();
    info!("=== 请求体 (共 {} 字节) ===", len);
    info!("\n{}", body);
    info!("=== 请求体结束 ===");
}

// 辅助函数：分段打印响应体
pub fn log_full_response(body: &str) {
    let len = body.len();
    info!("=== 响应体 (共 {} 字节) ===", len);
    info!("{}", body);
    info!("=== 响应体结束 ===");
}

pub fn calculate_tokens(stats: &RequestStats, body_str: &str) {
    let (total, user_new, user_hist, assistant, system) = analyze_request_body(body_str);
    update_token_stats(stats, total, user_new, user_hist, assistant, system);
}

pub fn calculate_tokens_from_json(stats: &RequestStats, request: &Value) {
    let (total, user_new, user_hist, assistant, system) = analyze_request_json(request);
    update_token_stats(stats, total, user_new, user_hist, assistant, system);
}

fn update_token_stats(
    stats: &RequestStats,
    total: u64,
    user_new: u64,
    user_hist: u64,
    assistant: u64,
    system: u64,
) {
    stats.total_tokens.fetch_add(total, Ordering::Relaxed);
    stats.user_new_tokens.fetch_add(user_new, Ordering::Relaxed);
    stats
        .user_history_tokens
        .fetch_add(user_hist, Ordering::Relaxed);
    stats
        .assistant_tokens
        .fetch_add(assistant, Ordering::Relaxed);
    stats.system_tokens.fetch_add(system, Ordering::Relaxed);
    let count = stats.request_count.fetch_add(1, Ordering::Relaxed) + 1;

    info!(
        "📊 本次 | 总: {} | 你: {} | 你(历史): {} | 助手(历史): {} | 系统: {}",
        total, user_new, user_hist, assistant, system
    );

    let total_acc = stats.total_tokens.load(Ordering::Relaxed);
    let new_acc = stats.user_new_tokens.load(Ordering::Relaxed);
    let hist_acc = stats.user_history_tokens.load(Ordering::Relaxed)
        + stats.assistant_tokens.load(Ordering::Relaxed);
    let sys_acc = stats.system_tokens.load(Ordering::Relaxed);

    warn!(
        "🔥 累计 {} 次 | 总: {} | 你: {} | 历史上下文: {} | 系统: {}",
        count, total_acc, new_acc, hist_acc, sys_acc
    );
}
