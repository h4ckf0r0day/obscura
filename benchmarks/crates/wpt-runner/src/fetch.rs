//! Fetch backend: run one `obscura fetch` process per test.
//!
//! The classic WPT runner drives one isolated process per test and reads the
//! result the report overlay leaves on `window.__wptresults_json`.

use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command;

use crate::manifest::TestCase;
use crate::report::FileResult;

const MAX_LINE: usize = 2000;

pub async fn run_fetch(
    obscura_bin: &str,
    tc: &TestCase,
    test_timeout: Duration,
    wait_secs: u64,
) -> FileResult {
    let start = Instant::now();
    // obscura's own deadline, plus a hard wall a bit beyond it for the process.
    let fetch_timeout = test_timeout.as_secs().max(5);
    let hard = test_timeout + Duration::from_secs(10);

    let mut cmd = Command::new(obscura_bin);
    cmd.arg("fetch")
        .arg(&tc.url)
        .arg("--allow-private-network")
        .arg("--quiet")
        .arg("--timeout")
        .arg(fetch_timeout.to_string())
        .arg("--wait")
        .arg(wait_secs.to_string())
        // JSON.parse so the value comes back as a JSON object (or null), which
        // obscura prints verbatim; this avoids guessing how a raw string prints.
        .arg("--eval")
        .arg("JSON.parse(window.__wptresults_json||\"null\")")
        .kill_on_drop(true);

    let mut result = match tokio::time::timeout(hard, cmd.output()).await {
        Err(_) => FileResult::timeout(tc),
        Ok(Err(e)) => FileResult::runner_error(tc, format!("spawn `{obscura_bin} fetch`: {e}")),
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let trimmed = stdout.trim();
            match serde_json::from_str::<Value>(trimmed) {
                Ok(v) if v.is_object() => {
                    let console = strings(v.get("console"));
                    let errors = strings(v.get("errors"));
                    let mut r = FileResult::from_payload(tc, v);
                    r.console = console;
                    r.exceptions = errors;
                    r
                }
                _ => {
                    // null / empty / unparseable: the harness never published a
                    // result. Keep the stderr tail as a hint for triage.
                    let mut r = FileResult::timeout(tc);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let stderr = stderr.trim();
                    if !stderr.is_empty() {
                        r.exceptions.push(clamp(stderr));
                    }
                    r
                }
            }
        }
    };
    result.duration_ms = start.elapsed().as_millis() as u64;
    result
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn clamp(s: &str) -> String {
    if s.chars().count() > MAX_LINE {
        s.chars().take(MAX_LINE).collect::<String>() + "..."
    } else {
        s.to_string()
    }
}
