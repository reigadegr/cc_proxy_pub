use anyhow::{Result, bail};
use bytes::Bytes;
use http_body_util::BodyExt;
use salvo::prelude::*;
use serde_json::{Value, from_slice, json, to_vec};

use crate::gateway::handler::{
    filter_messages_content, filter_system_prompts, filter_tools_by_description,
};

/// 尝试覆盖请求体中的 model 字段
pub fn override_model_in_body(body_bytes: &[u8], model: &str) -> Option<Bytes> {
    let json = from_slice::<Value>(body_bytes).ok()?;
    let original_model = json.get("model").and_then(|m| m.as_str());

    if let Some(original) = original_model {
        tracing::info!("原始 model: {} -> 覆盖为: {}", original, model);
    }

    let mut modified = json;
    modified["model"] = json!(model);

    to_vec(&modified).ok().map(Into::into)
}

pub async fn filter_req_body(req: &mut Request) -> Result<Bytes> {
    // 收集请求体
    let mut body_bytes = match BodyExt::collect(req.body_mut()).await {
        Ok(body) => body.to_bytes(),
        Err(e) => {
            bail!("Failed to collect request body: {e}");
        }
    };

    // 过滤 system 数组中占用大量 tokens 的提示词
    if !body_bytes.is_empty()
        && let Some(filtered) = filter_system_prompts(&body_bytes)
    {
        body_bytes = filtered;
    }

    // 过滤 messages.content 中占用大量 tokens 的无用标签
    if !body_bytes.is_empty()
        && let Some(filtered) = filter_messages_content(&body_bytes)
    {
        body_bytes = filtered;
    }

    // 过滤 tools.description 命中关键词的工具定义
    if !body_bytes.is_empty()
        && let Some(filtered) = filter_tools_by_description(&body_bytes)
    {
        body_bytes = filtered;
    }
    Ok(body_bytes)
}
