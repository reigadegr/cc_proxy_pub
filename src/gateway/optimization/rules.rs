use serde_json::Value;

use super::detection;
use crate::config::OptimizationConfig;

pub enum OptimizationRuleMatch {
    CountTokensUrl,
    QuotaProbe,
    HistoricalAnalysis,
    PrefixCommand { command: String },
    TitleGeneration,
    SuggestionMode,
    FilepathExtraction { command: String, output: String },
}

impl OptimizationRuleMatch {
    pub const fn log_message(&self) -> &'static str {
        match self {
            Self::CountTokensUrl => "Intercepted count_tokens URL",
            Self::QuotaProbe => "Intercepted and mocked quota probe",
            Self::HistoricalAnalysis => "Skipped historical analysis request",
            Self::PrefixCommand { .. } => "Handled fast prefix detection",
            Self::TitleGeneration => "Skipped title generation request",
            Self::SuggestionMode => "Skipped suggestion mode request",
            Self::FilepathExtraction { .. } => "Mocked filepath extraction request",
        }
    }
}

pub fn detect_url_rule(
    request_url: &str,
    flags: &OptimizationConfig,
) -> Option<OptimizationRuleMatch> {
    if flags.enable_network_probe_mock && detection::is_count_tokens_url(request_url) {
        return Some(OptimizationRuleMatch::CountTokensUrl);
    }

    None
}

pub fn detect_request_rule(
    request: &Value,
    flags: &OptimizationConfig,
) -> Option<OptimizationRuleMatch> {
    if flags.enable_network_probe_mock && detection::is_quota_check_request(request) {
        return Some(OptimizationRuleMatch::QuotaProbe);
    }

    if flags.enable_historical_analysis_mock && detection::is_historical_analysis_request(request) {
        return Some(OptimizationRuleMatch::HistoricalAnalysis);
    }

    if flags.enable_fast_prefix_detection
        && let Some(command) = detection::detect_prefix_command(request)
    {
        return Some(OptimizationRuleMatch::PrefixCommand { command });
    }

    if flags.enable_title_generation_skip && detection::is_title_generation_request(request) {
        return Some(OptimizationRuleMatch::TitleGeneration);
    }

    if flags.enable_suggestion_mode_skip && detection::is_suggestion_mode_request(request) {
        return Some(OptimizationRuleMatch::SuggestionMode);
    }

    if flags.enable_filepath_extraction_mock
        && let Some((command, output)) = detection::detect_filepath_extraction_request(request)
    {
        return Some(OptimizationRuleMatch::FilepathExtraction { command, output });
    }

    None
}
