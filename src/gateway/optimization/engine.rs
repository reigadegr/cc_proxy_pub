use serde_json::Value;

use crate::config::OptimizationConfig;

use super::{
    OptimizationResponse, command_utils, response_builder,
    rules::{OptimizationRuleMatch, detect_request_rule, detect_url_rule},
};

pub fn try_local_optimization(
    body_bytes: &[u8],
    request_url: &str,
    flags: &OptimizationConfig,
) -> Option<OptimizationResponse> {
    if let Some(rule_match) = detect_url_rule(request_url, flags) {
        tracing::info!("Optimization: {}", rule_match.log_message());
        return build_response(rule_match);
    }

    let request: Value = serde_json::from_slice(body_bytes).ok()?;
    let rule_match = detect_request_rule(&request, flags)?;

    tracing::info!("Optimization: {}", rule_match.log_message());
    build_response(rule_match)
}

fn build_response(rule_match: OptimizationRuleMatch) -> Option<OptimizationResponse> {
    match rule_match {
        OptimizationRuleMatch::CountTokensUrl => response_builder::build_text_response(
            "unknown-model",
            "Max tokens passed.",
            10,
            5,
            "max_tokens_mock",
        ),
        OptimizationRuleMatch::QuotaProbe => response_builder::build_text_response(
            "unknown-model",
            "Quota check passed.",
            10,
            5,
            "quota_probe_mock",
        ),
        OptimizationRuleMatch::HistoricalAnalysis => response_builder::build_text_response(
            "unknown-model",
            "historical analysis passed.",
            100,
            5,
            "historical_analysis_skip",
        ),
        OptimizationRuleMatch::PrefixCommand { command } => {
            let prefix = command_utils::extract_command_prefix(command.as_str());
            response_builder::build_text_response(
                "unknown-model",
                prefix.as_str(),
                100,
                5,
                "fast_prefix_detection",
            )
        }
        OptimizationRuleMatch::TitleGeneration => response_builder::build_text_response(
            "unknown-model",
            "Conversation",
            100,
            5,
            "title_generation_skip",
        ),
        OptimizationRuleMatch::SuggestionMode => response_builder::build_text_response(
            "unknown-model",
            "",
            100,
            1,
            "suggestion_mode_skip",
        ),
        OptimizationRuleMatch::FilepathExtraction { command, output } => {
            let filepaths =
                command_utils::extract_filepaths_from_command(command.as_str(), output.as_str());
            response_builder::build_text_response(
                "unknown-model",
                filepaths.as_str(),
                100,
                10,
                "filepath_extraction_mock",
            )
        }
    }
}
