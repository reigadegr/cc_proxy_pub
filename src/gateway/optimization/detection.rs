use std::borrow::Cow;

use serde_json::Value;

const HISTORY_ANALYSIS_PARSE: &str = "You are an expert at analyzing git history.";
const TITLE_GENERATION_PHRASE: &str = "Analyze if this message indicates a new conversation topic.";
const SUGGESTION_MODE_MARKER: &str = "[SUGGESTION MODE:";
const COMMAND_MARKER: &str = "Command:";
const OUTPUT_MARKER: &str = "Output:";

pub fn is_count_tokens_url(url: &str) -> bool {
    url.to_ascii_lowercase().contains("count_tokens")
}

pub fn is_quota_check_request(request: &Value) -> bool {
    if request.get("max_tokens").and_then(Value::as_i64) != Some(1) {
        return false;
    }

    let Some(messages) = get_messages(request) else {
        return false;
    };
    if messages.len() != 1 || message_role(&messages[0]) != Some("user") {
        return false;
    }

    let text = extract_message_text(&messages[0]);
    text.to_lowercase().contains("count")
}

pub fn detect_prefix_command(request: &Value) -> Option<String> {
    let messages = get_messages(request)?;
    if messages.len() != 1 || message_role(&messages[0]) != Some("user") {
        return None;
    }

    let content = extract_message_text(&messages[0]);
    if !content.contains("<policy_spec>") || !content.contains(COMMAND_MARKER) {
        return None;
    }

    let start = content.rfind(COMMAND_MARKER)? + COMMAND_MARKER.len();
    Some(content[start..].trim().to_owned())
}

pub fn is_historical_analysis_request(request: &Value) -> bool {
    let Some(system) = get_system(request) else {
        return false;
    };

    let Some(last_system) = system.last() else {
        return false;
    };

    let text = extract_system_text(last_system);
    text.contains(HISTORY_ANALYSIS_PARSE)
}

pub fn is_title_generation_request(request: &Value) -> bool {
    let Some(system) = get_system(request) else {
        return false;
    };

    let Some(last_system) = system.last() else {
        return false;
    };

    let text = extract_system_text(last_system);
    text.contains(TITLE_GENERATION_PHRASE)
}

pub fn is_suggestion_mode_request(request: &Value) -> bool {
    let Some(messages) = get_messages(request) else {
        return false;
    };

    messages.iter().any(|message| {
        message_role(message) == Some("user")
            && extract_message_text(message).contains(SUGGESTION_MODE_MARKER)
    })
}

pub fn detect_filepath_extraction_request(request: &Value) -> Option<(String, String)> {
    let messages = get_messages(request)?;
    if messages.len() != 1 || message_role(&messages[0]) != Some("user") {
        return None;
    }

    if request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        return None;
    }

    let content = extract_message_text(&messages[0]);
    if !content.contains(COMMAND_MARKER) || !content.contains(OUTPUT_MARKER) {
        return None;
    }

    let content_lower = content.to_lowercase();
    let user_has_filepaths =
        content_lower.contains("filepaths") || content_lower.contains("<filepaths>");

    let system_text = request
        .get("system")
        .map_or(Cow::Borrowed(""), extract_text_from_content);
    let system_text_lower = system_text.to_lowercase();
    let system_has_extract = system_text_lower.contains("extract any file paths")
        || system_text_lower.contains("file paths that this command");

    if !user_has_filepaths && !system_has_extract {
        return None;
    }

    let command_start = content.find(COMMAND_MARKER)? + COMMAND_MARKER.len();
    let output_marker = content[command_start..].find(OUTPUT_MARKER)? + command_start;

    let command = content[command_start..output_marker].trim().to_owned();
    let mut output = content[output_marker + OUTPUT_MARKER.len()..]
        .trim()
        .to_owned();

    for marker in ["<", "\n\n"] {
        if let Some(index) = output.find(marker) {
            output = output[..index].trim().to_owned();
        }
    }

    Some((command, output))
}

fn get_messages(request: &Value) -> Option<&[Value]> {
    request.get("messages")?.as_array().map(Vec::as_slice)
}

fn get_system(request: &Value) -> Option<&[Value]> {
    request.get("system")?.as_array().map(Vec::as_slice)
}

fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

fn extract_message_text(message: &Value) -> Cow<'_, str> {
    message
        .get("content")
        .map_or(Cow::Borrowed(""), extract_text_from_content)
}

fn extract_system_text(message: &Value) -> Cow<'_, str> {
    message
        .get("text")
        .map_or(Cow::Borrowed(""), extract_text_from_content)
}

fn extract_text_from_content(content: &Value) -> Cow<'_, str> {
    match content {
        Value::String(text) => Cow::Borrowed(text.as_str()),
        Value::Array(blocks) => Cow::Owned(
            blocks
                .iter()
                .filter_map(|block| {
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .or_else(|| block.get("thinking").and_then(Value::as_str))
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => Cow::Borrowed(""),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_quota_check_request() {
        let request = json!({
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "count"}]
        });
        assert!(is_quota_check_request(&request));

        let non_quota = json!({
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert!(!is_quota_check_request(&non_quota));
    }

    #[test]
    fn test_count_tokens_url_detection() {
        assert!(is_count_tokens_url("/v1/messages/count_tokens"));
        assert!(is_count_tokens_url("/api?route=count_tokens"));
        assert!(!is_count_tokens_url("/v1/messages"));
    }

    #[test]
    fn test_prefix_command_detection() {
        let request = json!({
            "messages": [{
                "role": "user",
                "content": "<policy_spec>abc</policy_spec>\nCommand: git commit -m test"
            }]
        });

        assert_eq!(
            detect_prefix_command(&request),
            Some(String::from("git commit -m test"))
        );
    }

    #[test]
    fn test_filepath_extraction_detected_by_system_prompt() {
        let request = json!({
            "messages": [{"role": "user", "content": "Command: ls\nOutput: src\nCargo.toml"}],
            "system": "Extract any file paths that this command reads or modifies."
        });

        let result = detect_filepath_extraction_request(&request);
        assert_eq!(
            result,
            Some((String::from("ls"), String::from("src\nCargo.toml")))
        );
    }

    #[test]
    fn test_suggestion_mode_detection() {
        let request = json!({
            "messages": [
                {"role": "assistant", "content": "ignore"},
                {"role": "user", "content": "hello\n[SUGGESTION MODE: on]\n"}
            ]
        });

        assert!(is_suggestion_mode_request(&request));
    }

    #[test]
    fn test_title_generation_detection() {
        let request = json!({
            "system": [
                {
                    "text": "x-anthropic-billing-header: cc_version=2.1.50.ae0; cc_entrypoint=cli; cch=00000;",
                    "type": "text"
                },
                {
                    "text": "Analyze if this message indicates a new conversation topic. If it does, extract a 2-3 word title that captures the new topic. Format your response as a JSON object with two fields: 'isNewTopic' (boolean) and 'title' (string, or null if isNewTopic is false).",
                    "type": "text"
                }
            ]
        });
        assert!(is_title_generation_request(&request));
    }
}
