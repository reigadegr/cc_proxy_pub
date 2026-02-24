//! `OpenAI` Responses API 与 Anthropic Claude API 格式双向转换
//!
//! 功能：
//! - Claude CLI 请求 → `OpenAI` Responses 请求
//! - `OpenAI` Responses 响应 → Claude CLI 响应
//!
//! 参考文档：`API_FORMAT_CONVERSION.md`

use bytes::Bytes;

mod media;
mod request;
mod response;
mod tools;

/// Claude 请求 → `OpenAI` Responses 请求
pub fn anthropic_request_to_responses(body: &Bytes) -> Result<Bytes, String> {
    request::anthropic_request_to_responses(body)
}

/// `OpenAI` Responses 响应 → Claude 响应
pub fn responses_response_to_anthropic(
    body: &Bytes,
    model_hint: Option<&str>,
) -> Result<Bytes, String> {
    response::responses_response_to_anthropic(body, model_hint)
}
