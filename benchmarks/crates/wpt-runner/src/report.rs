//! Result aggregation and the stable JSON contract consumed by `triage`.
//!
//! testharness.js uses harness status 0 for OK and 1..3 for failures. Runner
//! statuses are -1 timeout, -2 error, and -3 explicitly unsupported.

use serde_json::{json, Value};
use std::time::Duration;

use crate::manifest::TestCase;

pub struct Subtest {
    pub name: String,
    pub status: i64,
    pub message: Option<String>,
    pub stack: Option<String>,
}

pub struct FileResult {
    pub path: String,
    pub url: String,
    pub harness_status: i64,
    pub harness_message: Option<String>,
    pub error: Option<String>,
    pub subtests: Vec<Subtest>,
    pub console: Vec<String>,
    pub exceptions: Vec<String>,
    pub duration_ms: u64,
}

impl FileResult {
    pub fn from_payload(tc: &TestCase, payload: Value) -> Self {
        let harness_status = payload
            .pointer("/harness/status")
            .and_then(Value::as_i64)
            .unwrap_or(1);
        let harness_message = payload
            .pointer("/harness/message")
            .and_then(Value::as_str)
            .map(str::to_string);
        let subtests = payload
            .get("tests")
            .and_then(Value::as_array)
            .map(|tests| {
                tests
                    .iter()
                    .map(|test| Subtest {
                        name: test
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        status: test.get("status").and_then(Value::as_i64).unwrap_or(1),
                        message: test
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        stack: test
                            .get("stack")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path: tc.path.clone(),
            url: tc.url.clone(),
            harness_status,
            harness_message,
            error: None,
            subtests,
            console: Vec::new(),
            exceptions: Vec::new(),
            duration_ms: 0,
        }
    }

    pub fn timeout(tc: &TestCase) -> Self {
        Self::stub(tc, -1, "no result before timeout")
    }

    pub fn runner_error(tc: &TestCase, message: String) -> Self {
        let mut result = Self::stub(tc, -2, "");
        result.error = Some(message);
        result
    }

    pub fn unsupported(tc: &TestCase, reason: String) -> Self {
        Self::stub(tc, -3, &reason)
    }

    fn stub(tc: &TestCase, harness_status: i64, error: &str) -> Self {
        Self {
            path: tc.path.clone(),
            url: tc.url.clone(),
            harness_status,
            harness_message: None,
            error: (!error.is_empty()).then(|| error.to_string()),
            subtests: Vec::new(),
            console: Vec::new(),
            exceptions: Vec::new(),
            duration_ms: 0,
        }
    }

    pub fn pass(&self) -> usize {
        self.subtests.iter().filter(|test| test.status == 0).count()
    }

    pub fn total(&self) -> usize {
        self.subtests.len()
    }

    pub fn ok(&self) -> bool {
        self.harness_status == 0 && self.subtests.iter().all(|test| test.status == 0)
    }

    pub fn unsupported_status(&self) -> bool {
        self.harness_status == -3
    }

    fn tag(&self) -> &'static str {
        match self.harness_status {
            0 if self.ok() => "OK  ",
            0 => "FAIL",
            -1 => "TMO ",
            -2 => "ERR ",
            -3 => "UNSP",
            1 => "HERR",
            2 => "HTMO",
            3 => "PREC",
            _ => "????",
        }
    }

    pub fn line(&self) -> String {
        format!(
            "{} {:>4}/{:<4} {}",
            self.tag(),
            self.pass(),
            self.total(),
            self.path
        )
    }

    fn to_json(&self) -> Value {
        json!({
            "type": "testharness",
            "path": self.path,
            "url": self.url,
            "harness_status": self.harness_status,
            "harness_message": self.harness_message,
            "ok": self.ok(),
            "pass": self.pass(),
            "total": self.total(),
            "duration_ms": self.duration_ms,
            "error": self.error,
            "console": self.console,
            "exceptions": self.exceptions,
            "subtests": self.subtests.iter().map(|test| json!({
                "name": test.name,
                "status": test.status,
                "message": test.message,
                "stack": test.stack,
            })).collect::<Vec<_>>(),
        })
    }
}

pub struct Summary {
    pub files: usize,
    pub files_ok: usize,
    pub files_error: usize,
    pub files_unsupported: usize,
    pub subtest_pass: usize,
    pub subtest_fail: usize,
    pub subtest_total: usize,
}

pub fn summarize(results: &[FileResult]) -> Summary {
    let mut summary = Summary {
        files: results.len(),
        files_ok: 0,
        files_error: 0,
        files_unsupported: 0,
        subtest_pass: 0,
        subtest_fail: 0,
        subtest_total: 0,
    };
    for result in results {
        if result.ok() {
            summary.files_ok += 1;
        }
        if result.unsupported_status() {
            summary.files_unsupported += 1;
        } else if result.harness_status != 0 {
            summary.files_error += 1;
        }
        summary.subtest_pass += result.pass();
        summary.subtest_total += result.total();
        summary.subtest_fail += result.total() - result.pass();
    }
    summary
}

pub fn print_results(
    results: &[FileResult],
    elapsed: Duration,
    as_json: bool,
    engine_version: &str,
    wpt_revision: &str,
) {
    let summary = summarize(results);
    if as_json {
        let output = json!({
            "schema_version": 2,
            "profile": "no-render",
            "engine_version": engine_version,
            "wpt_revision": wpt_revision,
            "elapsed_ms": elapsed.as_millis() as u64,
            "summary": {
                "files": summary.files,
                "files_ok": summary.files_ok,
                "files_error": summary.files_error,
                "files_unsupported": summary.files_unsupported,
                "subtest_pass": summary.subtest_pass,
                "subtest_fail": summary.subtest_fail,
                "subtest_total": summary.subtest_total,
            },
            "results": results.iter().map(FileResult::to_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
        return;
    }

    let pct = |n: usize, d: usize| {
        if d == 0 {
            100.0
        } else {
            n as f64 * 100.0 / d as f64
        }
    };
    eprintln!("------------------------------------------------------------");
    eprintln!(
        "files:    {}/{} ok ({:.1}%), {} errors, {} unsupported",
        summary.files_ok,
        summary.files,
        pct(summary.files_ok, summary.files),
        summary.files_error,
        summary.files_unsupported,
    );
    eprintln!(
        "subtests: {}/{} pass ({:.1}%), {} fail",
        summary.subtest_pass,
        summary.subtest_total,
        pct(summary.subtest_pass, summary.subtest_total),
        summary.subtest_fail,
    );
    eprintln!("elapsed:  {:.1}s", elapsed.as_secs_f64());
}
