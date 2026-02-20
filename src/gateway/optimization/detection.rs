use serde_json::Value;

const TITLE_GENERATION_PHRASE: &str = "write a 5-10 word title";
const SUGGESTION_MODE_MARKER: &str = "[SUGGESTION MODE:";
const COMMAND_MARKER: &str = "Command:";
const OUTPUT_MARKER: &str = "Output:";

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
    text.to_lowercase().contains("quota")
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

pub fn is_title_generation_request(request: &Value) -> bool {
    let Some(messages) = get_messages(request) else {
        return false;
    };

    let Some(last_message) = messages.last() else {
        return false;
    };

    if message_role(last_message) != Some("user") {
        return false;
    }

    let text = extract_message_text(last_message);
    text.to_lowercase().contains(TITLE_GENERATION_PHRASE)
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
        .map_or_else(String::new, extract_text_from_content);
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

fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

fn extract_message_text(message: &Value) -> String {
    message
        .get("content")
        .map_or_else(String::new, extract_text_from_content)
}

fn extract_text_from_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| block.get("thinking").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_quota_check_request() {
        let request = json!({
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "check quota now"}]
        });
        assert!(is_quota_check_request(&request));

        let non_quota = json!({
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert!(!is_quota_check_request(&non_quota));
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
            "messages": [{"role": "user", "content": "Please write a 5-10 word title"}]
        });
        assert!(is_title_generation_request(&request));
    }
}
