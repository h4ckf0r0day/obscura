//! triage: the bug catcher.
//!
//! Reads one or more `wpt-runner --json` reports and collapses a wall of
//! failures into a ranked, deduplicated list of root causes. The point is to
//! answer "what handful of bugs would fix the most tests, and which crate owns
//! them" without scrolling through thousands of subtest lines by hand.
//!
//! Input is the wpt-runner JSON contract:
//!   { "elapsed_ms", "summary", "results":[ {
//!       "path","url","harness_status","harness_message","ok","pass","total",
//!       "duration_ms","error","console":[..],"exceptions":[..],
//!       "subtests":[ {"name","status","message","stack"} ] } ] }
//!
//! Subtest status 0 is PASS, anything else is a failure. Harness status 0 is OK;
//! runner codes -1 and -2 are timeout/error, while -3 is unsupported and is
//! reported separately rather than counted as a conformance failure.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde_json::Value;

mod tiers;
use tiers::{TierTotals, Tiers};

#[derive(Parser)]
#[command(
    name = "triage",
    about = "Rank and deduplicate WPT failures from wpt-runner --json output into actionable root causes."
)]
struct Args {
    /// wpt-runner --json files. If none are given, read JSON from stdin.
    inputs: Vec<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Markdown)]
    format: Format,

    /// Show at most this many error signatures.
    #[arg(long, default_value_t = 30)]
    top: usize,

    /// Only show signatures hit at least this many times.
    #[arg(long, default_value_t = 1)]
    min_count: usize,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Markdown,
    Json,
}

/// One failing thing: either a failed subtest or a file whose harness errored.
struct Record {
    spec_area: String,
    path: String,
    subtest: String,
    #[allow(dead_code)]
    status: i64,
    message: String,
    /// First captured uncaught exception for the owning file, if any.
    exception: Option<String>,
}

/// A group of records that share a normalized signature.
struct Signature {
    text: String,
    count: usize,
    areas: BTreeMap<String, ()>,
    samples: Vec<(String, String)>,
    exception: Option<String>,
}

fn main() {
    let args = Args::parse();
    if let Err(e) = run(&args) {
        eprintln!("triage: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<()> {
    let tier_rules = Tiers::load();
    let mut records: Vec<Record> = Vec::new();
    let mut totals = Totals::default();
    let mut tier_totals = TierTotals::default();

    if args.inputs.is_empty() {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading stdin")?;
        match parse_report(&buf) {
            Ok(v) => ingest(&v, &mut records, &mut totals, &tier_rules, &mut tier_totals),
            Err(e) => eprintln!("triage: warning: skipping stdin: {e:#}"),
        }
    } else {
        for input in &args.inputs {
            let buf = match std::fs::read_to_string(input) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("triage: warning: skipping {}: {e}", input.display());
                    continue;
                }
            };
            match parse_report(&buf) {
                Ok(v) => ingest(&v, &mut records, &mut totals, &tier_rules, &mut tier_totals),
                Err(e) => {
                    eprintln!("triage: warning: skipping {}: {e:#}", input.display());
                    continue;
                }
            }
        }
    }

    let signatures = group(&records);
    let by_area = area_breakdown(&records);

    match args.format {
        Format::Markdown => print_markdown(&totals, &tier_totals, &signatures, &by_area, args),
        Format::Json => print_json(&totals, &tier_totals, &signatures, &by_area, args),
    }
    Ok(())
}

/// Parse a report file into a JSON value. `ingest` then accepts either a full
/// report object (with a "results" array) or a bare array of result objects.
fn parse_report(buf: &str) -> Result<Value> {
    let v: Value = serde_json::from_str(buf).context("not valid JSON")?;
    Ok(v)
}

#[derive(Default)]
struct Totals {
    files: usize,
    failing_files: usize,
    unsupported_files: usize,
    failing_subtests: usize,
}

fn ingest(
    report: &Value,
    records: &mut Vec<Record>,
    totals: &mut Totals,
    tier_rules: &Tiers,
    tier_totals: &mut TierTotals,
) {
    let results = report
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| report.as_array());
    let results = match results {
        Some(r) => r,
        None => {
            eprintln!("triage: warning: input has no \"results\" array, ignoring");
            return;
        }
    };

    for res in results {
        totals.files += 1;
        let path = res
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let spec_area = first_segment(path);
        let harness_status = res
            .get("harness_status")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        if harness_status == -3 {
            totals.unsupported_files += 1;
            continue;
        }

        // Conformance-tier accounting, using the runner's per-file pass/total/ok.
        {
            let pass = res.get("pass").and_then(Value::as_u64).unwrap_or(0) as usize;
            let total = res.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
            let ok = res.get("ok").and_then(Value::as_bool).unwrap_or(false);
            tier_totals.record(tier_rules.tier_for(path), ok, pass, total);
        }

        // First captured exception is usually the real root cause: an uncaught
        // throw makes every downstream assert fail with a generic message.
        let exception = res
            .get("exceptions")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find_map(|e| e.as_str()))
            .map(str::to_string);

        let mut file_failed = harness_status != 0;

        // Subtest-level failures.
        if let Some(subs) = res.get("subtests").and_then(Value::as_array) {
            for sub in subs {
                let status = sub.get("status").and_then(Value::as_i64).unwrap_or(0);
                if status == 0 {
                    continue;
                }
                file_failed = true;
                totals.failing_subtests += 1;
                let name = sub
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let message = sub
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                records.push(Record {
                    spec_area: spec_area.clone(),
                    path: path.to_string(),
                    subtest: name,
                    status,
                    message,
                    exception: exception.clone(),
                });
            }
        }

        // File-level failure: the harness itself never reported clean.
        if harness_status != 0 {
            let message = res
                .get("harness_message")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    res.get("error")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or("<harness error>")
                .to_string();
            records.push(Record {
                spec_area: spec_area.clone(),
                path: path.to_string(),
                subtest: "<harness>".to_string(),
                status: harness_status,
                message,
                exception: exception.clone(),
            });
        }

        if file_failed {
            totals.failing_files += 1;
        }
    }
}

fn first_segment(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    let seg = trimmed.split('/').next().unwrap_or("");
    if seg.is_empty() {
        "unknown".to_string()
    } else {
        seg.to_string()
    }
}

/// Collapse the raw message (or exception) into a stable, comparable template.
///
/// We keep human readability but rip out anything test-specific so that "a
/// thousand asserts that all failed the same way" land in one bucket: quoted
/// values become `<v>`, numbers and hex become `N`, and the noisy
/// "expected X but got Y" tail is folded into a fixed template.
fn normalize(raw: &str) -> String {
    // First non-empty line only.
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();

    let mut s = replace_quoted(&line);
    s = replace_numbers(&s);
    s = collapse_ws(&s);
    s = fold_expected_got(&s);

    if s.chars().count() > 160 {
        s = s.chars().take(157).collect::<String>() + "...";
    }
    if s.is_empty() {
        "<empty message>".to_string()
    } else {
        s
    }
}

/// Replace single- and double-quoted runs with `<v>`.
fn replace_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            // Consume until the matching close quote (or end of string).
            while let Some(&n) = chars.peek() {
                chars.next();
                if n == quote {
                    break;
                }
            }
            out.push_str("<v>");
        } else {
            out.push(c);
        }
    }
    out
}

/// Replace 0x-hex literals and plain digit runs with `N`. Hex is handled first
/// so the leading `0` does not get eaten as a separate number.
fn replace_numbers(s: &str) -> String {
    let bytes: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        // 0x-hex
        if c == '0'
            && i + 1 < bytes.len()
            && (bytes[i + 1] == 'x' || bytes[i + 1] == 'X')
            && i + 2 < bytes.len()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            i += 2;
            while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            out.push('N');
            continue;
        }
        if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.') {
                i += 1;
            }
            out.push('N');
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Fold the variable "expected ... but got ..." tail of testharness asserts into
/// a fixed template so wording differences do not split a bucket.
fn fold_expected_got(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    if let Some(assert_end) = lower.find(':') {
        let head = &s[..assert_end];
        let head_lower = head.to_ascii_lowercase();
        if head_lower.starts_with("assert_") {
            let tail = &lower[assert_end..];
            if tail.contains("expected") && tail.contains("but got") {
                return format!("{}: expected <v> but got <v>", head);
            }
            if tail.contains("expected") {
                return format!("{}: expected <v>", head);
            }
        }
    }
    s.to_string()
}

/// Map a signature to the Obscura crate that most likely owns the bug. First
/// keyword match wins; keywords are matched case-insensitively as substrings.
fn subsystem_for(signature: &str, exception: Option<&str>) -> &'static str {
    let mut hay = signature.to_ascii_lowercase();
    if let Some(e) = exception {
        hay.push(' ');
        hay.push_str(&e.to_ascii_lowercase());
    }
    let has = |needles: &[&str]| needles.iter().any(|n| hay.contains(*n));

    // Order matters. Check the distinctive named surfaces (fetch, storage,
    // events, css, event loop) first, then the specific DOM method names, and
    // only then the broad DOM nouns. The broad nouns ("node", "element",
    // "document") and "url" are common enough in unrelated messages that they
    // must sit last, behind word-boundary matching, or they swallow failures
    // that really belong to another subsystem.
    if has(&[
        "fetch",
        "headers",
        "request",
        "response",
        "xmlhttprequest",
        "blob",
        "formdata",
        "urlsearchparams",
    ]) || has_word(&hay, &["url"])
    {
        return "obscura-net / bootstrap.js fetch shim";
    }
    if has(&[
        "localstorage",
        "sessionstorage",
        "indexeddb",
        "storage",
        "cookie",
    ]) {
        return "storage (bootstrap.js / obscura-net cookie jar)";
    }
    if has(&[
        "addeventlistener",
        "dispatchevent",
        "customevent",
        "eventtarget",
        "mutationobserver",
    ]) {
        return "events (bootstrap.js)";
    }
    if has(&[
        "getcomputedstyle",
        "cssstyledeclaration",
        "style.",
        "css.",
        "matchmedia",
    ]) {
        return "css (obscura-dom style / bootstrap.js)";
    }
    if has(&[
        "promise",
        "settimeout",
        "queuemicrotask",
        "async",
        "await",
        "microtask",
    ]) {
        return "obscura-js event loop";
    }
    if has(&[
        "queryselector",
        "getelementbyid",
        "childnodes",
        "innerhtml",
        "appendchild",
        "insertbefore",
        "attribute",
        "classname",
        "classlist",
    ]) || has_word(&hay, &["node", "element", "document"])
    {
        return "obscura-dom (tree.rs / bootstrap.js)";
    }
    if has(&[
        "is not a function",
        "is not defined",
        "cannot read properties",
        "undefined is not",
    ]) {
        return "missing/incomplete JS API (bootstrap.js)";
    }
    "unclassified"
}

/// Whole-word-ish containment for the broad DOM nouns so "node"/"element"/
/// "document" do not match inside unrelated words.
fn has_word(hay: &str, words: &[&str]) -> bool {
    let bytes = hay.as_bytes();
    for w in words {
        let mut start = 0;
        while let Some(pos) = hay[start..].find(w) {
            let at = start + pos;
            let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
            let after = at + w.len();
            let after_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
            start = at + 1;
        }
    }
    false
}

fn group(records: &[Record]) -> Vec<Signature> {
    let mut map: BTreeMap<String, Signature> = BTreeMap::new();
    for r in records {
        // The exception is the better root-cause signal when present.
        let basis = r.exception.as_deref().unwrap_or(&r.message);
        let sig = normalize(basis);
        let entry = map.entry(sig.clone()).or_insert_with(|| Signature {
            text: sig.clone(),
            count: 0,
            areas: BTreeMap::new(),
            samples: Vec::new(),
            exception: None,
        });
        entry.count += 1;
        entry.areas.insert(r.spec_area.clone(), ());
        if entry.samples.len() < 3 {
            entry.samples.push((r.path.clone(), r.subtest.clone()));
        }
        if entry.exception.is_none() {
            if let Some(e) = &r.exception {
                entry.exception = Some(first_line(e));
            }
        }
    }
    let mut v: Vec<Signature> = map.into_values().collect();
    // Most-frequent first; ties broken by signature text for a stable order.
    v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.text.cmp(&b.text)));
    v
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

struct AreaStat {
    area: String,
    subtest_fail: usize,
    files: usize,
}

fn area_breakdown(records: &[Record]) -> Vec<AreaStat> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut files: BTreeMap<String, BTreeMap<String, ()>> = BTreeMap::new();
    for r in records {
        *counts.entry(r.spec_area.clone()).or_insert(0) += 1;
        files
            .entry(r.spec_area.clone())
            .or_default()
            .insert(r.path.clone(), ());
    }
    let mut v: Vec<AreaStat> = counts
        .into_iter()
        .map(|(area, subtest_fail)| {
            let files = files.get(&area).map(|m| m.len()).unwrap_or(0);
            AreaStat {
                area,
                subtest_fail,
                files,
            }
        })
        .collect();
    v.sort_by(|a, b| {
        b.subtest_fail
            .cmp(&a.subtest_fail)
            .then_with(|| a.area.cmp(&b.area))
    });
    v
}

fn areas_string(sig: &Signature) -> String {
    sig.areas.keys().cloned().collect::<Vec<_>>().join(", ")
}

fn print_markdown(
    totals: &Totals,
    tier_totals: &TierTotals,
    signatures: &[Signature],
    by_area: &[AreaStat],
    args: &Args,
) {
    let shown: Vec<&Signature> = signatures
        .iter()
        .filter(|s| s.count >= args.min_count)
        .take(args.top)
        .collect();

    println!(
        "WPT triage: {} files, {} failing files, {} unsupported files, {} failing subtests.",
        totals.files, totals.failing_files, totals.unsupported_files, totals.failing_subtests
    );
    println!();

    print_tiers_md(tier_totals);

    println!("## Top error signatures");
    println!();
    if shown.is_empty() {
        println!("No failures matched the current filters.");
        println!();
    } else {
        println!("| # | count | suspected subsystem | areas | signature |");
        println!("| - | ----- | ------------------- | ----- | --------- |");
        for (i, sig) in shown.iter().enumerate() {
            let subsystem = subsystem_for(&sig.text, sig.exception.as_deref());
            println!(
                "| {} | {} | {} | {} | {} |",
                i + 1,
                sig.count,
                subsystem,
                md_cell(&areas_string(sig)),
                md_cell(&sig.text),
            );
        }
        println!();

        println!("### samples");
        println!();
        for (i, sig) in shown.iter().take(10).enumerate() {
            println!("{}. `{}`", i + 1, sig.text);
            for (path, subtest) in &sig.samples {
                println!("   - {}::{}", path, subtest);
            }
            if let Some(e) = &sig.exception {
                println!("   - exception: `{}`", e);
            }
        }
        println!();
    }

    println!("## By spec area");
    println!();
    if by_area.is_empty() {
        println!("No failing subtests.");
    } else {
        println!("| area | failing subtests | files affected |");
        println!("| ---- | ---------------- | -------------- |");
        for a in by_area {
            println!("| {} | {} | {} |", a.area, a.subtest_fail, a.files);
        }
    }
    println!();

    println!("## Suspected fix areas (ranked by failing-subtest count)");
    println!();
    let fix_ranked = fix_area_ranking(signatures);
    if fix_ranked.is_empty() {
        println!("Nothing to rank.");
    } else {
        for (subsystem, count) in fix_ranked {
            println!("- {} ({} failing subtests)", subsystem, count);
        }
    }
}

/// Render the conformance-tier table. Core is the scraping/automation contract
/// and the headline; Relevant is Core plus broader JS-observable correctness;
/// Full is the whole run. Excluded and Other are shown for transparency only.
/// See crates/triage/src/tiers.list for the (capability-based) definitions.
fn print_tiers_md(t: &TierTotals) {
    let core = t.core();
    let relevant = t.relevant();
    let full = t.full();
    println!("## Conformance tiers");
    println!();
    println!("| tier | files ok | subtests pass | role |");
    println!("| ---- | -------- | ------------- | ---- |");
    let row = |name: &str, a: &tiers::TierAgg, role: &str| {
        println!(
            "| {} | {}/{} ({:.1}%) | {}/{} ({:.1}%) | {} |",
            name,
            a.files_ok,
            a.files,
            a.file_pct(),
            a.pass,
            a.total,
            a.subtest_pct(),
            role,
        );
    };
    row("Core", &core, "headline + SLA");
    row("Relevant", &relevant, "Core + broader correctness");
    row("Full", &full, "whole suite (transparency)");
    if t.excluded.files > 0 {
        row("(excluded)", &t.excluded, "out of scope, no target");
    }
    if t.other.files > 0 {
        row(
            "(unclassified)",
            &t.other,
            "not yet tiered, extend manifest",
        );
    }
    println!();
}

/// Sum signature counts per suspected subsystem, biggest first.
fn fix_area_ranking(signatures: &[Signature]) -> Vec<(&'static str, usize)> {
    let mut map: BTreeMap<&'static str, usize> = BTreeMap::new();
    for sig in signatures {
        let subsystem = subsystem_for(&sig.text, sig.exception.as_deref());
        *map.entry(subsystem).or_insert(0) += sig.count;
    }
    let mut v: Vec<(&'static str, usize)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v
}

/// Escape the bits of a string that would break a markdown table cell.
fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn tier_json(a: &tiers::TierAgg) -> Value {
    serde_json::json!({
        "files": a.files,
        "files_ok": a.files_ok,
        "file_pct": (a.file_pct() * 10.0).round() / 10.0,
        "subtest_pass": a.pass,
        "subtest_total": a.total,
        "subtest_pct": (a.subtest_pct() * 10.0).round() / 10.0,
    })
}

fn print_json(
    totals: &Totals,
    tier_totals: &TierTotals,
    signatures: &[Signature],
    by_area: &[AreaStat],
    args: &Args,
) {
    let shown: Vec<&Signature> = signatures
        .iter()
        .filter(|s| s.count >= args.min_count)
        .take(args.top)
        .collect();

    let sigs_json: Vec<Value> = shown
        .iter()
        .enumerate()
        .map(|(i, sig)| {
            let subsystem = subsystem_for(&sig.text, sig.exception.as_deref());
            serde_json::json!({
                "rank": i + 1,
                "count": sig.count,
                "signature": sig.text,
                "subsystem": subsystem,
                "areas": sig.areas.keys().cloned().collect::<Vec<_>>(),
                "samples": sig.samples.iter().map(|(p, s)| serde_json::json!({
                    "path": p,
                    "subtest": s,
                })).collect::<Vec<_>>(),
                "exception": sig.exception,
            })
        })
        .collect();

    let by_area_json: Vec<Value> = by_area
        .iter()
        .map(|a| {
            serde_json::json!({
                "area": a.area,
                "subtest_fail": a.subtest_fail,
                "files": a.files,
            })
        })
        .collect();

    let out = serde_json::json!({
        "totals": {
            "files": totals.files,
            "failing_files": totals.failing_files,
            "unsupported_files": totals.unsupported_files,
            "failing_subtests": totals.failing_subtests,
        },
        "tiers": {
            "core": tier_json(&tier_totals.core()),
            "relevant": tier_json(&tier_totals.relevant()),
            "full": tier_json(&tier_totals.full()),
            "excluded": tier_json(&tier_totals.excluded),
            "other": tier_json(&tier_totals.other),
        },
        "signatures": sigs_json,
        "by_area": by_area_json,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unsupported_files_are_not_counted_as_failures_or_tier_results() {
        let report = json!({"results": [{
            "path": "css/example.html",
            "harness_status": -3,
            "ok": false,
            "pass": 0,
            "total": 0
        }]});
        let mut records = Vec::new();
        let mut totals = Totals::default();
        let mut tiers = TierTotals::default();
        ingest(
            &report,
            &mut records,
            &mut totals,
            &Tiers::load(),
            &mut tiers,
        );

        assert_eq!(totals.files, 1);
        assert_eq!(totals.unsupported_files, 1);
        assert_eq!(totals.failing_files, 0);
        assert!(records.is_empty());
        assert_eq!(tiers.full().files, 0);
    }
}
