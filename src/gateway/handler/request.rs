use bytes::Bytes;
use serde_json::{Value, from_slice, json, to_vec};

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
