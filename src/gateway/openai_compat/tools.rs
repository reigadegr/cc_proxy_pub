//! 工具定义和 `tool_choice` 格式转换
//!
//! Anthropic Messages API → `OpenAI` Responses API 的工具格式转换：
//! - Anthropic: { name, description, `input_schema` }
//! - `OpenAI`: { type: "function", function: { name, description, parameters } }

use serde_json::{Map, Value, json};

/// Anthropic tools → `OpenAI` Responses tools
pub fn map_anthropic_tools_to_responses(value: &Value) -> Value {
    let Some(tools) = value.as_array() else {
        return Value::Array(Vec::new());
    };
    let mapped = tools
        .iter()
        .filter_map(map_anthropic_tool)
        .collect::<Vec<_>>();
    Value::Array(mapped)
}

fn map_anthropic_tool(value: &Value) -> Option<Value> {
    let tool = value.as_object()?;
    let name = tool.get("name").and_then(Value::as_str)?;
    let mut out = Map::new();
    out.insert("type".to_string(), json!("function"));
    out.insert("name".to_string(), Value::String(name.to_string()));
    if let Some(description) = tool.get("description") {
        out.insert("description".to_string(), description.clone());
    }
    if let Some(input_schema) = tool.get("input_schema") {
        out.insert("parameters".to_string(), input_schema.clone());
    }
    Some(Value::Object(out))
}

/// Anthropic `tool_choice` → `OpenAI` `tool_choice`
pub fn map_anthropic_tool_choice_to_responses(
    tool_choice: Option<&Value>,
) -> (Option<Value>, Option<bool>) {
    let Some(tool_choice) = tool_choice.and_then(Value::as_object) else {
        return (None, None);
    };

    let choice_type = tool_choice
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mapped_choice = match choice_type {
        "auto" => Some(json!("auto")),
        "any" => Some(json!("required")),
        "none" => Some(json!("none")),
        "tool" => {
            let name = tool_choice
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if name.is_empty() {
                None
            } else {
                Some(json!({ "type": "function", "name": name }))
            }
        }
        _ => None,
    };

    let parallel_tool_calls = tool_choice
        .get("disable_parallel_tool_use")
        .and_then(Value::as_bool)
        .map(|disable| !disable);

    (mapped_choice, parallel_tool_calls)
}

/// Anthropic `stop_sequences` → `OpenAI` stop
pub fn map_anthropic_stop_sequences_to_openai_stop(stop: Option<&Value>) -> Option<Value> {
    let stop = stop?;
    let items = stop.as_array()?;
    match items.len() {
        0 => None,
        1 => Some(items[0].clone()),
        _ => Some(Value::Array(items.clone())),
    }
}
