//! 媒体内容格式转换
//!
//! 图片和文档的格式转换：
//! - Claude: { type: "image", source: { type: "base64", `media_type`, data } }
//! - `OpenAI`: { type: "`input_image`", `image_url`: "data:xxx;base64,xxx" }

use serde_json::{Map, Value, json};

/// Claude 图片块 → `OpenAI` `input_image`
pub fn claude_image_block_to_input_image_part(block: &Map<String, Value>) -> Option<Value> {
    let source = block.get("source").and_then(Value::as_object)?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let media_type = source
        .get("media_type")
        .and_then(Value::as_str)
        .unwrap_or("image/png");
    let data = source.get("data").and_then(Value::as_str)?;
    Some(json!({
        "type": "input_image",
        "image_url": format!("data:{media_type};base64,{data}")
    }))
}

/// Claude 文档块 → `OpenAI` `input_file`
pub fn claude_document_block_to_input_file_part(block: &Map<String, Value>) -> Option<Value> {
    let source = block.get("source").and_then(Value::as_object)?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let media_type = source
        .get("media_type")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    let data = source.get("data").and_then(Value::as_str)?;
    Some(json!({
        "type": "input_file",
        "file_url": format!("data:{media_type};base64,{data}")
    }))
}
