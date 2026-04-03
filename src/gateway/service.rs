use std::{borrow::Cow, sync::atomic::Ordering};

use rayon::prelude::*;
use serde_json::Value;
use tracing::{info, warn};

use crate::gateway::RequestStats;

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
pub fn analyze_request_body(body: &str) -> (u64, u64, u64, u64, u64) {
    let mut system_tokens = 0;
    let mut user_new_tokens = 0;
    let mut user_history_tokens = 0;
    let mut assistant_tokens = 0;

    if let Ok(json) = serde_json::from_str::<Value>(body) {
        // 统计独立的 system 字段
        if let Some(system) = json.get("system") {
            system_tokens += estimate_tokens(&system.to_string());
        }

        // 统计 OpenAI 格式的 instructions 字段
        if let Some(instructions) = json.get("instructions") {
            system_tokens += estimate_tokens(&instructions.to_string());
        }

        // 统计 tools
        if let Some(tools) = json.get("tools") {
            system_tokens += estimate_tokens(&tools.to_string());
        }

        // 统计 messages
        if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
            // 预处理所有消息，提取纯文本和角色
            let parsed_messages: Vec<(Cow<'_, str>, Cow<'_, str>, u64)> = messages
                .par_iter()
                .filter_map(|msg| {
                    let role = Cow::Borrowed(msg.get("role")?.as_str()?);
                    let content = msg.get("content")?;
                    let text = extract_text(content);
                    let tokens = estimate_tokens(text.as_ref());
                    Some((role, text, tokens))
                })
                .collect();

            // 找到最后一条真正的 user 消息（排除 system-reminder）
            let last_real_user_idx = parsed_messages
                .iter()
                .enumerate()
                .rev()
                .find(|(_, (role, text, _))| role == "user" && !is_system_reminder(text))
                .map(|(idx, _)| idx);

            for (idx, (role, text, tokens)) in parsed_messages.iter().enumerate() {
                match role.as_ref() {
                    "user" => {
                        if is_system_reminder(text) {
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
    } else {
        // JSON 解析失败，可能是二进制或非标准格式
        user_new_tokens = estimate_tokens(body);
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
