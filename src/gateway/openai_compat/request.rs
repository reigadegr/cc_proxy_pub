//! 请求格式转换
//!
//! Anthropic Claude 请求 → `OpenAI` Responses 请求
//!
//! 主要转换：
//! - system → instructions
//! - messages[] → input[]
//! - `tool_use` → `function_call`
//! - `tool_result` → `function_call_output`
//! - `max_tokens` → `max_output_tokens`

use std::borrow::Cow;

use bytes::Bytes;
use rayon::prelude::*;
use serde_json::{Map, Value, json};

use super::media;
use super::tools;

/// Anthropic Claude 请求 → `OpenAI` Responses 请求
pub fn anthropic_request_to_responses(body: &Bytes) -> Result<Bytes, String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "Request body must be JSON.".to_string())?;
    let Some(object) = value.as_object() else {
        return Err("Request body must be a JSON object.".to_string());
    };

    let model = object
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| "Request must include model.".to_string())?;

    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let max_output_tokens = object
        .get("max_tokens")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(4096);

    let mut instructions_texts = Vec::new();
    if let Some(system) = object.get("system")
        && let Some(text) = claude_system_to_text(system)
        && !text.trim().is_empty()
    {
        instructions_texts.push(text);
    }

    let Some(messages) = object.get("messages").and_then(Value::as_array) else {
        return Err("Request must include messages.".to_string());
    };
    let per_message_items: Vec<Vec<Value>> = messages
        .par_iter()
        .map(claude_message_to_responses_input_items)
        .collect();
    let input_items: Vec<Value> = per_message_items.into_iter().flatten().collect();

    let mut out = Map::new();
    out.insert("model".to_string(), Value::String(model.to_string()));
    out.insert(
        "max_output_tokens".to_string(),
        Value::Number(max_output_tokens.into()),
    );
    out.insert("stream".to_string(), Value::Bool(stream));
    out.insert("input".to_string(), Value::Array(input_items));

    if let Some(instructions) = join_system_texts(&instructions_texts) {
        out.insert("instructions".to_string(), Value::String(instructions));
    }

    if let Some(temperature) = object.get("temperature") {
        out.insert("temperature".to_string(), temperature.clone());
    }
    if let Some(top_p) = object.get("top_p") {
        out.insert("top_p".to_string(), top_p.clone());
    }

    if let Some(stop) =
        tools::map_anthropic_stop_sequences_to_openai_stop(object.get("stop_sequences"))
    {
        out.insert("stop".to_string(), stop);
    }

    if let Some(tools_value) = object.get("tools") {
        out.insert(
            "tools".to_string(),
            tools::map_anthropic_tools_to_responses(tools_value),
        );
    }

    let (tool_choice, parallel_tool_calls) =
        tools::map_anthropic_tool_choice_to_responses(object.get("tool_choice"));
    if let Some(tool_choice) = tool_choice {
        out.insert("tool_choice".to_string(), tool_choice);
    }
    if let Some(parallel_tool_calls) = parallel_tool_calls {
        out.insert(
            "parallel_tool_calls".to_string(),
            Value::Bool(parallel_tool_calls),
        );
    }

    serde_json::to_vec(&Value::Object(out))
        .map(Bytes::from)
        .map_err(|err| format!("Failed to serialize request: {err}"))
}

fn claude_message_to_responses_input_items(message: &Value) -> Vec<Value> {
    let mut input_items = Vec::new();

    let Some(message) = message.as_object() else {
        return input_items;
    };
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    if role == "system" {
        return input_items;
    }

    let content = message.get("content");
    let blocks = claude_content_to_blocks(content);

    let mut message_parts = Vec::new();
    let text_part_type = match role {
        // OpenAI Responses schema expects assistant messages in `input` to use output types.
        // This avoids errors like: "Invalid value: 'input_text'. Supported values are: 'output_text' and 'refusal'."
        "assistant" => "output_text",
        _ => "input_text",
    };
    for block in &blocks {
        let Some(block) = block.as_object() else {
            continue;
        };
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    message_parts.push(json!({ "type": text_part_type, "text": text }));
                }
            }
            "image" => {
                if let Some(part) = media::claude_image_block_to_input_image_part(block) {
                    message_parts.push(part);
                }
            }
            "document" => {
                if let Some(part) = media::claude_document_block_to_input_file_part(block) {
                    message_parts.push(part);
                }
            }
            _ => {}
        }
    }
    if !message_parts.is_empty() {
        input_items.push(json!({
            "type": "message",
            "role": role,
            "content": message_parts
        }));
    }

    for block in blocks {
        let Some(block) = block.as_object() else {
            continue;
        };
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "tool_use" => {
                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("call_proxy");
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                input_items.push(json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments
                }));
            }
            "tool_result" => {
                let call_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let output_text: Cow<'_, str> = match block.get("content") {
                    Some(Value::String(text)) => Cow::Borrowed(text.as_str()),
                    Some(other) => Cow::Owned(serde_json::to_string(other).unwrap_or_default()),
                    None => Cow::Borrowed(""),
                };
                let is_error = block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                let mut item = Map::new();
                item.insert("type".to_string(), json!("function_call_output"));
                item.insert("call_id".to_string(), Value::String(call_id.to_string()));

                // 注意：上游 OpenAI Responses API 不支持 is_error 字段
                // 如果有错误，将错误信息包装在 output 文本中
                let final_output = if is_error && !output_text.is_empty() {
                    format!("[ERROR] {output_text}")
                } else {
                    output_text.into_owned()
                };
                item.insert("output".to_string(), Value::String(final_output));
                input_items.push(Value::Object(item));
            }
            _ => {}
        }
    }

    input_items
}

fn claude_system_to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let texts = items
                .iter()
                .filter_map(|item| item.as_object())
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>();
            join_system_texts(&texts)
        }
        _ => None,
    }
}

fn join_system_texts(texts: &[String]) -> Option<String> {
    let combined = texts
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn claude_content_to_blocks(content: Option<&Value>) -> Vec<Value> {
    let Some(content) = content else {
        return Vec::new();
    };
    match content {
        Value::String(text) => vec![json!({ "type": "text", "text": text })],
        Value::Array(items) => items
            .iter()
            .cloned()
            .map(|mut item| {
                normalize_text_block_in_place(&mut item);
                item
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn normalize_text_block_in_place(block: &mut Value) {
    let Some(object) = block.as_object_mut() else {
        return;
    };
    let block_type = object.get("type").and_then(Value::as_str).unwrap_or("");
    if block_type != "text" {
        return;
    }
    let text_value = object.get("text");
    let new_text = text_value.and_then(extract_text_value);
    if let Some(new_text) = new_text {
        if matches!(
            (&new_text, text_value),
            (Cow::Borrowed(_), Some(Value::String(_)))
        ) {
            return;
        }
        object.insert("text".to_string(), Value::String(new_text.into_owned()));
        return;
    }
    // If text exists but is not convertible, coerce to empty string to satisfy schema.
    if text_value.is_some() {
        object.insert("text".to_string(), Value::String(String::new()));
    }
}

fn extract_text_value(value: &Value) -> Option<Cow<'_, str>> {
    match value {
        Value::String(text) => Some(Cow::Borrowed(text.as_str())),
        Value::Object(object) => {
            if let Some(text) = object.get("text") {
                return extract_text_value(text);
            }
            if let Some(text) = object.get("value") {
                return extract_text_value(text);
            }
            None
        }
        _ => None,
    }
}
