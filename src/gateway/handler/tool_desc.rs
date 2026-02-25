use serde_json::{Value, from_slice, to_vec};

/// 需要从 tools[].description 中过滤的关键词
const TOOLS_DESCRIPTION_FILTER_KEYWORDS: &[&str] = &[
    "A powerful search tool built on ripgrep",
    "Allows Claude to search the web",
    "WebFetch WILL FAIL for authenticated or private URLs.",
    "List all available sources (websites) in the Actionbook database.",
    "Search for sources (websites) by keyword.",
    "Search for website action manuals by keyword.",
    "Get complete action details by area_id, including DOM selectors and element information.",
    "Get complete action details by action ID, including DOM selectors and step-by-step instructions.",
];

/// 检查 tool.description 是否包含需要过滤的关键词
fn should_remove_tool_by_description(description: &str) -> bool {
    TOOLS_DESCRIPTION_FILTER_KEYWORDS
        .iter()
        .any(|keyword| description.contains(keyword))
}

/// 过滤 tools 数组中 description 命中关键词的元素
pub fn filter_tools_by_description(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = from_slice::<Value>(body_bytes).ok()?;

    let tools = json.get_mut("tools")?.as_array_mut()?;
    let original_len = tools.len();

    tools.retain(|tool| {
        tool.get("description")
            .and_then(|d| d.as_str())
            .is_none_or(|description| !should_remove_tool_by_description(description))
    });

    if tools.len() < original_len {
        tracing::info!(
            "🧹 已过滤 tools 数组: {} 个元素 → {} 个元素 (移除了 {} 个)",
            original_len,
            tools.len(),
            original_len - tools.len()
        );
    }

    to_vec(&json).ok().map(Into::into)
}
