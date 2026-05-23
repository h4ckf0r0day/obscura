use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(method: &str, params: &Value, ctx: &mut CdpContext) -> Result<Value, String> {
    match method {
        "read" => {
            let handle = params
                .get("handle")
                .and_then(|v| v.as_str())
                .ok_or("handle required")?;
            let bytes = ctx
                .io_streams
                .lock()
                .await
                .remove(handle)
                .unwrap_or_default();
            Ok(json!({
                "base64Encoded": true,
                "data": general_purpose::STANDARD.encode(bytes),
                "eof": true,
            }))
        }
        "close" => {
            if let Some(handle) = params.get("handle").and_then(|v| v.as_str()) {
                ctx.io_streams.lock().await.remove(handle);
            }
            Ok(json!({}))
        }
        _ => Err(format!("Unknown IO method: {}", method)),
    }
}
