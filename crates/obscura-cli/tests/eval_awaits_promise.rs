//! `fetch --eval` prints the settled value of a returned Promise (#693; #697
//! fixed only the worker path), while synchronous results are unchanged.

use std::process::Command;

const PAGE: &str = "data:text/html,<title>T</title>";

fn eval(expression: &str, timeout: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["fetch", PAGE, "--quiet", "--timeout", timeout, "--eval", expression])
        .output()
        .expect("run obscura fetch");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn async_eval_prints_the_resolved_value() {
    assert_eq!(eval("(async () => { await new Promise(r => setTimeout(r, 100)); return document.title; })()", "10"), "T");
    assert_eq!(eval("Promise.resolve('done')", "10"), "done");
}

#[test]
fn rejected_or_unsettled_promises_print_null_within_the_timeout() {
    assert_eq!(eval("Promise.reject(new Error('no'))", "10"), "null");
    let started = std::time::Instant::now();
    assert_eq!(eval("new Promise(() => {})", "2"), "null");
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
}

#[test]
fn synchronous_eval_results_are_unchanged() {
    assert_eq!(eval("1+1", "10"), "2.0");
    assert_eq!(eval("var a = 1; return a + 1", "10"), "2.0");
    assert_eq!(eval("document.title;", "10"), "T");
    assert_eq!(eval("throw new Error('x')", "10"), "null");
    assert_eq!(eval("[1,{b:2}]", "10"), "[1,{\"b\":2}]");
}
