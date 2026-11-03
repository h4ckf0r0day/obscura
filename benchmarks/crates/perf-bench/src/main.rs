use std::fs;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "perf-bench",
    about = "Times Obscura CLI fetch performance: per-url latency and scrape throughput."
)]
struct Args {
    /// URLs to benchmark. Mutually exclusive with --urls-file.
    urls: Vec<String>,

    /// File with one url per line. Lines starting with '#' are ignored.
    #[arg(long)]
    urls_file: Option<String>,

    /// Path or command name of the obscura binary.
    #[arg(long, default_value = "obscura")]
    binary: String,

    /// What the fetch should produce.
    #[arg(long, default_value = "text")]
    mode: Mode,

    /// JS expression for --mode eval. Required when mode is eval.
    #[arg(long)]
    eval: Option<String>,

    /// Timed runs per url.
    #[arg(long, default_value_t = 5)]
    runs: u32,

    /// Discarded warmup runs per url before timing.
    #[arg(long, default_value_t = 1)]
    warmup: u32,

    /// Per-fetch timeout in seconds, passed to obscura --timeout.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Emit machine readable JSON instead of a table.
    #[arg(long)]
    json: bool,

    /// Throughput mode: one scrape over all urls instead of per-url latency.
    #[arg(long)]
    scrape: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    Text,
    Markdown,
    Html,
    Links,
    Original,
    Eval,
}

impl Mode {
    fn as_dump(self) -> &'static str {
        match self {
            Mode::Text => "text",
            Mode::Markdown => "markdown",
            Mode::Html => "html",
            Mode::Links => "links",
            Mode::Original => "original",
            Mode::Eval => "eval",
        }
    }
}

#[derive(Serialize)]
struct UrlResult {
    url: String,
    runs: usize,
    fails: usize,
    min_ms: f64,
    median_ms: f64,
    mean_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Serialize)]
struct ScrapeResult {
    urls: usize,
    total_ms: f64,
    urls_per_sec: f64,
}

#[derive(Serialize)]
struct Report {
    binary: String,
    mode: String,
    runs: u32,
    warmup: u32,
    results: Vec<UrlResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scrape: Option<ScrapeResult>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.mode == Mode::Eval && args.eval.is_none() {
        bail!("--mode eval requires --eval <expr>");
    }

    let urls = collect_urls(&args)?;
    if urls.is_empty() {
        bail!("no urls given; pass urls as arguments or use --urls-file");
    }

    verify_binary(&args.binary)?;

    if args.scrape {
        let scrape = run_scrape(&args, &urls);
        let report = Report {
            binary: args.binary.clone(),
            mode: args.mode.as_dump().to_string(),
            runs: args.runs,
            warmup: args.warmup,
            results: Vec::new(),
            scrape,
        };
        emit(&args, &report);
        return Ok(());
    }

    let mut results = Vec::with_capacity(urls.len());
    for url in &urls {
        results.push(bench_url(&args, url));
    }

    let report = Report {
        binary: args.binary.clone(),
        mode: args.mode.as_dump().to_string(),
        runs: args.runs,
        warmup: args.warmup,
        results,
        scrape: None,
    };
    emit(&args, &report);
    Ok(())
}

fn collect_urls(args: &Args) -> Result<Vec<String>> {
    if let Some(path) = &args.urls_file {
        if !args.urls.is_empty() {
            bail!("pass urls as arguments or --urls-file, not both");
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading urls file {path}"))?;
        let urls = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.to_string())
            .collect();
        Ok(urls)
    } else {
        Ok(args.urls.clone())
    }
}

fn verify_binary(binary: &str) -> Result<()> {
    let status = Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("`{binary} --version` exited with {s}; is this the obscura binary?"),
        Err(e) => bail!("could not run `{binary} --version`: {e}; check --binary path or PATH"),
    }
}

/// Build the per-fetch command for a single url.
fn fetch_command(args: &Args, url: &str) -> Command {
    let mut cmd = Command::new(&args.binary);
    cmd.arg("fetch").arg(url);
    if args.mode == Mode::Eval {
        // safe: validated in main that eval is Some when mode is eval
        cmd.arg("--eval").arg(args.eval.as_deref().unwrap_or(""));
    } else {
        cmd.arg("--dump").arg(args.mode.as_dump());
    }
    cmd.arg("--quiet")
        .arg("--timeout")
        .arg(args.timeout.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// Run one cold process and return (elapsed_ms, success).
fn timed_run(mut cmd: Command) -> (f64, bool) {
    let start = Instant::now();
    let status = cmd.status();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let ok = matches!(status, Ok(s) if s.success());
    (elapsed, ok)
}

fn bench_url(args: &Args, url: &str) -> UrlResult {
    for _ in 0..args.warmup {
        let (_, _) = timed_run(fetch_command(args, url));
    }

    let mut times = Vec::with_capacity(args.runs as usize);
    let mut fails = 0usize;
    for _ in 0..args.runs {
        let (ms, ok) = timed_run(fetch_command(args, url));
        if ok {
            times.push(ms);
        } else {
            fails += 1;
        }
    }

    if times.is_empty() {
        return UrlResult {
            url: url.to_string(),
            runs: 0,
            fails,
            min_ms: 0.0,
            median_ms: 0.0,
            mean_ms: 0.0,
            p95_ms: 0.0,
            max_ms: 0.0,
        };
    }

    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = times.len();
    let sum: f64 = times.iter().sum();

    UrlResult {
        url: url.to_string(),
        runs: n,
        fails,
        min_ms: times[0],
        median_ms: median(&times),
        mean_ms: sum / n as f64,
        p95_ms: percentile(&times, 95.0),
        max_ms: times[n - 1],
    }
}

/// Median of an already-sorted slice.
fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Nearest-rank percentile over an already-sorted slice.
fn percentile(sorted: &[f64], pct: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return sorted[0];
    }
    let rank = (pct / 100.0 * n as f64).ceil() as usize;
    let idx = rank.clamp(1, n) - 1;
    sorted[idx]
}

fn run_scrape(args: &Args, urls: &[String]) -> Option<ScrapeResult> {
    let mut cmd = Command::new(&args.binary);
    cmd.arg("scrape");
    for url in urls {
        cmd.arg(url);
    }
    cmd.arg("--quiet")
        .arg("--format")
        .arg("json")
        .arg("--timeout")
        .arg(args.timeout.to_string());
    if args.mode == Mode::Eval {
        cmd.arg("--eval").arg(args.eval.as_deref().unwrap_or(""));
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::inherit());

    let start = Instant::now();
    let status = cmd.status();
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;

    match status {
        Ok(s) if s.success() => {
            let secs = total_ms / 1000.0;
            let urls_per_sec = if secs > 0.0 {
                urls.len() as f64 / secs
            } else {
                0.0
            };
            Some(ScrapeResult {
                urls: urls.len(),
                total_ms,
                urls_per_sec,
            })
        }
        Ok(s) => {
            eprintln!("scrape failed: obscura exited with {s}");
            None
        }
        Err(e) => {
            eprintln!("scrape failed: could not run obscura scrape: {e}");
            None
        }
    }
}

fn emit(args: &Args, report: &Report) {
    if args.json {
        match serde_json::to_string_pretty(report) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to serialize report: {e}"),
        }
        return;
    }
    print_table(report);
}

fn print_table(report: &Report) {
    println!(
        "obscura perf: binary={} mode={} runs={} warmup={}",
        report.binary, report.mode, report.runs, report.warmup
    );

    if let Some(scrape) = &report.scrape {
        println!();
        println!(
            "scrape: {} urls in {:.1} ms ({:.2} urls/sec)",
            scrape.urls, scrape.total_ms, scrape.urls_per_sec
        );
        return;
    }

    println!();
    println!(
        "{:<40} {:>5} {:>9} {:>9} {:>9} {:>9} {:>5}",
        "url", "runs", "min", "median", "p95", "max", "fails"
    );
    println!("{}", "-".repeat(40 + 6 + 10 * 4 + 6));

    for r in &report.results {
        let url = truncate(&r.url, 40);
        if r.runs == 0 {
            println!(
                "{:<40} {:>5} {:>9} {:>9} {:>9} {:>9} {:>5}",
                url, 0, "-", "-", "-", "-", r.fails
            );
        } else {
            println!(
                "{:<40} {:>5} {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>5}",
                url, r.runs, r.min_ms, r.median_ms, r.p95_ms, r.max_ms, r.fails
            );
        }
    }

    let mut medians: Vec<f64> = report
        .results
        .iter()
        .filter(|r| r.runs > 0)
        .map(|r| r.median_ms)
        .collect();
    medians.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let total_fails: usize = report.results.iter().map(|r| r.fails).sum();

    println!();
    if medians.is_empty() {
        println!("overall: no successful runs ({} failures)", total_fails);
    } else {
        println!(
            "overall median-of-medians: {:.1} ms across {} urls ({} failures)",
            median(&medians),
            medians.len(),
            total_fails
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(3);
        let head: String = s.chars().take(keep).collect();
        format!("{head}...")
    }
}
