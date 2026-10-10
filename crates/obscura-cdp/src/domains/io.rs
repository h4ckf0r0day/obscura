use std::collections::{HashMap, VecDeque};

use base64::Engine as _;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

// Default chunk size when the client does not pass `size`. Chrome uses a similar
// order of magnitude; keeping chunks bounded is the point of streaming (issue
// #360), so we never return the whole body in one IO.read.
const DEFAULT_CHUNK: usize = 1 << 20; // 1 MiB
const MAX_READ_CHUNK: usize = 4 << 20; // 4 MiB

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
pub struct IoStreamStore {
    streams: HashMap<String, (Vec<u8>, usize)>,
    order: VecDeque<String>,
    total_bytes: usize,
    counter: u64,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for IoStreamStore {
    fn default() -> Self {
        Self::with_limits(io_stream_max_entries(), io_stream_max_bytes())
    }
}

impl IoStreamStore {
    fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            streams: HashMap::new(),
            order: VecDeque::new(),
            total_bytes: 0,
            counter: 0,
            max_entries: max_entries.max(1),
            max_bytes,
        }
    }

    /// Store a body and return its handle, evicting the oldest streams if this
    /// would push the store past its entry or byte cap. A single body larger
    /// than the byte cap is rejected rather than becoming an unbounded
    /// exception to the store's memory contract.
    pub fn insert(&mut self, bytes: Vec<u8>) -> Result<String, String> {
        if bytes.len() > self.max_bytes {
            return Err(format!(
                "IO stream body is {} bytes, exceeding the {}-byte per-context limit",
                bytes.len(),
                self.max_bytes,
            ));
        }

        while !self.order.is_empty()
            && (self.order.len() >= self.max_entries
                || self
                    .total_bytes
                    .checked_add(bytes.len())
                    .is_none_or(|total| total > self.max_bytes))
        {
            if let Some(oldest) = self.order.pop_front() {
                if let Some((body, _)) = self.streams.remove(&oldest) {
                    self.total_bytes = self.total_bytes.saturating_sub(body.len());
                }
            }
        }

        let handle = format!("stream-{}", self.counter);
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or("IO stream handle space exhausted")?;
        self.total_bytes = self
            .total_bytes
            .checked_add(bytes.len())
            .ok_or("IO stream byte accounting overflow")?;
        self.streams.insert(handle.clone(), (bytes, 0));
        self.order.push_back(handle.clone());
        Ok(handle)
    }

    /// Read up to `size` bytes from the stream, advancing its cursor. Returns
    /// the base64 chunk and whether EOF was reached, or None for an unknown or
    /// already-freed handle.
    pub fn read(
        &mut self,
        handle: &str,
        offset: Option<usize>,
        size: usize,
    ) -> Option<(String, bool)> {
        let (bytes, cursor) = self.streams.get_mut(handle)?;
        if let Some(offset) = offset {
            *cursor = offset.min(bytes.len());
        }
        let size = size.min(MAX_READ_CHUNK);
        let start = (*cursor).min(bytes.len());
        let end = start.saturating_add(size).min(bytes.len());
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
                .map(|value| {
                    value
                        .as_i64()
                        .filter(|size| *size >= 0)
                        .and_then(|size| usize::try_from(size).ok())
                        .ok_or("IO.read size must be a non-negative integer")
                })
                .transpose()?
                .unwrap_or(DEFAULT_CHUNK);
            let offset = params
                .get("offset")
                .map(|value| {
                    value
                        .as_i64()
                        .filter(|offset| *offset >= 0)
                        .and_then(|offset| usize::try_from(offset).ok())
                        .ok_or("IO.read offset must be a non-negative integer")
                })
                .transpose()?;

            let (data, eof) = ctx
                .io_streams
                .read(handle, offset, size)
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
        let mut store = IoStreamStore::with_limits(4, 1024);
        let h = store.insert(b"hello".to_vec()).unwrap();

        let (d1, eof1) = store.read(&h, None, 3).unwrap();
        assert_eq!(decode(&d1), b"hel");
        assert!(!eof1);

        let (d2, eof2) = store.read(&h, None, 3).unwrap();
        assert_eq!(decode(&d2), b"lo");
        assert!(eof2);

        store.remove(&h);
        assert!(store.read(&h, None, 3).is_none());
    }

    #[test]
    fn read_offset_seeks_and_zero_size_does_not_advance() {
        let mut store = IoStreamStore::with_limits(2, 1024);
        let handle = store.insert(b"abcdef".to_vec()).unwrap();

        let (empty, eof) = store.read(&handle, Some(1), 0).unwrap();
        assert_eq!(decode(&empty), b"");
        assert!(!eof);
        let (middle, eof) = store.read(&handle, None, 2).unwrap();
        assert_eq!(decode(&middle), b"bc");
        assert!(!eof);
        let (tail, eof) = store.read(&handle, Some(4), 10).unwrap();
        assert_eq!(decode(&tail), b"ef");
        assert!(eof);
    }

    #[tokio::test]
    async fn read_rejects_negative_or_non_integer_ranges() {
        let mut ctx = CdpContext::new();
        let handle_id = ctx.io_streams.insert(b"data".to_vec()).unwrap();
        for params in [
            json!({"handle": handle_id.clone(), "offset": -1}),
            json!({"handle": handle_id.clone(), "size": -1}),
            json!({"handle": handle_id, "offset": 1.5}),
        ] {
            assert!(handle("read", &params, &mut ctx).await.is_err(), "{params}");
        }
    }

    #[test]
    fn evicts_oldest_over_entry_cap() {
        let mut store = IoStreamStore::with_limits(3, 1024);
        let h0 = store.insert(vec![0]).unwrap();
        let h1 = store.insert(vec![1]).unwrap();
        let _h2 = store.insert(vec![2]).unwrap();
        let h3 = store.insert(vec![3]).unwrap(); // 4th entry, cap 3 -> h0 evicted

        assert!(
            store.read(&h0, None, 10).is_none(),
            "oldest stream should be evicted"
        );
        assert!(store.read(&h1, None, 10).is_some());
        assert!(store.read(&h3, None, 10).is_some());
    }

    #[test]
    fn evicts_over_byte_cap_and_rejects_oversized_body() {
        let mut store = IoStreamStore::with_limits(4, 10);
        let h0 = store.insert(vec![0u8; 8]).unwrap();
        let h1 = store.insert(vec![1u8; 8]).unwrap(); // 16 > 10 -> h0 evicted
        let error = store
            .insert(vec![2u8; 100])
            .expect_err("a single body cannot bypass the context byte cap");

        assert!(store.read(&h0, None, 100).is_none());
        assert!(store.read(&h1, None, 100).is_some());
        assert!(error.contains("exceeding"), "{error}");
    }

    #[test]
    fn requested_read_size_is_capped() {
        let mut store = IoStreamStore::with_limits(2, MAX_READ_CHUNK * 2);
        let handle = store.insert(vec![7u8; MAX_READ_CHUNK + 17]).unwrap();
        let (first, eof) = store.read(&handle, None, usize::MAX).unwrap();
        assert_eq!(decode(&first).len(), MAX_READ_CHUNK);
        assert!(!eof);
        let (second, eof) = store.read(&handle, None, usize::MAX).unwrap();
        assert_eq!(decode(&second).len(), 17);
        assert!(eof);
    }
}
