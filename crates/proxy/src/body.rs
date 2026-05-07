use anyhow::{Result, bail};
use bytes::Bytes;
use http_body_util::BodyExt;
use salvo::prelude::*;
use serde_json::{Value, from_slice, to_vec};

pub async fn get_req_body(req: &mut Request) -> Result<Bytes> {
    let body_bytes = match BodyExt::collect(req.body_mut()).await {
        Ok(body) => body.to_bytes(),
        Err(e) => {
            bail!("Failed to collect request body: {e}");
        }
    };
    Ok(body_bytes)
}

pub fn parse_body_json(body_bytes: &[u8]) -> Result<Value> {
    from_slice::<Value>(body_bytes).map_err(Into::into)
}

pub fn serialize_body_json(json: &Value) -> Result<Bytes> {
    to_vec(json).map(Into::into).map_err(Into::into)
}
