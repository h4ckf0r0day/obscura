use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

static INIT: Once = Once::new();

/// Set once the first isolate has been constructed, i.e. once V8's platform is
/// up. From that point `set_flags_from_string` is not merely ineffective but
/// fatal, so [`set_v8_flags`] must refuse rather than call it.
static V8_STARTED: AtomicBool = AtomicBool::new(false);

/// Records that V8 has been initialized. Called from `ObscuraJsRuntime`'s
/// constructor under the isolate-creation lock, before the first
/// `JsRuntime::new`.
pub(crate) fn mark_v8_started() {
    V8_STARTED.store(true, Ordering::SeqCst);
}

/// Whether the first isolate has already been constructed in this process.
#[cfg(test)]
pub(crate) fn v8_started() -> bool {
    V8_STARTED.load(Ordering::SeqCst)
}

/// Apply user-supplied V8 flags exactly once, before the first isolate is
/// created.
///
/// `flags` is a raw V8 flag string in the same form V8/Chromium/Node accept
/// (e.g. `"--max-old-space-size=4096 --max-semi-space-size=64"`). An empty or
/// whitespace-only string is a no-op and does not consume the one-shot guard,
/// so a later non-empty call still takes effect.
///
/// A call made after the first isolate exists is **ignored with a warning**.
/// The previous comment here claimed "V8 ignores `set_flags_from_string` once
/// the platform is initialized", which is not what V8 does: it calls
/// `V8_Fatal` from `FlagList::SetFlagsFromCommandLine` and aborts the process
/// with SIGTRAP. Under `cargo test`, where a whole test binary shares one
/// process, that is how `obscura-js --lib` died mid-suite:
/// `heap_limit_terminates_script_and_runtime_recovers` calls this after earlier
/// tests have already built runtimes, and the abort took the binary down,
/// hiding every test that would have run after it.
///
/// The same crash was reachable in production: any embedder calling
/// `set_v8_flags` after creating a runtime aborted the process.
pub fn set_v8_flags(flags: &str) {
    let trimmed = flags.trim();
    if trimmed.is_empty() {
        return;
    }
    if V8_STARTED.load(Ordering::SeqCst) {
        tracing::warn!(
            "ignoring V8 flags {:?}: an isolate already exists, and V8 aborts the \
             process if flags are set after initialization. Pass --v8-flags before \
             the first page or CDP connection.",
            trimmed
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

    /// The regression that matters: once V8 is up, this must return quietly
    /// instead of aborting the process. Before the guard existed this call was
    /// a SIGTRAP.
    #[test]
    fn setting_flags_after_v8_started_is_ignored_not_fatal() {
        mark_v8_started();
        // If this aborts, the test binary dies and no later test reports.
        set_v8_flags("--max-old-space-size=32");
        set_v8_flags("--this-is-not-a-real-v8-flag");
    }
}
