use std::io::Read;

use bytes::Bytes;
use flate2::read::GzDecoder;

/// 尝试解压 gzip 编码的响应体
///
/// 检查 content-encoding 头部，如果是 gzip 则自动解压。
/// 返回解压后的字节和是否进行了解压的标志。
pub fn decompress_gzip_if_needed(body_bytes: &Bytes, content_encoding: Option<&str>) -> Bytes {
    let is_gzip = content_encoding.is_some_and(|enc| enc.to_lowercase().contains("gzip"));

    if !is_gzip {
        return body_bytes.clone();
    }

    let mut decoder = GzDecoder::new(&body_bytes[..]);
    let mut decompressed = Vec::new();
    match decoder.read_to_end(&mut decompressed) {
        Ok(_) => {
            tracing::debug!(
                "📦 gzip 解压成功: {} bytes → {} bytes",
                body_bytes.len(),
                decompressed.len()
            );
            decompressed.into()
        }
        Err(e) => {
            tracing::warn!("gzip 解压失败: {}，使用原始响应体", e);
            body_bytes.clone()
        }
    }
}
