use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static INIT: Once = Once::new();
/// Set once the first `ObscuraJsRuntime` (and thus the V8 platform) has been
/// constructed. A `set_v8_flags` call after this point is dropped rather than
/// aborting the process.
static PLATFORM_STARTED: AtomicBool = AtomicBool::new(false);

/// Record that a JS runtime is being constructed, so any later `set_v8_flags`
/// call is refused instead of reaching V8's fatal late-flag path. Called at the
/// top of runtime construction, before the platform is initialized.
pub(crate) fn mark_platform_started() {
    PLATFORM_STARTED.store(true, Ordering::SeqCst);
}

/// Apply user-supplied V8 flags exactly once, before the first isolate is
/// created.
///
/// `flags` is a raw V8 flag string in the same form V8/Chromium/Node accept
/// (e.g. `"--max-old-space-size=4096 --max-semi-space-size=64"`). An empty or
/// whitespace-only string is a no-op and does not consume the one-shot guard,
/// so a later non-empty call still takes effect.
///
/// V8 ignores `set_flags_from_string` once the platform is initialized, so the
/// first non-empty call must run before any `JsRuntime` is constructed.
/// Subsequent calls are silently dropped.
pub fn set_v8_flags(flags: &str) {
    let trimmed = flags.trim();
    if trimmed.is_empty() {
        return;
    }
    if PLATFORM_STARTED.load(Ordering::SeqCst) {
        // Once an isolate exists, V8's SetFlagsFromCommandLine calls V8_Fatal
        // and aborts the whole process (SIGTRAP) — it does NOT silently ignore
        // the call as the platform docs imply. Drop the late call with a
        // warning instead of crashing the embedder; flags only take effect
        // before the first ObscuraJsRuntime is constructed.
        tracing::warn!(
            "set_v8_flags({trimmed:?}) ignored: a JS runtime already exists — \
             V8 flags must be set before the first ObscuraJsRuntime is constructed"
        );
        return;
    }
    INIT.call_once(|| {
        deno_core::v8::V8::set_flags_from_string(trimmed);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_noop() {
        // Must not panic and must not consume the Once guard.
        set_v8_flags("");
        set_v8_flags("   ");
        set_v8_flags("\t\n");
    }

    // #853 — a set_v8_flags call after a runtime exists used to abort the whole
    // process (V8_Fatal / SIGTRAP). Constructing a real runtime initializes the
    // V8 platform (and marks it started); the subsequent set_v8_flags must be
    // dropped with a warning rather than crashing. Reaching the assertion at all
    // (no SIGTRAP) is the regression check.
    #[test]
    fn late_call_after_a_runtime_exists_does_not_abort() {
        let _rt = crate::runtime::ObscuraJsRuntime::new();
        set_v8_flags("--max-old-space-size=32");
        assert!(PLATFORM_STARTED.load(Ordering::SeqCst));
    }
}
