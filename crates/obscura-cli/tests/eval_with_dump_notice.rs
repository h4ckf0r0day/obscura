//! Issue #1542: `fetch --eval` combined with `--dump` or `--selector` prints
//! the page rather than the expression's value (by design, issue #248). That
//! must not happen silently.

use std::process::{Command, Output};

const PAGE: &str = "data:text/html,<html><body><p class=quote>quoted</p></body></html>";
const NOTICE: &str = "Note: --eval ran, but";

fn fetch(extra: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["fetch", PAGE, "--quiet", "--eval", "1+1"])
        .args(extra)
        .output()
        .expect("run obscura fetch");
    assert!(
        output.status.success(),
        "obscura fetch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn bare_eval_prints_its_value_without_a_notice() {
    let output = fetch(&[]);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "2.0");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(NOTICE));
}

#[test]
fn eval_with_dump_prints_the_page_and_says_so() {
    let output = fetch(&["--dump", "text"]);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "quoted");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(NOTICE) && stderr.contains("--dump"), "{stderr}");
}

#[test]
fn eval_with_selector_prints_the_page_and_says_so() {
    let output = fetch(&["--selector", ".quote"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("<p class=\"quote\">quoted</p>"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(NOTICE) && stderr.contains("--selector"), "{stderr}");
}
