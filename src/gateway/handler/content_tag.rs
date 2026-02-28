use serde_json::{Value, from_slice, to_vec};

/// 需要从 messages[].content[] 中移除的标签（成对匹配）
const CONTENT_TAG_FILTERS: &[(&str, &str)] = &[
    ("<system-reminder>", "</system-reminder>"),
    ("<local-command-stdout>", "</local-command-stdout>"),
    ("<command-name>", "</command-name>"),
    ("<local-command-caveat>", "</local-command-caveat>"),
    ("<command-name>", "</command-args>"),
];

/// 需要强制将 `is_error` 改为 false 的错误内容前缀
const ERROR_OVERRIDE_PREFIX: &str = "EACCES: permission denied, mkdir '/tmp";

/// 检查文本是否应该从 content 中移除
fn should_remove_content(text: &str) -> bool {
    let trimmed = text.trim();
    for (start, end) in CONTENT_TAG_FILTERS {
        if trimmed.starts_with(start) && trimmed.ends_with(end) {
            return true;
        }
    }
    false
}

/// 过滤 messages[].content[] 数组，移除无用标签内容
///
/// Claude CLI 发送的请求中，content 数组可能包含大量无用的标签内容：
/// - <system-reminder>...</system-reminder>
/// - <local-command-stdout>...</local-command-stdout>
/// - <command-name>...</command-name>
/// - <local-command-caveat>...</local-command-caveat>
///
/// 这些内容占用大量 tokens 但对模型无用，此函数将其移除。
pub fn filter_messages_content(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = from_slice::<Value>(body_bytes).ok()?;

    let messages = json.get_mut("messages")?.as_array_mut()?;

    let mut total_removed = 0usize;
    let mut total_chars = 0usize;

    for message in messages.iter_mut() {
        let Some(content) = message.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };

        // 统计移除前的信息
        for item in content.iter() {
            if let Some(text) = item.get("text").and_then(|t| t.as_str())
                && should_remove_content(text)
            {
                total_removed += 1;
                total_chars += text.len();
            }
        }

        // 过滤掉需要移除的内容
        content.retain(|item| {
            item.get("text")
                .and_then(|t| t.as_str())
                .is_none_or(|text| !should_remove_content(text))
        });
    }

    if total_removed > 0 {
        tracing::info!(
            "🧹 已过滤 messages.content: 移除 {} 项, 节省约 {} 字符 (~{} tokens)",
            total_removed,
            total_chars,
            total_chars / 4
        );
    }

    to_vec(&json).ok().map(Into::into)
}

/// 覆盖特定错误的 `is_error` 字段
///
/// 当 role 为 "user" 的消息中，content 数组包含特定权限错误时，
/// 强制将 `is_error` 改为 false，避免触发不必要的错误处理。
pub fn override_permission_error(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = from_slice::<Value>(body_bytes).ok()?;

    let messages = json.get_mut("messages")?.as_array_mut()?;

    let mut overridden_count = 0usize;

    for message in messages.iter_mut() {
        // 只处理 role 为 "user" 的消息
        let Some(role) = message.get("role").and_then(|r| r.as_str()) else {
            continue;
        };

        if role != "user" {
            continue;
        }

        let Some(content) = message.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };

        // 检查 content 数组中的每个元素
        for item in content.iter_mut() {
            // 检查是否有 content 字段包含特定错误前缀
            let has_permission_error = item
                .get("content")
                .and_then(|c| c.as_str())
                .is_some_and(|content| content.starts_with(ERROR_OVERRIDE_PREFIX));

            if has_permission_error {
                // 将 is_error 强制改为 false
                if let Some(is_error) = item.get_mut("is_error") {
                    *is_error = Value::Bool(false);
                    overridden_count += 1;
                }
            }
        }
    }

    if overridden_count > 0 {
        tracing::info!(
            "🔧 已覆盖 {} 个 permission denied 错误的 is_error 为 false",
            overridden_count
        );
    }

    to_vec(&json).ok().map(Into::into)
}
