//! Process-wide async lock that serializes V8 work across all `JsRuntime`s.
//!
//! V8's invariant: on any given OS thread, only one `Isolate` may be entered
//! at a time, and HandleScope/ContextScope stacks must unwind on the Isolate
//! they belong to. Obscura's CDP server runs every `JsRuntime` (one per Page)
//! on a single OS thread via `tokio::task::LocalSet` + `spawn_local`. As soon
//! as two pages' V8-touching futures interleave across an `.await`, V8 trips
//! its `heap->isolate() == Isolate::TryGetCurrent()` check and aborts the
//! whole process (no Rust panic; `V8_Fatal` calls `abort(3)`).
//!
//! Acquiring this lock around any block that calls `JsRuntime::execute_script`
//! or `JsRuntime::run_event_loop` keeps that block contiguous on the thread:
//! V8 fully exits the prior Isolate before the next page is allowed in. This
//! converts the abort into latency. It is the issue-19 "Option 1" fix.
//!
//! The properly concurrent fix is to pin each `JsRuntime` to its own OS
//! thread (issue-19 "Option 2"); that's a larger refactor tracked separately.

use std::sync::OnceLock;
use tokio::sync::Mutex;

static V8_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Returns the process-wide V8 serialization lock.
pub fn global() -> &'static Mutex<()> {
    V8_LOCK.get_or_init(|| Mutex::new(()))
}
