use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

static RESPONSE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct OptimizationResponse {
    pub body: Vec<u8>,
    pub reason: &'static str,
}

pub fn build_text_response(
    model: &str,
    text: &str,
    input_tokens: u64,
    output_tokens: u64,
    reason: &'static str,
) -> Option<OptimizationResponse> {
    let payload = json!({
        "id": build_message_id(),
        "type": "message",
        "role": "assistant",
        "model": if model.is_empty() { "unknown-model" } else { model },
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens
        }
    });

    let body = serde_json::to_vec(&payload).ok()?;
    Some(OptimizationResponse { body, reason })
}

fn build_message_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    let sequence = RESPONSE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("msg_{millis}_{sequence}")
}
