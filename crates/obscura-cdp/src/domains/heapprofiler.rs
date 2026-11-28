use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::types::CdpEvent;

/// Upper bound on the size of one `HeapProfiler.addHeapSnapshotChunk` payload.
pub const HEAP_SNAPSHOT_CHUNK_BYTES: usize = 1 << 20;

pub async fn handle(
    method: &str,
    _params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "collectGarbage" => {
            let js = ctx
                .get_session_page_mut(session_id)
                .and_then(|page| page.js.as_mut())
                .ok_or_else(|| "No JavaScript runtime".to_string())?;
            js.collect_garbage();
            Ok(json!({}))
        }
        "takeHeapSnapshot" => {
            let js = ctx
                .get_session_page_mut(session_id)
                .and_then(|page| page.js.as_mut())
                .ok_or_else(|| "No JavaScript runtime".to_string())?;
            let snapshot = String::from_utf8(js.take_heap_snapshot())
                .map_err(|e| format!("Heap snapshot is not valid UTF-8: {e}"))?;
            let mut rest = snapshot.as_str();
            while !rest.is_empty() {
                let mut end = HEAP_SNAPSHOT_CHUNK_BYTES.min(rest.len());
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                let (piece, tail) = rest.split_at(end);
                ctx.pending_events.push(CdpEvent {
                    method: "HeapProfiler.addHeapSnapshotChunk".to_string(),
                    params: json!({ "chunk": piece }),
                    session_id: session_id.clone(),
                });
                rest = tail;
            }
            Ok(json!({}))
        }
        _ => Ok(json!({})),
    }
}
