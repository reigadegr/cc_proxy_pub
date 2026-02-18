use crate::Gateway;
use pingora::http::RequestHeader;
use serde_json::Value;
use std::sync::atomic::Ordering;
use tracing::{info, warn};

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
fn extract_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => content.to_string(),
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

        // 统计 tools
        if let Some(tools) = json.get("tools") {
            system_tokens += estimate_tokens(&tools.to_string());
        }

        // 统计 messages
        if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
            // 预处理所有消息，提取纯文本和角色
            let parsed_messages: Vec<(String, String, u64)> = messages
                .iter()
                .filter_map(|msg| {
                    let role = msg.get("role")?.as_str()?.to_string();
                    let content = msg.get("content")?;
                    let text = extract_text(content);
                    let tokens = estimate_tokens(&text);
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
                match role.as_str() {
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
    const CHUNK_SIZE: usize = 8000;

    let len = body.len();
    info!("=== 请求体 (共 {} 字节) ===", len);

    if len <= CHUNK_SIZE {
        info!("{}", body);
    } else {
        let total_chunks = len.div_ceil(CHUNK_SIZE);
        let mut start = 0;

        for i in 0..total_chunks {
            // 计算理论结束位置
            let mut end = (start + CHUNK_SIZE).min(len);

            // 🔑 关键修复：确保结束位置是字符边界（UTF-8 safe）
            // 如果不是字符边界，向前调整直到是边界
            while end < len && !body.is_char_boundary(end) {
                end -= 1;
            }

            // 安全切片（get 返回 Option，不会 panic）
            if let Some(chunk) = body.get(start..end) {
                info!("--- 第 {}/{} 段 ---\n{}", i + 1, total_chunks, chunk);
            } else {
                warn!("无法获取第 {}/{} 段内容", i + 1, total_chunks);
                break;
            }

            start = end;
        }
    }
    info!("=== 请求体结束 ===");
}

// 辅助函数：分段打印响应体
pub fn log_full_response(body: &str) {
    const CHUNK_SIZE: usize = 8000;

    let len = body.len();
    info!("=== 响应体 (共 {} 字节) ===", len);

    if len <= CHUNK_SIZE {
        info!("{}", body);
    } else {
        let total_chunks = len.div_ceil(CHUNK_SIZE);
        let mut start = 0;

        for i in 0..total_chunks {
            let mut end = (start + CHUNK_SIZE).min(len);

            while end < len && !body.is_char_boundary(end) {
                end -= 1;
            }

            if let Some(chunk) = body.get(start..end) {
                info!("--- 第 {}/{} 段 ---\n{}", i + 1, total_chunks, chunk);
            } else {
                warn!("无法获取第 {}/{} 段内容", i + 1, total_chunks);
                break;
            }

            start = end;
        }
    }
    info!("=== 响应体结束 ===");
}

pub fn calculate_tokens(gateway: &Gateway, body_str: &str) {
    let (total, user_new, user_hist, assistant, system) = analyze_request_body(body_str);

    gateway.total_tokens.fetch_add(total, Ordering::Relaxed);
    gateway
        .user_new_tokens
        .fetch_add(user_new, Ordering::Relaxed);
    gateway
        .user_history_tokens
        .fetch_add(user_hist, Ordering::Relaxed);
    gateway
        .assistant_tokens
        .fetch_add(assistant, Ordering::Relaxed);
    gateway.system_tokens.fetch_add(system, Ordering::Relaxed);
    let count = gateway.request_count.fetch_add(1, Ordering::Relaxed) + 1;

    let waste = user_hist + assistant + system;
    let waste_ratio = if user_new > 0 {
        waste as f64 / user_new as f64
    } else {
        0.0
    };

    info!(
        "📊 本次 | 总: {} | 你: {} | 你(历史): {} | 助手(历史): {} | 系统: {} | 浪费比: {:.1}:1",
        total, user_new, user_hist, assistant, system, waste_ratio
    );

    let total_acc = gateway.total_tokens.load(Ordering::Relaxed);
    let new_acc = gateway.user_new_tokens.load(Ordering::Relaxed);
    let hist_acc = gateway.user_history_tokens.load(Ordering::Relaxed)
        + gateway.assistant_tokens.load(Ordering::Relaxed);
    let sys_acc = gateway.system_tokens.load(Ordering::Relaxed);

    warn!(
        "🔥 累计 {} 次 | 总: {} | 你: {} | 浪费: {} (历史:{} 系统:{}) | 平均浪费比: {:.1}:1",
        count,
        total_acc,
        new_acc,
        hist_acc + sys_acc,
        hist_acc,
        sys_acc,
        if new_acc > 0 {
            (hist_acc + sys_acc) as f64 / new_acc as f64
        } else {
            0.0
        }
    );
}

/// 打印全部请求头
pub fn log_request_headers(req: &RequestHeader) {
    info!("=== 请求头 ===");
    info!("Method: {}", req.method);
    info!("URI: {}", req.uri);
    info!("Version: {:?}", req.version);

    for (name, value) in &req.headers {
        if let Ok(value_str) = value.to_str() {
            info!("{}: {}", name, value_str);
        }
    }
    info!("=== 请求头结束 ===");
}
