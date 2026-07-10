use base64::Engine as _;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

// Default chunk size when the client does not pass `size`. Chrome uses a similar
// order of magnitude; keeping chunks bounded is the point of streaming (issue
// #360), so we never return the whole body in one IO.read.
const DEFAULT_CHUNK: usize = 1 << 20; // 1 MiB

/// CDP IO domain. Streams a response body handed out by
/// Fetch.takeResponseBodyAsStream: IO.read returns the next base64 chunk and
/// IO.close frees the buffer. Nothing here runs unless a client opened a stream.
pub async fn handle(method: &str, params: &Value, ctx: &mut CdpContext) -> Result<Value, String> {
    match method {
        "read" => {
            let handle = params
                .get("handle")
                .and_then(|v| v.as_str())
                .ok_or("IO.read requires handle")?;
            let size = params
                .get("size")
                .and_then(|v| v.as_u64())
                .map(|s| s as usize)
                .unwrap_or(DEFAULT_CHUNK)
                .max(1);

            let (bytes, cursor) = ctx
                .io_streams
                .get_mut(handle)
                .ok_or_else(|| format!("IO.read: unknown handle {handle}"))?;

            let start = (*cursor).min(bytes.len());
            let end = start.saturating_add(size).min(bytes.len());
            let chunk = &bytes[start..end];
            let data = base64::engine::general_purpose::STANDARD.encode(chunk);
            *cursor = end;
            let eof = end >= bytes.len();

            Ok(json!({ "data": data, "eof": eof, "base64Encoded": true }))
        }
        "close" => {
            let handle = params
                .get("handle")
                .and_then(|v| v.as_str())
                .ok_or("IO.close requires handle")?;
            // Dropping the entry frees the buffered body.
            ctx.io_streams.remove(handle);
            Ok(json!({}))
        }
        _ => Err(format!("Unknown IO method: {}", method)),
    }
}
