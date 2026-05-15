use std::sync::Once;

static INIT: Once = Once::new();

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
}
