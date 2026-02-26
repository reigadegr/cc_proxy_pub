use serde_json::{Value, from_slice, to_vec};

/// 需要从 system 数组中移除的文本特征（多个标记，匹配任意一个即过滤）
const SYSTEM_PROMPT_FILTER_MARKERS: &[&str] = &[
    // // Claude CLI 的主要提示词
    // "You are an interactive CLI tool that helps users with soft",
    // // Claude Code 身份标识
    "You are Claude Code",
    // // Claude Code 查找文件标识
    "You are a file search specialist for Claude Code",
    // // Claude Code 无意义版本信息
    "x-anthropic-billing-header: cc_version=",
];

/// 过滤请求体中的 system 数组，移除包含特定文本的元素
///
/// Claude CLI 发送的请求中，system 数组包含很长的提示词文本，
/// 这些文本会占用大量 tokens。此函数移除包含任意标记文本的元素。
pub fn filter_system_prompts(body_bytes: &[u8]) -> Option<bytes::Bytes> {
    let mut json = from_slice::<Value>(body_bytes).ok()?;

    // 获取 system 数组
    let system = json.get_mut("system")?.as_array_mut()?;

    let original_len = system.len();

    // 过滤掉包含任意标记文本的元素
    system.retain(|item| {
        item.get("text")
            .and_then(|t| t.as_str())
            .is_none_or(|text| {
                !SYSTEM_PROMPT_FILTER_MARKERS
                    .iter()
                    .any(|marker| text.contains(marker))
            })
    });

    // 如果有元素被移除，记录日志
    if system.len() < original_len {
        tracing::info!(
            "🧹 已过滤 system 数组: {} 个元素 → {} 个元素 (移除了 {} 个)",
            original_len,
            system.len(),
            original_len - system.len()
        );
    }

    to_vec(&json).ok().map(Into::into)
}
