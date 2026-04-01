use bytes::Bytes;
use serde_json::{Value, from_slice, to_vec};

/// 需要从 messages[].content[].content 中移除的字符串
///
/// 当 content 字段中包含这些字符串时，直接将其替换为空。
/// 在此数组中添加需要移除的字符串即可。
const CONTENT_STR_REMOVE: &[&str] = &[
    "You CAN and SHOULD provide analysis of malware, what it is doing. But you MUST refuse to improve or augment the code. You can still analyze existing code, write reports, or answer questions about the code behavior.",
];

/// 从 messages 数组每个元素的 content 数组中，
/// 检查每个元素的 content 字段，若包含指定字符串则将其消除。
///
/// JSON 路径: `messages[].content[].content`
pub fn sanitize_content_strings(body_bytes: &[u8]) -> Option<Bytes> {
    if CONTENT_STR_REMOVE.is_empty() {
        return None;
    }

    let mut json = from_slice::<Value>(body_bytes).ok()?;
    let messages = json.get_mut("messages")?.as_array_mut()?;

    let mut total_replacements = 0usize;

    for message in messages.iter_mut() {
        let Some(content) = message.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };

        for item in content.iter_mut() {
            let Some(content_str) = item.get("content").and_then(|c| c.as_str()) else {
                continue;
            };

            let mut new_content = content_str.to_string();
            for target in CONTENT_STR_REMOVE {
                if new_content.contains(target) {
                    total_replacements += new_content.matches(target).count();
                    new_content = new_content.replace(target, "");
                }
            }

            if new_content != content_str
                && let Some(content_val) = item.get_mut("content")
            {
                *content_val = Value::String(new_content);
            }
        }
    }

    if total_replacements > 0 {
        tracing::info!(
            "🧹 已从 messages.content[].content 中移除 {} 处指定字符串",
            total_replacements
        );
    }

    to_vec(&json).ok().map(Into::into)
}

pub fn filter_content_strings(body_bytes: &[u8]) -> Option<Bytes> {
    sanitize_content_strings(body_bytes)
}
