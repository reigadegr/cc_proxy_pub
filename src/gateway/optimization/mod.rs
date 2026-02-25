mod command_utils;
mod detection;
mod response_builder;

pub use response_builder::OptimizationResponse;
use serde_json::Value;

use crate::config::OptimizationConfig;

pub fn try_local_optimization(
    body_bytes: &[u8],
    request_url: &str,
    flags: &OptimizationConfig,
) -> Option<OptimizationResponse> {
    if flags.enable_network_probe_mock && detection::is_count_tokens_url(request_url) {
        tracing::info!("Optimization: Intercepted count_tokens URL");
        return response_builder::build_text_response(
            "unknown-model",
            "Max tokens passed.",
            10,
            5,
            "max_tokens_mock",
        );
    }

    let request: Value = serde_json::from_slice(body_bytes).ok()?;

    if flags.enable_network_probe_mock && detection::is_quota_check_request(&request) {
        tracing::info!("Optimization: Intercepted and mocked quota probe");
        return response_builder::build_text_response(
            "unknown-model",
            "Quota check passed.",
            10,
            5,
            "quota_probe_mock",
        );
    }

    if flags.enable_historical_analysis_mock && detection::is_historical_analysis_request(&request)
    {
        tracing::info!("Optimization: Skipped historical analysis request");
        return response_builder::build_text_response(
            "unknown-model",
            "historical analysis passed.",
            100,
            5,
            "historical_analysis_skip",
        );
    }

    if flags.enable_fast_prefix_detection
        && let Some(command) = detection::detect_prefix_command(&request)
    {
        tracing::info!("Optimization: Handled fast prefix detection");
        let prefix = command_utils::extract_command_prefix(command.as_str());
        return response_builder::build_text_response(
            "unknown-model",
            prefix.as_str(),
            100,
            5,
            "fast_prefix_detection",
        );
    }

    if flags.enable_title_generation_skip && detection::is_title_generation_request(&request) {
        tracing::info!("Optimization: Skipped title generation request");
        return response_builder::build_text_response(
            "unknown-model",
            "Conversation",
            100,
            5,
            "title_generation_skip",
        );
    }

    if flags.enable_suggestion_mode_skip && detection::is_suggestion_mode_request(&request) {
        tracing::info!("Optimization: Skipped suggestion mode request");
        return response_builder::build_text_response(
            "unknown-model",
            "",
            100,
            1,
            "suggestion_mode_skip",
        );
    }

    if flags.enable_filepath_extraction_mock
        && let Some((command, output)) = detection::detect_filepath_extraction_request(&request)
    {
        tracing::info!("Optimization: Mocked filepath extraction request");
        let filepaths =
            command_utils::extract_filepaths_from_command(command.as_str(), output.as_str());

        return response_builder::build_text_response(
            "unknown-model",
            filepaths.as_str(),
            100,
            10,
            "filepath_extraction_mock",
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::try_local_optimization;
    use crate::config::OptimizationConfig;

    fn to_json_bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap_or_default()
    }

    fn require_optimization_response(
        response: Option<super::OptimizationResponse>,
        reason: &str,
    ) -> super::OptimizationResponse {
        let Some(response) = response else {
            panic!("{reason}");
        };
        response
    }

    fn get_text_from_optimization_response(response_body: &[u8]) -> String {
        let payload: Value = serde_json::from_slice(response_body).unwrap_or_default();
        payload
            .get("content")
            .and_then(Value::as_array)
            .and_then(|content| content.first())
            .and_then(|block| block.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn test_quota_probe_mock_hit() {
        let request = json!({
            "model": "claude-test",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "count"}]
        });
        let body = to_json_bytes(&request);

        let response = require_optimization_response(
            try_local_optimization(&body, "/v1/messages", &OptimizationConfig::default()),
            "quota probe should hit",
        );

        assert_eq!(response.reason, "quota_probe_mock");
        assert_eq!(
            get_text_from_optimization_response(&response.body),
            "Quota check passed."
        );
    }

    #[test]
    fn test_prefix_detection_hit() {
        let request = json!({
            "model": "claude-test",
            "messages": [{
                "role": "user",
                "content": "<policy_spec>strict</policy_spec>\nCommand: git commit -m 'feat'"
            }]
        });
        let body = to_json_bytes(&request);

        let response = require_optimization_response(
            try_local_optimization(&body, "/v1/messages", &OptimizationConfig::default()),
            "prefix optimization should hit",
        );

        assert_eq!(response.reason, "fast_prefix_detection");
        assert_eq!(
            get_text_from_optimization_response(&response.body),
            "git commit"
        );
    }

    #[test]
    fn test_title_generation_skip_hit() {
        let request = json!({
            "system": [{
                "text": "Analyze if this message indicates a new conversation topic.",
                "type": "text"
            }]
        });
        let body = to_json_bytes(&request);

        let response = require_optimization_response(
            try_local_optimization(&body, "/v1/messages", &OptimizationConfig::default()),
            "title optimization should hit",
        );

        assert_eq!(response.reason, "title_generation_skip");
        assert_eq!(
            get_text_from_optimization_response(&response.body),
            "Conversation"
        );
    }

    #[test]
    fn test_suggestion_mode_skip_hit() {
        let request = json!({
            "messages": [{"role": "user", "content": "hi\n[SUGGESTION MODE: on]"}]
        });
        let body = to_json_bytes(&request);

        let response = require_optimization_response(
            try_local_optimization(&body, "/v1/messages", &OptimizationConfig::default()),
            "suggestion optimization should hit",
        );

        assert_eq!(response.reason, "suggestion_mode_skip");
        assert_eq!(get_text_from_optimization_response(&response.body), "");
    }

    #[test]
    fn test_filepath_extraction_mock_hit() {
        let request = json!({
            "messages": [{
                "role": "user",
                "content": "Command: cat foo.txt bar.md\nOutput: line1\nline2\n\nPlease extract <filepaths>."
            }]
        });
        let body = to_json_bytes(&request);

        let response = require_optimization_response(
            try_local_optimization(&body, "/v1/messages", &OptimizationConfig::default()),
            "filepath optimization should hit",
        );

        assert_eq!(response.reason, "filepath_extraction_mock");
        assert_eq!(
            get_text_from_optimization_response(&response.body),
            "<filepaths>\nfoo.txt\nbar.md\n</filepaths>"
        );
    }

    #[test]
    fn test_non_optimization_request_returns_none() {
        let request = json!({
            "messages": [{"role": "user", "content": "normal chat message"}]
        });
        let body = to_json_bytes(&request);

        let response =
            try_local_optimization(&body, "/v1/messages", &OptimizationConfig::default());
        assert!(response.is_none());
    }

    #[test]
    fn test_count_tokens_url_hit() {
        let request = json!({"model": "claude-test"});
        let body = to_json_bytes(&request);

        let response = require_optimization_response(
            try_local_optimization(
                &body,
                "/v1/messages/count_tokens?foo=bar",
                &OptimizationConfig::default(),
            ),
            "count_tokens url should hit",
        );

        assert_eq!(response.reason, "max_tokens_mock");
        assert_eq!(
            get_text_from_optimization_response(&response.body),
            "Max tokens passed."
        );
    }

    #[test]
    fn test_count_tokens_url_hit_with_invalid_json_body() {
        let body = b"not json";

        let response = require_optimization_response(
            try_local_optimization(
                body,
                "/v1/messages/count_tokens?foo=bar",
                &OptimizationConfig::default(),
            ),
            "count_tokens url should hit even for invalid json",
        );

        assert_eq!(response.reason, "max_tokens_mock");
        assert_eq!(
            get_text_from_optimization_response(&response.body),
            "Max tokens passed."
        );
    }

    #[test]
    fn test_optimization_can_be_disabled_by_flag() {
        let request = json!({
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "quota"}]
        });
        let body = to_json_bytes(&request);

        let flags = OptimizationConfig {
            enable_network_probe_mock: false,
            ..OptimizationConfig::default()
        };

        let response = try_local_optimization(&body, "/v1/messages", &flags);
        assert!(response.is_none());
    }
}
