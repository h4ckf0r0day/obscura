use std::collections::{HashMap, VecDeque};

use base64::Engine as _;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

// Default chunk size when the client does not pass `size`. Chrome uses a similar
// order of magnitude; keeping chunks bounded is the point of streaming (issue
// #360), so we never return the whole body in one IO.read.
const DEFAULT_CHUNK: usize = 1 << 20; // 1 MiB

fn io_stream_max_entries() -> usize {
    std::env::var("OBSCURA_IO_STREAM_MAX_ENTRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

fn io_stream_max_bytes() -> usize {
    std::env::var("OBSCURA_IO_STREAM_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256 * 1024 * 1024)
}

/// Bounded store of the response bodies handed out by
/// Fetch.takeResponseBodyAsStream. Streaming exists to keep large downloads out
/// of memory (issue #360), but each taken body is moved out of the page's
/// LRU-bounded cache into this map, which lives for the whole server lifetime.
/// A client that opens streams and never calls IO.close, or simply disconnects
/// mid-download, would otherwise pin every taken body forever and reintroduce
/// exactly the unbounded accumulation streaming was meant to avoid. Cap the
/// number of open streams and their total bytes, evicting the oldest first, so
/// memory stays bounded regardless of client behavior. Reading an evicted
/// handle fails cleanly (the client re-takes or gives up), which is the right
/// trade against an OOM.
#[derive(Default)]
pub struct IoStreamStore {
    streams: HashMap<String, (Vec<u8>, usize)>,
    order: VecDeque<String>,
    total_bytes: usize,
    counter: u64,
}

impl IoStreamStore {
    /// Store a body and return its handle, evicting the oldest streams if this
    /// pushes the store past its entry or byte cap.
    pub fn insert(&mut self, bytes: Vec<u8>) -> String {
        let handle = format!("stream-{}", self.counter);
        self.counter += 1;
        self.total_bytes += bytes.len();
        self.streams.insert(handle.clone(), (bytes, 0));
        self.order.push_back(handle.clone());

        let max_entries = io_stream_max_entries().max(1);
        let max_bytes = io_stream_max_bytes();
        // Evict oldest streams while over either cap. Never evict the stream we
        // just inserted (it sits at the back of `order`); a lone oversized body
        // is kept, since the client explicitly asked to stream it.
        while self.order.len() > 1
            && (self.order.len() > max_entries || self.total_bytes > max_bytes)
        {
            if let Some(oldest) = self.order.pop_front() {
                if let Some((b, _)) = self.streams.remove(&oldest) {
                    self.total_bytes -= b.len();
                }
            }
        }
        handle
    }

    /// Read up to `size` bytes from the stream, advancing its cursor. Returns
    /// the base64 chunk and whether EOF was reached, or None for an unknown or
    /// already-freed handle.
    pub fn read(&mut self, handle: &str, size: usize) -> Option<(String, bool)> {
        let (bytes, cursor) = self.streams.get_mut(handle)?;
        let start = (*cursor).min(bytes.len());
        let end = start.saturating_add(size.max(1)).min(bytes.len());
        let data = base64::engine::general_purpose::STANDARD.encode(&bytes[start..end]);
        *cursor = end;
        Some((data, end >= bytes.len()))
    }

    /// Free a stream's buffer (IO.close). A no-op for an unknown handle.
    pub fn remove(&mut self, handle: &str) {
        if let Some((b, _)) = self.streams.remove(handle) {
            self.total_bytes -= b.len();
            self.order.retain(|h| h != handle);
        }
    }
}

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
                .unwrap_or(DEFAULT_CHUNK);

            let (data, eof) = ctx
                .io_streams
                .read(handle, size)
                .ok_or_else(|| format!("IO.read: unknown handle {handle}"))?;

            Ok(json!({ "data": data, "eof": eof, "base64Encoded": true }))
        }
        "close" => {
            let handle = params
                .get("handle")
                .and_then(|v| v.as_str())
                .ok_or("IO.close requires handle")?;
            ctx.io_streams.remove(handle);
            Ok(json!({}))
        }
        _ => Err(format!("Unknown IO method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    #[test]
    fn reads_chunks_then_frees() {
        let mut store = IoStreamStore::default();
        let h = store.insert(b"hello".to_vec());

        let (d1, eof1) = store.read(&h, 3).unwrap();
        assert_eq!(decode(&d1), b"hel");
        assert!(!eof1);

        let (d2, eof2) = store.read(&h, 3).unwrap();
        assert_eq!(decode(&d2), b"lo");
        assert!(eof2);

        store.remove(&h);
        assert!(store.read(&h, 3).is_none());
    }

    #[test]
    fn evicts_oldest_over_entry_cap() {
        std::env::set_var("OBSCURA_IO_STREAM_MAX_ENTRIES", "3");
        let mut store = IoStreamStore::default();
        let h0 = store.insert(vec![0]);
        let h1 = store.insert(vec![1]);
        let _h2 = store.insert(vec![2]);
        let h3 = store.insert(vec![3]); // 4th entry, cap 3 -> h0 evicted
        std::env::remove_var("OBSCURA_IO_STREAM_MAX_ENTRIES");

        assert!(store.read(&h0, 10).is_none(), "oldest stream should be evicted");
        assert!(store.read(&h1, 10).is_some());
        assert!(store.read(&h3, 10).is_some());
    }

    #[test]
    fn evicts_over_byte_cap_but_keeps_newest() {
        std::env::set_var("OBSCURA_IO_STREAM_MAX_BYTES", "10");
        let mut store = IoStreamStore::default();
        let h0 = store.insert(vec![0u8; 8]);
        let h1 = store.insert(vec![1u8; 8]); // 16 > 10 -> h0 evicted
        // A single body larger than the cap is still kept (client asked for it).
        let big = store.insert(vec![2u8; 100]);
        std::env::remove_var("OBSCURA_IO_STREAM_MAX_BYTES");

        assert!(store.read(&h0, 100).is_none());
        assert!(store.read(&big, 200).is_some(), "the just-inserted body is never evicted");
        let _ = h1;
    }
}
