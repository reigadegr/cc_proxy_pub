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

    // 修复异常的 function_call_output 元素
    let mut result_value = Value::Object(out);
    fix_malformed_function_call_outputs(&mut result_value);

    if let Some(instructions) = join_system_texts(&instructions_texts)
        && let Some(obj) = result_value.as_object_mut()
    {
        obj.insert("instructions".to_string(), Value::String(instructions));
    }

    if let Some(temperature) = object.get("temperature")
        && let Some(obj) = result_value.as_object_mut()
    {
        obj.insert("temperature".to_string(), temperature.clone());
    }
    if let Some(top_p) = object.get("top_p")
        && let Some(obj) = result_value.as_object_mut()
    {
        obj.insert("top_p".to_string(), top_p.clone());
    }

    if let Some(stop) =
        tools::map_anthropic_stop_sequences_to_openai_stop(object.get("stop_sequences"))
        && let Some(obj) = result_value.as_object_mut()
    {
        obj.insert("stop".to_string(), stop);
    }

    if let Some(tools_value) = object.get("tools")
        && let Some(obj) = result_value.as_object_mut()
    {
        obj.insert(
            "tools".to_string(),
            tools::map_anthropic_tools_to_responses(tools_value),
        );
    }

    let (tool_choice, parallel_tool_calls) =
        tools::map_anthropic_tool_choice_to_responses(object.get("tool_choice"));
    if let Some(tool_choice) = tool_choice
        && let Some(obj) = result_value.as_object_mut()
    {
        obj.insert("tool_choice".to_string(), tool_choice);
    }
    if let Some(parallel_tool_calls) = parallel_tool_calls
        && let Some(obj) = result_value.as_object_mut()
    {
        obj.insert(
            "parallel_tool_calls".to_string(),
            Value::Bool(parallel_tool_calls),
        );
    }

    serde_json::to_vec(&result_value)
        .map(Bytes::from)
        .map_err(|err| format!("Failed to serialize request: {err}"))
}

/// 修复 input 数组中异常的 `function_call_output` 元素
///
/// 检测 `function_call_output` 的 `output` 字段是否可解析为 JSON 数组
/// 如果可以，将其转换为 `type: message, role: assistant, content: [...]` 格式
///
/// 这是处理上游发送异常数据的情况：某些孤立的 `function_call_output` 的 output
/// 字段是字符串化的 JSON 数组，实际包含的是 assistant 消息内容
pub fn fix_malformed_function_call_outputs(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    let Some(input) = obj.get_mut("input").and_then(|v| v.as_array_mut()) else {
        return;
    };

    let mut fixed_count = 0;

    // 遍历并修复异常的 function_call_output 元素
    for item in input.iter_mut() {
        let Some(item_obj) = item.as_object_mut() else {
            continue;
        };

        let type_str = item_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if type_str == "function_call_output" {
            let Some(output_str) = item_obj.get("output").and_then(|v| v.as_str()) else {
                continue;
            };

            // 尝试将 output 解析为 JSON 数组
            if let Ok(mut parsed_output) = serde_json::from_str::<serde_json::Value>(output_str)
                && parsed_output.is_array()
            {
                // output 是 JSON 数组，需要转换为 message 格式
                // 移除 function_call_output 的字段
                item_obj.remove("call_id");
                item_obj.remove("output");

                // 处理 content 数组：
                // 1. 移除第二个元素（索引1）
                // 2. 将第一个元素（索引0）的 type 改为 "output_text"
                if let Some(content_array) = parsed_output.as_array_mut() {
                    // 移除索引1的元素（如果存在）
                    if content_array.len() > 1 {
                        content_array.remove(1);
                    }
                    // 修改索引0元素的 type 为 "output_text"
                    if let Some(first_item) = content_array.first_mut()
                        && let Some(first_obj) = first_item.as_object_mut() {
                            first_obj.insert("type".to_string(), json!("output_text"));
                        }
                }

                // 添加 message 字段
                item_obj.insert("type".to_string(), json!("message"));
                item_obj.insert("role".to_string(), json!("assistant"));
                item_obj.insert("content".to_string(), parsed_output);

                fixed_count += 1;
                tracing::info!("✅ 修复异常的 function_call_output: 转换为 assistant message");
            }
        }
    }

    if fixed_count > 0 {
        tracing::info!("共修复 {} 个异常的 function_call_output 元素", fixed_count);
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 测试修复异常的 `function_call_output`
    #[test]
    fn test_fix_malformed_function_call_outputs() {
        // 构造一个包含异常 function_call_output 的请求
        let mut request = json!({
            "model": "test-model",
            "max_output_tokens": 4096,
            "stream": false,
            "input": [
                {
                    "content": [{"text": "用户消息", "type": "input_text"}],
                    "role": "user",
                    "type": "message"
                },
                {
                    "call_id": "call_dfff71beedfc4e05a1ca7100",
                    "output": "[{\"text\":\"我这边先说明下：当前环境里终端命令调用被拒绝了，所以我暂时无法直接扫描你的本地仓库文件。\\n\\n你可以任选一种方式，我立刻继续做精确定位：\\n\\n- 方式 1（推荐）：把仓库根目录路径下的关键文件列表/目录结构贴给我（例如 `src/`, `app/`, `components/` 相关）\\n- 方式 2：直接把你怀疑相关的文件内容贴上来（比如包含“注释/annotation/comment/page/layout/number/index”的文件）\\n- 方式 3：你本地先跑以下命令，把结果发我（我会据此给出“所有相关文件 + 关键代码段 + 结论”）：\\n\\n```bash\\n# 1) 找候选文件\\nrg -n \\\"annotation|annot|comment|note|footnote|序号|编号|页|分页|layout|position|left|bottom|vertical|horizontal|direction\\\" src app components\\n\\n# 2) 如果是 React Native/前端项目，再补充样式方向相关\\nrg -n \\\"flexDirection|writingMode|position:\\\\\\\\s*'absolute'|left:\\\\\\\\s*|bottom:\\\\\\\\s*|transform|column|row\\\" src app components\\n```\\n\\n拿到这些结果后，我会按你关心的三点给出明确结论：\\n- 注释序号如何编号\\n- 分页是否重置编号\\n- 左下角注释是横排还是竖排（以及由哪段布局代码决定）\",\"type\":\"text\"},{\"text\":\"agentId: a883913cc85bbedd1 (for resuming to continue this agent's work if needed)\\n<usage>total_tokens: 11154\\ntool_uses: 1\\nduration_ms: 36320</usage>\",\"type\":\"text\"}]",
            "type": "function_call_output"
                },
                {
                    "call_id": "call_normal456",
                    "output": "No files found",
                    "type": "function_call_output"
                }
            ]
        });

        // 调用修复函数
        fix_malformed_function_call_outputs(&mut request);

        // 验证结果
        let input = request.get("input").and_then(|v| v.as_array()).unwrap();

        // 第一个元素应该是普通用户消息，保持不变
        assert_eq!(input[0].get("type").unwrap().as_str().unwrap(), "message");
        assert_eq!(input[0].get("role").unwrap().as_str().unwrap(), "user");

        // 第二个元素：异常的 function_call_output 应该被转换为 assistant message
        assert_eq!(input[1].get("type").unwrap().as_str().unwrap(), "message");
        assert_eq!(input[1].get("role").unwrap().as_str().unwrap(), "assistant");
        // 不应该再有 call_id 和 output 字段
        assert!(input[1].get("call_id").is_none());
        assert!(input[1].get("output").is_none());
        // content 应该是解析后的数组（第二个元素已被移除，只剩一个）
        let content = input[1].get("content").and_then(|v| v.as_array()).unwrap();
        assert_eq!(content.len(), 1);
        // 验证第一个元素的 type 已被改为 "output_text"
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "output_text"
        );
        // 验证 text 包含预期的内容
        let first_text = content[0].get("text").unwrap().as_str().unwrap();
        assert!(
            first_text.contains("我这边先说明下：当前环境里终端命令调用被拒绝了"),
            "First text should contain expected content, got: {first_text}"
        );

        // 第三个元素：正常的 function_call_output 应该保持不变
        assert_eq!(
            input[2].get("type").unwrap().as_str().unwrap(),
            "function_call_output"
        );
        assert_eq!(
            input[2].get("call_id").unwrap().as_str().unwrap(),
            "call_normal456"
        );
        assert_eq!(
            input[2].get("output").unwrap().as_str().unwrap(),
            "No files found"
        );
    }

    /// 测试不包含异常数据的情况
    #[test]
    fn test_fix_with_no_malformed_outputs() {
        let mut request = json!({
            "model": "test-model",
            "input": [
                {
                    "call_id": "call_123",
                    "output": "Normal output",
                    "type": "function_call_output"
                }
            ]
        });

        fix_malformed_function_call_outputs(&mut request);

        let input = request.get("input").and_then(|v| v.as_array()).unwrap();
        assert_eq!(
            input[0].get("type").unwrap().as_str().unwrap(),
            "function_call_output"
        );
        assert_eq!(
            input[0].get("output").unwrap().as_str().unwrap(),
            "Normal output"
        );
    }

    /// 测试空 input 数组
    #[test]
    fn test_fix_with_empty_input() {
        let mut request = json!({
            "model": "test-model",
            "input": []
        });

        fix_malformed_function_call_outputs(&mut request);

        // 不应该 panic，input 应该仍然为空
        let input = request.get("input").and_then(|v| v.as_array()).unwrap();
        assert_eq!(input.len(), 0);
    }

    /// 测试 output 是无效 JSON 的情况
    #[test]
    fn test_fix_with_invalid_json_output() {
        let mut request = json!({
            "model": "test-model",
            "input": [
                {
                    "call_id": "call_123",
                    "output": "{invalid json}",
                    "type": "function_call_output"
                }
            ]
        });

        fix_malformed_function_call_outputs(&mut request);

        // 无效 JSON 应该保持原样
        let input = request.get("input").and_then(|v| v.as_array()).unwrap();
        assert_eq!(
            input[0].get("type").unwrap().as_str().unwrap(),
            "function_call_output"
        );
        assert_eq!(
            input[0].get("output").unwrap().as_str().unwrap(),
            "{invalid json}"
        );
    }

    /// 测试 output 是 JSON 对象而非数组的情况
    #[test]
    fn test_fix_with_json_object_output() {
        let mut request = json!({
            "model": "test-model",
            "input": [
                {
                    "call_id": "call_123",
                    "output": "{\"result\":\"success\"}",
                    "type": "function_call_output"
                }
            ]
        });

        fix_malformed_function_call_outputs(&mut request);

        // JSON 对象应该保持原样（只有数组才会被转换）
        let input = request.get("input").and_then(|v| v.as_array()).unwrap();
        assert_eq!(
            input[0].get("type").unwrap().as_str().unwrap(),
            "function_call_output"
        );
        assert_eq!(
            input[0].get("output").unwrap().as_str().unwrap(),
            "{\"result\":\"success\"}"
        );
    }

    /// 测试缺少 input 字段的情况
    #[test]
    fn test_fix_without_input_field() {
        let mut request = json!({
            "model": "test-model"
        });

        // 不应该 panic
        fix_malformed_function_call_outputs(&mut request);
    }

    /// 测试包含真实场景的数据（类似于用户提供的示例）
    #[test]
    fn test_fix_real_world_example() {
        let output_array = json!([
            {"text": "我这边先说明下：当前环境里终端命令调用被拒绝了，所以我暂时无法直接扫描你的本地仓库文件。", "type": "text"},
            {"text": "agentId: a883913cc85bbedd1 (for resuming to continue this agent's work if needed)\n<usage>total_tokens: 11154\ntool_uses: 1\nduration_ms: 36320</usage>", "type": "text"}
        ]);

        let mut request = json!({
            "model": "test-model",
            "input": [
                {
                    "call_id": "call_dfff71beedfc4e05a1ca7100",
                    "output": serde_json::to_string(&output_array).unwrap(),
                    "type": "function_call_output"
                }
            ]
        });

        fix_malformed_function_call_outputs(&mut request);

        let input = request.get("input").and_then(|v| v.as_array()).unwrap();
        assert_eq!(input[0].get("type").unwrap().as_str().unwrap(), "message");
        assert_eq!(input[0].get("role").unwrap().as_str().unwrap(), "assistant");

        let content = input[0].get("content").and_then(|v| v.as_array()).unwrap();
        // 第二个元素已被移除，只剩一个元素
        assert_eq!(content.len(), 1);
        // 第一个元素的 type 应该被改为 "output_text"
        assert_eq!(
            content[0].get("type").unwrap().as_str().unwrap(),
            "output_text"
        );
        assert!(
            content[0]
                .get("text")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("终端命令调用被拒绝了")
        );
    }
}
