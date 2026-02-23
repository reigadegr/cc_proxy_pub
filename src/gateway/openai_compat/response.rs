//! 响应格式转换
//!
//! `OpenAI` Responses API 响应 → Anthropic Claude API 响应
//!
//! 主要转换：
//! - output[] → content[]
//! - `function_call` → `tool_use`
//! - `output_text` → text
//! - `reasoning_text` → thinking

use bytes::Bytes;
use serde_json::{Map, Value, json};

/// `OpenAI` Responses 响应 → Anthropic 响应
pub fn responses_response_to_anthropic(
    body: &Bytes,
    model_hint: Option<&str>,
) -> Result<Bytes, String> {
    let raw_body_str = String::from_utf8_lossy(body);
    tracing::debug!("🔍 原始上游响应 JSON: {}", raw_body_str);

    let value: Value = serde_json::from_slice(body).map_err(|e| {
        tracing::error!("❌ JSON 解析失败: {}", e);
        "Upstream response must be JSON.".to_string()
    })?;
    let Some(object) = value.as_object() else {
        return Err("Upstream response must be a JSON object.".to_string());
    };

    let id = object
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("msg_proxy");
    tracing::debug!("📋 响应 id: {}", id);
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .or(model_hint)
        .unwrap_or("unknown");

    let usage = object
        .get("usage")
        .and_then(Value::as_object)
        .map(map_openai_usage_to_anthropic_usage);

    let output: &[Value] = object
        .get("output")
        .and_then(Value::as_array)
        .map_or(&[], |items| items.as_slice());
    tracing::debug!("📤 output 数组长度: {}", output.len());
    let mut combined_text = String::new();
    let mut thinking_text = String::new();
    let mut tool_uses = Vec::new();

    for item in output {
        let Some(item) = item.as_object() else {
            tracing::debug!("⚠️ output 项不是对象");
            continue;
        };
        let item_type = item.get("type").and_then(Value::as_str);
        tracing::debug!("📤 output 项类型: {:?}", item_type);
        match item_type {
            Some("message") => {
                if item.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for part in content {
                        let Some(part) = part.as_object() else {
                            continue;
                        };
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    combined_text.push_str(text);
                                }
                            }
                            Some("reasoning_text") => {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    thinking_text.push_str(text);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some("function_call") => {
                if let Some(tool_use) = responses_function_call_to_tool_use(item) {
                    tool_uses.push(tool_use);
                }
            }
            _ => {}
        }
    }

    let mut content = Vec::new();
    if !thinking_text.trim().is_empty() {
        content.push(json!({ "type": "thinking", "thinking": thinking_text }));
    }
    if !combined_text.trim().is_empty() || tool_uses.is_empty() {
        content.push(json!({ "type": "text", "text": combined_text }));
    }
    let has_tool_uses = !tool_uses.is_empty();
    content.extend(tool_uses);

    let finish_reason = chat_finish_reason_from_response_object(object, has_tool_uses);
    let stop_reason = anthropic_stop_reason_from_chat_finish_reason(finish_reason);

    let out = json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage.unwrap_or_else(|| json!({ "input_tokens": 0, "output_tokens": 0 }))
    });

    serde_json::to_vec(&out)
        .map(Bytes::from)
        .map_err(|err| format!("Failed to serialize response: {err}"))
}

fn responses_function_call_to_tool_use(item: &Map<String, Value>) -> Option<Value> {
    let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
    let item_id = item.get("id").and_then(Value::as_str).unwrap_or("");
    let id = if call_id.is_empty() { item_id } else { call_id };
    if id.is_empty() {
        return None;
    }
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
    let input = serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|v| v.as_object().cloned().map(Value::Object))
        .unwrap_or_else(|| json!({ "_raw": arguments }));
    Some(json!({
        "type": "tool_use",
        "id": id,
        "name": name,
        "input": input
    }))
}

fn map_openai_usage_to_anthropic_usage(usage: &Map<String, Value>) -> Value {
    let input_tokens = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens
    })
}

/// 从 `OpenAI` Responses 响应对象推断 `finish_reason`
fn chat_finish_reason_from_response_object(
    object: &Map<String, Value>,
    has_tool_uses: bool,
) -> &str {
    // 检查 status 字段
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");

    match status {
        "incomplete" => {
            // 检查是否因 max_tokens 而中断
            if let Some(error) = object.get("error").and_then(Value::as_object)
                && error.get("code").and_then(Value::as_str) == Some("max_output_tokens")
            {
                return "max_tokens";
            }
            "max_tokens"
        }
        "completed" => {
            if has_tool_uses {
                "tool_use"
            } else {
                "end_turn"
            }
        }
        _ => "end_turn",
    }
}

/// Chat `finish_reason` → Anthropic `stop_reason`
fn anthropic_stop_reason_from_chat_finish_reason(reason: &str) -> &str {
    match reason {
        "tool_use" => "tool_use",
        "max_tokens" => "max_tokens",
        "stop_sequence" => "stop_sequence",
        _ => "end_turn",
    }
}
