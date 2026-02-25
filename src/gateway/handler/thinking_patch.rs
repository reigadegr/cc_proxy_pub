use serde_json::{Value, from_slice, json, to_vec};

/// 缺省的 `reasoning_content` 占位符
const REASONING_PLACEHOLDER: &str = "[Previous reasoning not available in context]";

/// 从 message.content 中提取 type=thinking 的 thinking 文本
pub fn extract_thinking_text(message: &Value) -> Option<&str> {
    message
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|content| {
            content
                .iter()
                .find(|block| block.get("type").and_then(|t| t.as_str()) == Some("thinking"))
        })
        .and_then(|block| block.get("thinking").and_then(|t| t.as_str()))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// 判断 `reasoning_content` 是否缺失或仍为占位符
fn reasoning_missing_or_placeholder(message: &Value) -> bool {
    message
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .is_none_or(|value| value == REASONING_PLACEHOLDER)
}

/// 根据 thinking 文本补丁单条消息的 `reasoning_content`
fn patch_message_reasoning_content(message: &mut Value, fallback_thinking: Option<&str>) -> bool {
    if !reasoning_missing_or_placeholder(message) {
        return false;
    }

    let reasoning_value = extract_thinking_text(message)
        .or(fallback_thinking)
        .unwrap_or(REASONING_PLACEHOLDER)
        .to_string();

    let Some(object) = message.as_object_mut() else {
        return false;
    };

    let should_update = object
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .is_none_or(|current| current != reasoning_value);

    if should_update {
        object.insert("reasoning_content".to_string(), json!(reasoning_value));
        return true;
    }

    false
}

/// 为 Kimi Thinking 模式补全缺失的 `reasoning_content`
///
/// 在 thinking 启用时：
/// - 优先从 message.content[type=thinking].thinking 提取文本
/// - 给 `assistant` 消息补上/替换 `reasoning_content`（缺失或为占位符时）
/// - 给 `messages` 最后一个元素补上/替换 `reasoning_content`（缺失或为占位符时），不区分 role
pub fn patch_reasoning_for_thinking_mode(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = from_slice::<Value>(body_bytes).ok()?;

    // 检查是否启用了 thinking 模式
    let thinking_enabled = json
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|t| t.as_str())
        == Some("enabled");

    if !thinking_enabled {
        return None;
    }

    let messages = json.get_mut("messages")?.as_array_mut()?;
    let mut patched = false;

    // 用于兜底：取最后一个可用的 thinking 文本
    let latest_thinking = messages
        .iter()
        .rev()
        .find_map(extract_thinking_text)
        .map(str::to_string);

    for message in messages.iter_mut() {
        let is_assistant = message.get("role").and_then(|r| r.as_str()) == Some("assistant");

        if !is_assistant {
            continue;
        }

        if patch_message_reasoning_content(message, latest_thinking.as_deref()) {
            patched = true;
        }
    }

    if patched {
        tracing::debug!("Patched missing reasoning_content for thinking mode messages");
        to_vec(&json).ok().map(Into::into)
    } else {
        None
    }
}
