//! Turn buffered CDP protocol events into human-readable console and exception
//! lines. The runner drains `Cdp::take_events()` after a test finishes (or times
//! out) and hands the frames here.
//!
//! We only care about three event kinds:
//!   - Runtime.consoleAPICalled -> console.* output
//!   - Log.entryAdded           -> browser log entries (also console)
//!   - Runtime.exceptionThrown  -> uncaught exceptions
//!
//! Everything is treated as best-effort: each event field is optional and a
//! malformed frame just gets skipped. Output is capped so a runaway test cannot
//! produce gigabytes of strings.

use serde_json::Value;

/// Max lines kept per bucket.
const MAX_LINES: usize = 50;
/// Max characters kept per line.
const MAX_LEN: usize = 2000;

/// Returns `(console_lines, exception_lines)` extracted from a batch of CDP
/// protocol-event frames.
pub fn extract(events: &[Value]) -> (Vec<String>, Vec<String>) {
    let mut console = Vec::new();
    let mut exceptions = Vec::new();

    for ev in events {
        let method = match ev.get("method").and_then(Value::as_str) {
            Some(m) => m,
            None => continue,
        };
        let params = ev.get("params").unwrap_or(&Value::Null);

        match method {
            "Runtime.consoleAPICalled" => {
                if console.len() < MAX_LINES {
                    let kind = params
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("log");
                    let text = join_args(params.get("args"));
                    console.push(clamp(format!("[{kind}] {text}")));
                }
            }
            "Log.entryAdded" => {
                if console.len() < MAX_LINES {
                    let entry = params.get("entry").unwrap_or(&Value::Null);
                    let level = entry
                        .get("level")
                        .and_then(Value::as_str)
                        .unwrap_or("info");
                    let text = entry.get("text").and_then(Value::as_str).unwrap_or("");
                    console.push(clamp(format!("[{level}] {text}")));
                }
            }
            "Runtime.exceptionThrown" => {
                if exceptions.len() < MAX_LINES {
                    let details = params.get("exceptionDetails").unwrap_or(&Value::Null);
                    exceptions.push(clamp(format_exception(details)));
                }
            }
            _ => {}
        }
    }

    (console, exceptions)
}

/// Join the values of a `Runtime.consoleAPICalled` args array into one string,
/// preferring `value` (primitives) and falling back to `description` (objects).
fn join_args(args: Option<&Value>) -> String {
    let arr = match args.and_then(Value::as_array) {
        Some(a) => a,
        None => return String::new(),
    };
    let parts: Vec<String> = arr.iter().map(arg_to_string).collect();
    parts.join(" ")
}

fn arg_to_string(arg: &Value) -> String {
    if let Some(v) = arg.get("value") {
        match v {
            Value::String(s) => return s.clone(),
            Value::Null => {}
            other => return other.to_string(),
        }
    }
    if let Some(d) = arg.get("description").and_then(Value::as_str) {
        return d.to_string();
    }
    if let Some(t) = arg.get("type").and_then(Value::as_str) {
        return format!("<{t}>");
    }
    String::new()
}

/// Build a one-line summary of an `exceptionDetails` object, preferring the
/// thrown exception's `description`, then the top-level `text`, and appending
/// `url:line` when available.
fn format_exception(details: &Value) -> String {
    let mut msg = details
        .pointer("/exception/description")
        .and_then(Value::as_str)
        .or_else(|| details.get("text").and_then(Value::as_str))
        .unwrap_or("uncaught exception")
        .to_string();

    if let Some(url) = details.get("url").and_then(Value::as_str) {
        if !url.is_empty() {
            match details.get("lineNumber").and_then(Value::as_i64) {
                Some(line) => msg.push_str(&format!(" ({url}:{line})")),
                None => msg.push_str(&format!(" ({url})")),
            }
        }
    }
    msg
}

fn clamp(mut s: String) -> String {
    if s.len() > MAX_LEN {
        // Back off to the nearest char boundary so truncate never panics on
        // multi-byte UTF-8.
        let mut cut = MAX_LEN;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("...");
    }
    s
}
