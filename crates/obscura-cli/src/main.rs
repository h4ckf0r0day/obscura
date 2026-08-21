use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, Subcommand};
use obscura_browser::{BrowserContext, Page};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};

#[derive(Parser)]
#[command(
    name = "obscura",
    version = env!("OBSCURA_BUILD_VERSION"),
    about = "Obscura - A lightweight headless browser for web scraping and automation",
)]
struct Args {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long, default_value_t = 9222)]
    port: u16,

    #[arg(long, global = true)]
    proxy: Option<String>,

    /// Enable stealth mode (consistent browser fingerprint, and with the
    /// `stealth` build feature, TLS impersonation plus tracker blocking).
    /// Global: applies to fetch, serve, scrape, and mcp.
    #[arg(long, global = true)]
    stealth: bool,

    /// Respect robots.txt before navigating to an HTTP(S) URL.
    /// Global: applies to fetch and scrape.
    #[arg(long, global = true)]
    obey_robots: bool,

    #[arg(long)]
    user_agent: Option<String>,

    #[arg(long)]
    storage_dir: Option<std::path::PathBuf>,

    /// Permit fetches to loopback, RFC1918, and link-local addresses.
    /// Default is to block them (SSRF fix from #4). Use this for local
    /// development against http://localhost:N or http://192.168.x.y.
    /// Equivalent to `OBSCURA_ALLOW_PRIVATE_NETWORK=1` but per-process
    /// and survives in command pipelines.
    #[arg(long, global = true)]
    allow_private_network: bool,

    /// Pass raw flags to V8, in the same form V8/Chromium/Node accept
    /// (e.g. `"--max-old-space-size=4096 --max-semi-space-size=64 --expose-gc"`).
    /// Applied once at startup before any isolate is created.
    #[arg(long, value_name = "FLAGS", allow_hyphen_values = true)]
    v8_flags: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(short, long, default_value_t = 9222)]
        port: u16,

        // Bind address. Defaults to 127.0.0.1 (loopback only) for safety.
        // Set to 0.0.0.0 to listen on all interfaces (e.g. inside a Docker
        // container where you want the port to be reachable from the host
        // via -p mapping).
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(long)]
        proxy: Option<String>,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long, default_value_t = 1)]
        workers: u16,

        /// Maximum live CDP connections. Each connection runs on its own OS
        /// thread with its own V8 isolates, so this bounds the server's thread
        /// and memory footprint. Connections beyond the limit are refused with
        /// a 503 rather than queued.
        #[arg(long, default_value_t = obscura_cdp::DEFAULT_MAX_CONNECTIONS)]
        max_connections: usize,

        /// Allow CDP clients to navigate to file:// URLs. Off by
        /// default so a CDP connection cannot read arbitrary local
        /// files. Enable only when serving local HTML for testing
        /// and the port is on a trusted network.
        #[arg(long)]
        allow_file_access: bool,

        #[arg(long)]
        storage_dir: Option<std::path::PathBuf>,

        /// Suppress all logs (same as on `fetch`). Useful when scraping pages
        /// that flood the console with per-page script warnings (issue #264).
        #[arg(long)]
        quiet: bool,
    },

    Fetch {
        // Optional so a batch run can pass URLs via --file instead. A single
        // positional URL keeps the original one-shot behaviour.
        url: Option<String>,

        // Default is html. Kept as Option so we can tell whether --dump was
        // explicitly passed: a bare --eval returns its own value, while --eval
        // combined with --dump (or --selector) runs the eval, lets its async
        // work settle, then reads the page (issue #248).
        #[arg(long)]
        dump: Option<DumpFormat>,

        /// Read newline-delimited URLs from a file (one per line; blank lines
        /// and lines starting with `#` are skipped). Use `-` for stdin. Enables
        /// batch mode: every URL is fetched raw (--dump original) and one JSON
        /// status line is printed per URL. For rendered/DOM batch output use
        /// `scrape` instead (issue #349).
        #[arg(long)]
        file: Option<std::path::PathBuf>,

        /// Number of URLs fetched concurrently in batch mode. Ignored without
        /// --file.
        #[arg(long, default_value_t = std::num::NonZeroUsize::new(1).unwrap())]
        concurrency: std::num::NonZeroUsize,

        #[arg(long)]
        selector: Option<String>,

        /// Maximum adaptive post-load settle time in seconds. When supplied
        /// explicitly, this is a fixed delay; the default is a 5-second cap
        /// that returns once the page is quiescent.
        #[arg(long)]
        wait: Option<u64>,

        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        #[arg(long, default_value = "load")]
        wait_until: String,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,

        #[arg(long, short)]
        quiet: bool,

        #[arg(long)]
        storage_dir: Option<std::path::PathBuf>,

        /// Capture the settled page as a PNG. Requires the `render` feature.
        #[arg(long, short = 's', value_name = "FILE", conflicts_with = "file")]
        screenshot: Option<std::path::PathBuf>,
    },

    Scrape {
        urls: Vec<String>,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, default_value_t = std::num::NonZeroUsize::new(10).unwrap())]
        concurrency: std::num::NonZeroUsize,

        #[arg(long, default_value = "json")]
        format: String,

        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        #[arg(long, short)]
        quiet: bool,
    },

    Mcp {
        #[arg(long)]
        http: bool,

        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        #[arg(long, default_value_t = 3000)]
        port: u16,

        #[arg(long)]
        proxy: Option<String>,

        #[arg(long)]
        user_agent: Option<String>,
    },
}

#[derive(Clone, Debug, clap::ValueEnum, PartialEq, Eq)]
enum DumpFormat {
    Html,
    Text,
    Links,
    Markdown,
    /// Stream the raw HTTP response body verbatim (binary-safe).
    /// Bypasses the browser/JS layer — useful for fetching images,
    /// JSON, JS, CSS, or any non-HTML resource (cf. issue #117).
    Original,
    /// One JSON object per line listing every sub-resource URL the
    /// rendered page references (script src, link href, img src,
    /// iframe src, media sources, embed/object data). Lets callers
    /// replay the asset graph with their own HTTP client when they
    /// need the originals alongside the page (cf. issue 124).
    Assets,
    /// Dump all cookies in the browser jar as a JSON array, including
    /// HttpOnly cookies that are inaccessible via document.cookie.
    /// Useful for extracting session tokens set by anti-bot challenges.
    Cookies,
}

fn print_banner(port: u16) {
    println!(
        r#"
   ____  _                              
  / __ \| |                             
 | |  | | |__  ___  ___ _   _ _ __ __ _ 
 | |  | | '_ \/ __|/ __| | | | '__/ _` |
 | |__| | |_) \__ \ (__| |_| | | | (_| |
  \____/|_.__/|___/\___|\__,_|_|  \__,_|
                   
  Headless Browser v{}
  CDP server: ws://127.0.0.1:{}/devtools/browser
"#,
        env!("OBSCURA_BUILD_VERSION"),
        port
    );
}

fn select_log_filter(verbose: bool, quiet: bool) -> &'static str {
    if verbose {
        "debug"
    } else if quiet {
        "off"
    } else {
        "warn"
    }
}

fn is_quiet_command(cmd: &Option<Command>) -> bool {
    matches!(
        cmd,
        Some(Command::Fetch { quiet: true, .. })
            | Some(Command::Scrape { quiet: true, .. })
            | Some(Command::Serve { quiet: true, .. })
    )
}

fn merge_proxy(global_proxy: Option<String>, command_proxy: Option<String>) -> Option<String> {
    command_proxy.or(global_proxy)
}

/// Normalize a raw `--v8-flags` value into the string we'll hand to V8.
/// Returns `None` when the user didn't pass the flag, passed an empty string,
/// or passed only whitespace; in those cases V8 is left untouched.
fn normalize_v8_flags(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Default V8 flags applied at startup unless the user disabled them via
/// `--v8-flags`. The default heap matches headless Chrome (~4 GB) so pages
/// that ship heavy fingerprinting or analytics bundles
/// (e.g. demo.fingerprint.com — issue #199) don't SIGTRAP out of the box.
/// V8 parses flags left-to-right and later wins, so anything the user
/// passes via `--v8-flags` overrides these.
///
/// `--max-semi-space-size=4` caps V8's young generation (default 16 MB per
/// semi-space) so a parse/JS allocation burst does not inflate RSS, and
/// `--optimize-for-size` trades memory-heavy codegen choices for a smaller
/// footprint. Together they cut RSS ~18% on heavy pages (ycombinator.com
/// 173 MB -> 140 MB) at no measurable speed cost (V8 still JITs hot paths).
#[cfg(target_pointer_width = "64")]
const DEFAULT_V8_FLAGS: &str =
    "--max-old-space-size=4096 --max-semi-space-size=4 --optimize-for-size";
#[cfg(not(target_pointer_width = "64"))]
const DEFAULT_V8_FLAGS: &str =
    "--max-old-space-size=1024 --max-semi-space-size=4 --optimize-for-size";

fn effective_v8_flags(user: Option<&str>) -> String {
    match normalize_v8_flags(user) {
        Some(u) => format!("{} {}", DEFAULT_V8_FLAGS, u),
        None => DEFAULT_V8_FLAGS.to_string(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Pin the process timezone before V8/ICU reads it. V8 sources the zone for
    // both Date (getTimezoneOffset, toString) and Intl.DateTimeFormat from TZ; left
    // unset it defaults to UTC for Date while the page layer advertised a different
    // zone, a cross-surface mismatch fingerprinting scripts flag. Default to
    // Europe/Berlin; set OBSCURA_TIMEZONE to match the exit IP's region. An existing
    // TZ from the host is respected.
    // SAFETY: runs before any V8 isolate or worker thread starts, so the env is
    // effectively single threaded here.
    if let Some(tz) = std::env::var("OBSCURA_TIMEZONE")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        unsafe {
            std::env::set_var("TZ", tz);
        }
    } else if std::env::var_os("TZ").is_none() {
        unsafe {
            std::env::set_var("TZ", "Europe/Berlin");
        }
    }

    let quiet = is_quiet_command(&args.command);
    let filter = select_log_filter(args.verbose, quiet);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    let v8_flags = effective_v8_flags(args.v8_flags.as_deref());
    tracing::debug!("V8 flags: {}", v8_flags);
    obscura_js::set_v8_flags(&v8_flags);

    // The js-side fetch path (op_fetch_url) reads OBSCURA_ALLOW_PRIVATE_NETWORK
    // directly for its SSRF gate. Mirror the CLI flag into the env var so
    // iframe loads and JS fetch() see the same policy the http_client layer
    // already uses (issue #33).
    if args.allow_private_network {
        // SAFETY: set_var is unsafe in newer rustc; this runs before any
        // spawned thread inspects the env, so it's effectively single
        // threaded at this point.
        unsafe {
            std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
        }
    }

    let global_proxy = args.proxy.clone();
    let stealth = args.stealth;
    let obey_robots = args.obey_robots;

    match args.command {
        Some(Command::Serve {
            port,
            host,
            proxy,
            user_agent,
            workers,
            max_connections,
            allow_file_access,
            storage_dir,
            quiet: _,
        }) => {
            // Fall back to OBSCURA_PROXY so a proxy can be supplied without
            // putting credentials on the command line. The multi-worker load
            // balancer passes the proxy to each worker this way (issue #366).
            let proxy = merge_proxy(global_proxy.clone(), proxy).or_else(|| {
                std::env::var("OBSCURA_PROXY")
                    .ok()
                    .filter(|s| !s.is_empty())
            });
            print_banner(port);
            if let Some(ref dir) = storage_dir {
                tracing::info!("Storage dir: {}", dir.display());
            }
            if let Some(ref proxy) = proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            if let Some(ref ua) = user_agent {
                tracing::info!("User-Agent: {}", ua);
            }
            if stealth {
                #[cfg(feature = "stealth")]
                tracing::info!(
                    "Stealth mode enabled (TLS fingerprint impersonation + tracker blocking)"
                );
                #[cfg(not(feature = "stealth"))]
                tracing::info!("Stealth mode enabled (tracker blocking)");
            }

            if workers > 1 {
                tracing::info!("{} worker processes", workers);
                run_multi_worker_serve(port, host, workers, proxy, stealth, user_agent).await?;
            } else {
                obscura_cdp::start_with_serve_options_and_limit(
                    port,
                    &host,
                    proxy,
                    stealth,
                    user_agent,
                    allow_file_access,
                    storage_dir,
                    args.allow_private_network,
                    max_connections,
                )
                .await?;
            }
        }
        Some(Command::Fetch {
            url,
            dump,
            selector,
            wait,
            timeout,
            wait_until,
            user_agent,
            eval,
            output,
            quiet,
            storage_dir,
            file,
            concurrency,
            screenshot,
        }) => {
            if let Some(file) = file {
                if url.is_some() {
                    anyhow::bail!("Pass URLs via a positional argument or --file, not both.");
                }
                if screenshot.is_some() {
                    anyhow::bail!("--screenshot is only supported for a single URL, not --file batch mode.");
                }
                // Batch mode is raw HTTP only. Rendering each URL through the
                // browser/JS stack is what `scrape` is for.
                match dump {
                    None | Some(DumpFormat::Original) => {}
                    Some(_) => anyhow::bail!(
                        "batch mode (--file) only supports --dump original. Use `scrape` for rendered/DOM output."
                    ),
                }
                let urls = read_urls_from_file(&file)?;
                run_batch_fetch(
                    urls,
                    concurrency.get(),
                    timeout,
                    user_agent,
                    global_proxy,
                    output,
                    quiet,
                    stealth
                )
                .await?;
            } else {
                let url = url.ok_or_else(|| {
                    anyhow::anyhow!(
                        "No URL provided. Pass a URL, or a list of URLs with --file <path>."
                    )
                })?;
                let wait_is_fixed = wait.is_some();
                run_fetch(
                    &url,
                    dump,
                    selector,
                    wait.unwrap_or(5),
                    wait_is_fixed,
                    timeout,
                    &wait_until,
                    user_agent,
                    stealth,
                    eval,
                    output,
                    quiet,
                    global_proxy,
                    storage_dir,
                    args.allow_private_network,
                    obey_robots,
                    screenshot,
                )
                .await?;
            }
        }
        Some(Command::Scrape {
            urls,
            eval,
            concurrency,
            format,
            timeout,
            quiet,
        }) => {
            run_parallel_scrape(
                urls,
                eval,
                concurrency.get(),
                &format,
                timeout,
                quiet,
                global_proxy,
                stealth,
                obey_robots,
            )
            .await?;
        }
        Some(Command::Mcp {
            http,
            host,
            port,
            proxy,
            user_agent,
        }) => {
            let mcp_proxy = merge_proxy(global_proxy.clone(), proxy);
            if http {
                obscura_mcp::http::run(host, port, mcp_proxy, user_agent, stealth).await?;
            } else {
                obscura_mcp::run(mcp_proxy, user_agent, stealth).await?;
            }
        }
        None => {
            print_banner(args.port);
            if let Some(ref proxy) = args.proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            obscura_cdp::start_with_options(args.port, args.proxy, stealth).await?;
        }
    }

    Ok(())
}

async fn run_multi_worker_serve(
    port: u16,
    host: String,
    workers: u16,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::TcpListener;

    let exe = std::env::current_exe()?;
    let mut children = Vec::new();

    for i in 0..workers {
        let worker_port = port + 1 + i;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("serve").arg("--port").arg(worker_port.to_string());
        if let Some(ref p) = proxy {
            // Pass the proxy (which may embed credentials) via the environment,
            // not argv. A --proxy flag is visible in `ps`/`/proc/<pid>/cmdline`
            // to any local user; OBSCURA_PROXY is only readable by the owner
            // (issue #366). The worker's serve path reads this env as a fallback.
            cmd.env("OBSCURA_PROXY", p);
        }
        if let Some(ref ua) = user_agent {
            cmd.arg("--user-agent").arg(ua);
        }
        if stealth {
            cmd.arg("--stealth");
        }
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        let child = cmd.spawn()?;
        tracing::info!("Worker {} on port {}", i + 1, worker_port);
        children.push(child);
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Bind the load balancer to the requested host, not hardcoded loopback.
    // With --host 0.0.0.0 (e.g. in Docker) the single-worker path already binds
    // all interfaces; the multi-worker balancer must too, or the mapped port is
    // refused from outside the container (issue #336). Workers stay on loopback
    // and are only reached by the balancer.
    let listener = TcpListener::bind((host.as_str(), port)).await?;
    tracing::info!("Load balancer on {}:{}, {} workers", host, port, workers);

    let mut next_worker: u16 = 0;

    loop {
        let (client_stream, peer_addr) = listener.accept().await?;
        let worker_port = port + 1 + (next_worker % workers);
        next_worker = next_worker.wrapping_add(1);

        tracing::debug!("Routing {} to worker port {}", peer_addr, worker_port);

        let mut peek_buf = [0u8; 4];
        client_stream.peek(&mut peek_buf).await?;

        if &peek_buf == b"GET " {
            let mut full_peek = [0u8; 256];
            let n = client_stream.peek(&mut full_peek).await?;
            let request_line = String::from_utf8_lossy(&full_peek[..n]);

            if request_line.contains("/json") {
                let worker_addr = format!("127.0.0.1:{}", worker_port);
                match tokio::net::TcpStream::connect(&worker_addr).await {
                    Ok(mut worker_stream) => {
                        tokio::spawn(async move {
                            let std_stream = match client_stream.into_std() {
                                Ok(s) => s,
                                Err(e) => {
                                    tracing::error!(
                                        "/json: failed to convert client to std stream: {}",
                                        e
                                    );
                                    return;
                                }
                            };
                            let mut client = match tokio::net::TcpStream::from_std(std_stream) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::error!(
                                        "/json: failed to recreate tokio TcpStream: {}",
                                        e
                                    );
                                    return;
                                }
                            };
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut worker_stream)
                                .await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("/json worker {} unreachable: {}", worker_addr, e);
                        tokio::spawn(async move {
                            let mut s = client_stream;
                            let _ = s
                                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                                .await;
                            let _ = s.shutdown().await;
                        });
                    }
                }
                continue;
            }
        }

        let worker_addr = format!("127.0.0.1:{}", worker_port);
        tokio::spawn(async move {
            match tokio::net::TcpStream::connect(&worker_addr).await {
                Ok(mut worker_stream) => {
                    let mut client = client_stream;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut worker_stream).await;
                }
                Err(e) => {
                    tracing::warn!("worker {} unreachable: {}", worker_addr, e);
                    let mut s = client_stream;
                    let _ = s
                        .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                        .await;
                    let _ = s.shutdown().await;
                }
            }
        });
    }
}

async fn settle_page(page: &mut Page, wait_secs: u64, fixed: bool) {
    let wait_ms = wait_secs.saturating_mul(1000);
    if fixed {
        page.settle_for_duration(wait_ms).await;
    } else {
        page.settle(wait_ms).await;
    }
}

fn configure_fetch_navigation_timeout(page: &mut Page, timeout_secs: u64) {
    page.set_navigation_timeout(Duration::from_secs(timeout_secs));
}

async fn run_fetch(
    url_str: &str,
    dump: Option<DumpFormat>,
    selector: Option<String>,
    wait_secs: u64,
    wait_is_fixed: bool,
    timeout_secs: u64,
    wait_until: &str,
    user_agent: Option<String>,
    stealth: bool,
    eval: Option<String>,
    output: Option<std::path::PathBuf>,
    quiet: bool,
    proxy: Option<String>,
    storage_dir: Option<std::path::PathBuf>,
    allow_private_network: bool,
    obey_robots: bool,
    screenshot: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    // Whether the user explicitly passed --dump. With --eval also present this
    // decides whether we return the eval value or read the page after the
    // eval's async work settles (issue #248).
    let dump_specified = dump.is_some();
    let dump = dump.unwrap_or(DumpFormat::Html);

    // --dump original short-circuits the browser stack entirely: fetch the raw
    // response body via HTTP and stream the bytes verbatim. Useful for binary
    // payloads (images, fonts, …) and any non-HTML resource where parsing the
    // body through the DOM/JS layer would corrupt or discard data.
    if dump == DumpFormat::Original {
        let bytes = fetch_original_bytes(
            url_str,
            proxy,
            user_agent.clone(),
            timeout_secs,
            stealth,
        )
        .await?;
        write_or_print_bytes(&bytes, output.as_ref()).await?;
        return Ok(());
    }

    let mut context = BrowserContext::with_storage_and_network(
        "fetch".to_string(),
        proxy,
        stealth,
        user_agent.clone(),
        storage_dir.clone(),
        allow_private_network,
    );
    context.obey_robots = obey_robots;
    let context = Arc::new(context);
    let mut page = Page::new("fetch-page".to_string(), context.clone());
    // Keep the browser's end-to-end navigation ceiling aligned with the CLI
    // request deadline. Previously Page retained its independent 30s default,
    // so `fetch --timeout 50` could still fail after 30 seconds.
    configure_fetch_navigation_timeout(&mut page, timeout_secs);
    // A screenshot viewport is also the navigation viewport: responsive
    // frameworks must build the DOM for the same dimensions we later paint.
    // Previously page JS saw a randomized screen-sized innerWidth while the
    // screenshot used these values only at the final raster step.
    let screenshot_viewport = screenshot.as_ref().map(|_| {
        let width = std::env::var("OBSCURA_SHOT_W")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(1280.0);
        let height = std::env::var("OBSCURA_SHOT_H")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(720.0);
        (width, height)
    });
    if let Some(viewport) = screenshot_viewport {
        page.set_viewport(viewport);
    }

    if let Some(ref ua) = user_agent {
        page.http_client.set_user_agent(ua).await;
    }

    let wait_condition = obscura_browser::lifecycle::WaitUntil::from_str(wait_until);

    if !quiet {
        eprintln!("Fetching {}...", url_str);
    }

    // The paired corpus opts into a truthful capture boundary: its read-only
    // evaluation runs after all settle passes and the final scroll reassert,
    // immediately before screenshot paint. Ordinary CLI evaluation retains
    // its existing evaluate-then-settle behavior when this private variable is
    // absent.
    let eval_at_capture_boundary = screenshot.is_some()
        && eval.is_some()
        && std::env::var("OBSCURA_SHOT_EVAL_AT_CAPTURE").is_ok_and(|value| value == "1");
    let controlled_scroll_request = screenshot.as_ref().and_then(|_| {
        let raw_y = std::env::var("OBSCURA_SHOT_SCROLL_Y").ok()?;
        let x = match std::env::var("OBSCURA_SHOT_SCROLL_X") {
            Ok(raw) => raw.parse::<f64>().ok().filter(|value| value.is_finite())?,
            Err(_) => 0.0,
        };
        let requested_y = if raw_y.eq_ignore_ascii_case("bottom") {
            "document.documentElement.scrollHeight".to_string()
        } else {
            raw_y
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|value| value.to_string())?
        };
        Some((x, requested_y))
    });

    // Process-level hard deadline. A synchronous hang inside a Rust op invoked
    // from page JS cannot be cancelled by tokio (there is no await to interrupt)
    // nor by the V8 watchdog (terminate_execution only unwinds JS bytecode, not
    // native Rust running beneath a V8->op call). As an absolute backstop so one
    // fetch can never wedge the worker, a daemon thread force-exits if the whole
    // operation overruns navigation + every configured settle pass + grace. A
    // normal fetch returns first and the process exits before this fires.
    {
        let settle_passes = if eval_at_capture_boundary {
            1 + u64::from(controlled_scroll_request.is_some())
        } else if eval.is_some() && (screenshot.is_some() || selector.is_some() || dump_specified) {
            2
        } else {
            1
        };
        let hard = Duration::from_secs(
            timeout_secs
                .saturating_add(wait_secs.saturating_mul(settle_passes))
                .saturating_add(10),
        );
        std::thread::spawn(move || {
            std::thread::sleep(hard);
            eprintln!(
                "obscura: hard timeout exceeded ({}s); forcing exit",
                hard.as_secs()
            );
            std::process::exit(124);
        });
    }

    match timeout(
        Duration::from_secs(timeout_secs),
        page.navigate_with_wait(url_str, wait_condition),
    )
    .await
    {
        Ok(result) => {
            result.map_err(|e| anyhow::anyhow!("Failed to navigate to {}: {}", url_str, e))?
        }
        Err(_) => anyhow::bail!(
            "Timed out navigating to {} after {}s",
            url_str,
            timeout_secs
        ),
    }

    if !quiet {
        eprintln!("Page loaded: {} - \"{}\"", page.url_string(), page.title);
    }

    // --wait is a post-load settle: drive the event loop so timers, async work,
    // and completion callbacks (e.g. testharness's add_completion_callback) run
    // before we read the page. Returns early once the loop is idle, so static
    // pages stay fast.
    settle_page(&mut page, wait_secs, wait_is_fixed).await;

    let mut deferred_eval_output = None;
    let initial_controlled_scroll = if eval_at_capture_boundary {
        controlled_scroll_request.as_ref().map(|(x, requested_y)| {
            page.evaluate(&format!(
                "(()=>{{\
                 const requestedX={x},requestedY={requested_y};\
                 const preInitial={{x:window.scrollX,y:window.scrollY}};\
                 window.scrollTo(requestedX,requestedY);\
                 return {{requested:{{x:requestedX,y:requestedY}},\
                 preInitialActual:preInitial,\
                 postInitialActual:{{x:window.scrollX,y:window.scrollY}},\
                 initialBehavior:'authored',\
                 initialPhase:'before-controlled-scroll-settle'}}\
                 }})()"
            ))
        })
    } else {
        None
    };
    if initial_controlled_scroll.is_some() {
        settle_page(&mut page, wait_secs, wait_is_fixed).await;
    }

    if !eval_at_capture_boundary {
        if let Some(ref expr) = eval {
            // Bound the eval by the same budget as navigation so a runaway
            // expression (infinite loop, never-settling sync work) cannot hang.
            let result = page.evaluate_with_timeout(expr, Duration::from_secs(timeout_secs));

            // A bare --eval (no --selector, --dump, or --screenshot) returns the
            // eval value directly, so synchronous expressions
            // (JSON.stringify, ...) are unchanged. Screenshot captures continue
            // below so an evaluation such as scrollTo() affects the painted
            // viewport instead of being silently ignored.
            if !dump_specified && selector.is_none() && screenshot.is_none() {
                let rendered = match result {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Null => "null".to_string(),
                    other => other.to_string(),
                };
                write_or_print(rendered, output.as_ref()).await?;
                context.save_cookies();
                return Ok(());
            }
            if screenshot.is_some() {
                deferred_eval_output = Some(result);
            }

            // --eval combined with --selector, --dump, and/or --screenshot
            // typically kicks off async work (a fetch promise, a timer, a scroll
            // listener) that writes the DOM. Drive the event loop again so that
            // work completes, then fall through to selector/capture/dump instead
            // of returning the still-pending eval value (issue #248).
            settle_page(&mut page, wait_secs, wait_is_fixed).await;
        }
    }

    if let Some(ref sel) = selector {
        let found = wait_for_selector(&mut page, sel, wait_secs).await;
        if !found {
            eprintln!("Warning: selector '{}' not found after {}s", sel, wait_secs);
        }
    }

    // --screenshot renders the settled, optionally evaluated page to a PNG.
    // Requires the render feature; without it, page.screenshot is absent and
    // we report clearly.
    if let Some(ref path) = screenshot {
        #[cfg(feature = "render")]
        {
            let resource_deadline_ms = std::env::var("OBSCURA_RENDER_RESOURCE_DEADLINE_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(3_000);
            let _ = page
                .prepare_screenshot_resources(resource_deadline_ms)
                .await;
            // Default CSS-pixel viewport, matching the engine's innerWidth/Height.
            // OBSCURA_SHOT_W / OBSCURA_SHOT_H override it (e.g. a tall viewport to
            // capture below-the-fold content in one shot).
            let viewport = screenshot_viewport.unwrap_or((1280.0, 720.0));
            // Ordinary screenshots sample the live document timeline. The
            // comparison harness can request an exact instant (normally T=0)
            // so both engines paint the same animation frame.
            let requested_animation_sample = std::env::var("OBSCURA_SHOT_ANIMATION_TIME_MS")
                .ok()
                .map(|raw| {
                    let milliseconds = raw.parse::<f32>().map_err(|_| {
                        anyhow::anyhow!(
                            "OBSCURA_SHOT_ANIMATION_TIME_MS must be a finite non-negative number"
                        )
                    })?;
                    if !milliseconds.is_finite() || milliseconds < 0.0 {
                        anyhow::bail!(
                            "OBSCURA_SHOT_ANIMATION_TIME_MS must be a finite non-negative number"
                        );
                    }
                    Ok(obscura_browser::AnimationSampleTime { milliseconds })
                })
                .transpose()?;
            let capture_screenshot = |page: &Page| match requested_animation_sample {
                Some(sample) => page.screenshot_at_animation_time(viewport, sample),
                None => page.screenshot(viewport),
            };
            // The parity harness performs one throwaway paint in both engines
            // before observing image/font readiness. Obscura resolves retained
            // render resources during prepare/paint, so sampling first would
            // compare pre-paint Obscura state with post-load Chromium state.
            // Keep this private opt-in out of ordinary CLI screenshots.
            let warmup_capture =
                std::env::var("OBSCURA_SHOT_RESOURCE_WARMUP").is_ok_and(|value| value == "1");
            if warmup_capture {
                if capture_screenshot(&page).is_none() {
                    anyhow::bail!("resource warm-up screenshot failed: page has no DOM to render");
                }
                // Give completion callbacks one bounded task turn before the
                // capture-boundary evaluation reads resource state.
                page.settle(1).await;
            }
            // Paired renderer captures need a stable final coordinate after
            // the post-eval settle. Authored smooth scrolling and scroll
            // anchoring may legitimately move an earlier scrollTo while the
            // page changes above the viewport, so the comparison harness opts
            // into one instant reassertion at the actual capture boundary.
            // Ordinary CLI screenshots are unchanged when these private
            // capture-environment variables are absent.
            let controlled_scroll = controlled_scroll_request
                .as_ref()
                .map(|(x, requested_y)| {
                    page.evaluate(&format!(
                        "(()=>{{\
                         const requestedX={x},requestedY={requested_y};\
                         const preReassert={{x:window.scrollX,y:window.scrollY}};\
                         const root=document.documentElement;\
                         const previous=root?root.style.getPropertyValue('scroll-behavior'):'';\
                         const priority=root?root.style.getPropertyPriority('scroll-behavior'):'';\
                         if(root)root.style.setProperty('scroll-behavior','auto','important');\
                         window.scrollTo(requestedX,requestedY);\
                         if(root){{if(previous)root.style.setProperty('scroll-behavior',previous,priority);\
                         else root.style.removeProperty('scroll-behavior')}}\
                         return {{requested:{{x:requestedX,y:requestedY}},\
                         preReassertActual:preReassert,\
                         finalReassertActual:{{x:window.scrollX,y:window.scrollY}},\
                         behavior:'instant',\
                         phase:'immediately-before-capture-state-and-screenshot'}}\
                         }})()"
                    ))
                });
            if eval_at_capture_boundary {
                if let Some(ref expr) = eval {
                    deferred_eval_output =
                        Some(page.evaluate_with_timeout(expr, Duration::from_secs(timeout_secs)));
                }
            }
            let capture_state = deferred_eval_output.as_ref().map(|_| {
                page.evaluate(
                    "(()=>({\
                     scrollX:window.scrollX,scrollY:window.scrollY,\
                     innerWidth:window.innerWidth,innerHeight:window.innerHeight,\
                     scrollWidth:document.documentElement?document.documentElement.scrollWidth:0,\
                     scrollHeight:document.documentElement?document.documentElement.scrollHeight:0\
                     }))()",
                )
            });
            match capture_screenshot(&page) {
                Some(bytes) => std::fs::write(path, &bytes)?,
                None => anyhow::bail!("screenshot failed: page has no DOM to render"),
            }
            // A screenshot+eval command used to ignore the expression
            // completely. Emit both its value and a standard state sampled
            // after the post-eval settle so automation can record the exact
            // live viewport that was painted.
            if let Some(result) = deferred_eval_output {
                let mut controlled_scroll_report = controlled_scroll;
                if let (Some(report), Some(initial)) = (
                    controlled_scroll_report.as_mut(),
                    initial_controlled_scroll.as_ref(),
                ) {
                    if let (Some(report), Some(initial)) =
                        (report.as_object_mut(), initial.as_object())
                    {
                        for key in [
                            "preInitialActual",
                            "postInitialActual",
                            "initialBehavior",
                            "initialPhase",
                        ] {
                            if let Some(value) = initial.get(key) {
                                report.insert(key.to_string(), value.clone());
                            }
                        }
                    }
                }
                println!(
                    "{}",
                    serde_json::json!({
                        "evaluation": result,
                        "controlledScroll": controlled_scroll_report,
                        "resourceWarmup": {
                            "performed": warmup_capture,
                            "discardedShots": if warmup_capture { 1 } else { 0 },
                            "taskTurnMs": if warmup_capture { 1 } else { 0 },
                            "phase": "before-final-scroll-reassert-and-state-sample",
                        },
                        "captureState": capture_state.unwrap_or(serde_json::Value::Null),
                    })
                );
            }
            if !quiet {
                eprintln!(
                    "Screenshot written: {} ({} bytes)",
                    path.display(),
                    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
                );
            }
            context.save_cookies();
            return Ok(());
        }
        #[cfg(not(feature = "render"))]
        {
            anyhow::bail!(
                "--screenshot {} requires a build with the render feature (cargo build --features render)",
                path.display()
            );
        }
    }

    let rendered = match dump {
        DumpFormat::Html => dump_html(&page),
        DumpFormat::Text => dump_text(&mut page),
        DumpFormat::Links => dump_links(&page),
        DumpFormat::Markdown => dump_markdown(&mut page),
        DumpFormat::Assets => dump_assets(&page),
        DumpFormat::Cookies => dump_cookies(&page),
        // Handled above via the short-circuit branch; unreachable here.
        DumpFormat::Original => unreachable!("Original dump handled before page navigation"),
    };
    write_or_print(rendered, output.as_ref()).await?;

    // Save cookies to disk if storage_dir is configured
    context.save_cookies();

    Ok(())
}

async fn fetch_original_response(
    url_str: &str,
    proxy: Option<String>,
    user_agent: Option<String>,
    timeout_secs: u64,
    stealth: bool,
) -> anyhow::Result<obscura_net::Response> {
    let url = url::Url::parse(url_str)
        .map_err(|e| anyhow::anyhow!("Invalid URL '{}': {}", url_str, e))?;

    // `--dump original` short-circuits the browser stack (see run_fetch) and
    // builds its own client here instead of going through BrowserContext /
    // Page::do_fetch, which is where --stealth is normally applied. Without
    // this, the request stays on plain HTTP/1.1 with no TLS impersonation
    // regardless of --stealth (issue #482). file:// has no TLS handshake to
    // impersonate and the wreq client only speaks http(s), so it is excluded
    // here the same way ObscuraHttpClient::fetch_with_method excludes it
    // internally.
    if stealth && url.scheme() != "file" {
        #[cfg(feature = "stealth")]
        {
            let client = obscura_net::StealthHttpClient::with_proxy(
                Arc::new(obscura_net::CookieJar::new()),
                proxy.as_deref(),
            );
            return match timeout(Duration::from_secs(timeout_secs), client.fetch(&url)).await {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(e)) => anyhow::bail!("Failed to fetch {}: {}", url_str, e),
                Err(_) => anyhow::bail!("Timed out fetching {} after {}s", url_str, timeout_secs),
            };
        }
    }

    let client = obscura_net::ObscuraHttpClient::with_options(
        Arc::new(obscura_net::CookieJar::new()),
        proxy.as_deref(),
    );
    if let Some(ua) = user_agent {
        client.set_user_agent(&ua).await;
    }

    match timeout(Duration::from_secs(timeout_secs), client.fetch(&url)).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => anyhow::bail!("Failed to fetch {}: {}", url_str, e),
        Err(_) => anyhow::bail!("Timed out fetching {} after {}s", url_str, timeout_secs),
    }
}

async fn fetch_original_bytes(
    url_str: &str,
    proxy: Option<String>,
    user_agent: Option<String>,
    timeout_secs: u64,
    stealth: bool,
) -> anyhow::Result<Vec<u8>> {
    Ok(
        fetch_original_response(url_str, proxy, user_agent, timeout_secs, stealth)
            .await?
            .body,
    )
}

/// Read newline-delimited URLs from `path` (or stdin when `path` is `-`).
/// Blank lines and `#` comments are dropped, and surrounding whitespace is
/// trimmed so a list copy-pasted with indentation still works.
fn read_urls_from_file(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let content = if path == std::path::Path::new("-") {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| anyhow::anyhow!("Failed to read URLs from stdin: {}", e))?;
        s
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?
    };

    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}

/// Batch raw fetch: run `--dump original` over many URLs concurrently and print
/// one JSON status line per URL (issue #349). This is the raw-resource-check
/// counterpart to `scrape`; it never renders, so there is no browser/JS cost
/// per URL. Output stays in input order regardless of completion order.
async fn run_batch_fetch(
    urls: Vec<String>,
    concurrency: usize,
    timeout_secs: u64,
    user_agent: Option<String>,
    proxy: Option<String>,
    output: Option<std::path::PathBuf>,
    quiet: bool,
    stealth: bool,
) -> anyhow::Result<()> {
    let total = urls.len();
    if total == 0 {
        anyhow::bail!("No URLs to fetch (--file was empty).");
    }

    if !quiet {
        eprintln!(
            "Fetching {} URLs with {} concurrent request(s) (per-fetch timeout: {}s)...",
            total, concurrency, timeout_secs
        );
    }

    let start = Instant::now();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let user_agent = Arc::new(user_agent);
    let proxy = Arc::new(proxy);

    let mut handles = Vec::with_capacity(total);
    for (i, url) in urls.into_iter().enumerate() {
        let sem = semaphore.clone();
        let user_agent = user_agent.clone();
        let proxy = proxy.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let task_start = Instant::now();
            let result = fetch_original_response(
                &url,
                (*proxy).clone(),
                (*user_agent).clone(),
                timeout_secs,
                stealth,
            )
            .await;
            let elapsed_ms = task_start.elapsed().as_millis();

            let line = match result {
                Ok(resp) => serde_json::json!({
                    "url": url,
                    "ok": (200..400).contains(&resp.status),
                    "status": resp.status,
                    "content_type": resp.headers.get("content-type").cloned().unwrap_or_default(),
                    "bytes": resp.body.len(),
                    "elapsed_ms": elapsed_ms,
                }),
                Err(e) => serde_json::json!({
                    "url": url,
                    "ok": false,
                    "error": e.to_string(),
                    "elapsed_ms": elapsed_ms,
                }),
            };
            (i, line)
        }));
    }

    let mut results: Vec<Option<serde_json::Value>> = vec![None; total];
    let mut failures = 0usize;
    for handle in handles {
        if let Ok((i, line)) = handle.await {
            if !line["ok"].as_bool().unwrap_or(false) {
                failures += 1;
            }
            results[i] = Some(line);
        } else {
            failures += 1;
        }
    }

    let mut out = String::new();
    for line in results.into_iter().flatten() {
        out.push_str(&serde_json::to_string(&line).unwrap_or_default());
        out.push('\n');
    }

    if let Some(path) = output {
        tokio::fs::write(&path, out.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    } else {
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(out.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write to stdout: {}", e))?;
        stdout
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to flush stdout: {}", e))?;
    }

    if !quiet {
        eprintln!(
            "Done: {} URLs in {:.1}s ({} ok, {} failed).",
            total,
            start.elapsed().as_secs_f64(),
            total - failures,
            failures
        );
    }

    Ok(())
}

async fn write_or_print(
    content: String,
    output: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    if let Some(path) = output {
        tokio::fs::write(path, content)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    } else {
        println!("{}", content);
    }
    Ok(())
}

async fn write_or_print_bytes(
    bytes: &[u8],
    output: Option<&std::path::PathBuf>,
) -> anyhow::Result<()> {
    if let Some(path) = output {
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path.display(), e))?;
    } else {
        // Write raw bytes to stdout — never println! (would append a newline
        // and break binary payloads like JPEG/PNG).
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(bytes)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to write to stdout: {}", e))?;
        stdout
            .flush()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to flush stdout: {}", e))?;
    }
    Ok(())
}

async fn wait_for_selector(page: &mut Page, selector: &str, timeout_secs: u64) -> bool {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    loop {
        let found = page
            .with_dom(|dom| dom.query_selector(selector).ok().flatten().is_some())
            .unwrap_or(false);

        if found {
            return true;
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }

        // The selector may be created by a timer, dynamic import, or fetch
        // completion. Sleeping without pumping V8 makes those callbacks unable
        // to run, so a valid selector wait always times out. Drive one bounded
        // event-loop slice, then retain a 100ms polling cadence if it returned
        // idle immediately.
        let slice_started = tokio::time::Instant::now();
        page.settle(100).await;
        let spent = slice_started.elapsed();
        let cadence = tokio::time::Duration::from_millis(100);
        if spent < cadence {
            tokio::time::sleep(cadence - spent).await;
        }
    }
}

fn dump_cookies(page: &Page) -> String {
    let cookies = page.context.cookie_jar.get_all_cookies();
    serde_json::to_string_pretty(&cookies).unwrap_or_else(|_| "[]".to_string())
}

fn dump_html(page: &Page) -> String {
    page.with_dom(|dom| {
        if let Ok(Some(html_node)) = dom.query_selector("html") {
            let html = dom.outer_html(html_node);
            format!("<!DOCTYPE html>\n{}", html)
        } else {
            let doc = dom.document();
            dom.inner_html(doc)
        }
    })
    .unwrap_or_default()
}

fn dump_text(page: &mut Page) -> String {
    page.with_dom(|dom| {
        if let Ok(Some(body)) = dom.query_selector("body") {
            let text = extract_readable_text(dom, body);
            text.trim().to_string()
        } else {
            String::new()
        }
    })
    .unwrap_or_default()
}

fn dump_markdown(page: &mut Page) -> String {
    let result = page.evaluate(obscura_browser::HTML_TO_MARKDOWN_JS);
    result.as_str().unwrap_or_default().to_string()
}

fn extract_readable_text(dom: &obscura_dom::DomTree, node_id: obscura_dom::NodeId) -> String {
    use obscura_dom::NodeData;

    // Iterative DFS over an explicit work stack. A recursive walk overflowed the
    // call stack (a hard abort, not a catchable panic) on deeply nested pages,
    // taking down the process on `--dump text` (issue #362, the CLI counterpart
    // of the serialize/textContent paths made iterative in obscura-dom). A
    // `Newline` work item emits a block element's trailing newline after its
    // children, matching the old pre/post-recursion output exactly.
    enum Work {
        Visit(obscura_dom::NodeId),
        Newline,
    }

    // Defense-in-depth cap mirroring DomTree::descendants; never reached on a
    // valid tree since append_child / insert_before reject cycles.
    const MAX_NODES: usize = 5_000_000;

    let mut result = String::new();
    let mut stack: Vec<Work> = vec![Work::Visit(node_id)];
    let mut visited = 0usize;

    while let Some(work) = stack.pop() {
        let id = match work {
            Work::Newline => {
                result.push('\n');
                continue;
            }
            Work::Visit(id) => id,
        };

        visited += 1;
        if visited > MAX_NODES {
            break;
        }

        let node = match dom.get_node(id) {
            Some(n) => n,
            None => continue,
        };

        match &node.data {
            NodeData::Text { contents } => {
                let trimmed = contents.trim();
                if !trimmed.is_empty() {
                    result.push_str(trimmed);
                }
            }
            NodeData::Element { name, .. } => {
                let tag = name.local.as_ref();

                // Boilerplate elements rarely contain content the user wants to
                // scrape — strip them so `--dump text` returns the article body
                // instead of menus, footers, and cookie banners.
                if matches!(
                    tag,
                    "script" | "style" | "nav" | "header" | "footer" | "aside"
                ) {
                    continue;
                }

                let is_block = matches!(
                    tag,
                    "div"
                        | "p"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "li"
                        | "tr"
                        | "br"
                        | "hr"
                        | "blockquote"
                        | "pre"
                        | "section"
                        | "article"
                        | "header"
                        | "footer"
                        | "nav"
                        | "main"
                        | "aside"
                        | "figure"
                        | "figcaption"
                        | "table"
                        | "thead"
                        | "tbody"
                        | "tfoot"
                        | "dl"
                        | "dt"
                        | "dd"
                        | "ul"
                        | "ol"
                );

                if is_block {
                    result.push('\n');
                    // Processed after all children (stack is LIFO): the trailing newline.
                    stack.push(Work::Newline);
                }
                // Push children in reverse so they pop in document order.
                for child_id in dom.children(id).into_iter().rev() {
                    stack.push(Work::Visit(child_id));
                }
            }
            _ => {
                for child_id in dom.children(id).into_iter().rev() {
                    stack.push(Work::Visit(child_id));
                }
            }
        }
    }

    result
}

async fn run_parallel_scrape(
    urls: Vec<String>,
    eval: Option<String>,
    concurrency: usize,
    format: &str,
    timeout_secs: u64,
    quiet: bool,
    proxy: Option<String>,
    stealth: bool,
    obey_robots: bool,
) -> anyhow::Result<()> {
    let total = urls.len();
    let start = Instant::now();

    if total == 0 {
        anyhow::bail!("No URLs provided. Pass at least one URL to scrape.");
    }

    if !quiet {
        eprintln!(
            "Scraping {} URLs with {} concurrent workers (per-worker timeout: {}s)...",
            total, concurrency, timeout_secs
        );
    }

    let worker_name = if cfg!(windows) {
        "obscura-worker.exe"
    } else {
        "obscura-worker"
    };
    let worker_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(worker_name)))
        .unwrap_or_else(|| std::path::PathBuf::from(worker_name));

    if !worker_path.exists() {
        anyhow::bail!(
            "Worker binary not found at {}. Build with: cargo build --release",
            worker_path.display()
        );
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let eval = Arc::new(eval);
    let worker_path = Arc::new(worker_path);
    let worker_timeout = Duration::from_secs(timeout_secs);
    let read_timeout = Duration::from_secs(timeout_secs.min(30));
    let shutdown_timeout = Duration::from_secs(5);

    let mut handles = Vec::new();

    for (i, url) in urls.into_iter().enumerate() {
        let sem = semaphore.clone();
        let eval = eval.clone();
        let worker_path = worker_path.clone();
        let proxy = proxy.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let task_start = Instant::now();

            let mut child = match TokioCommand::new(worker_path.as_ref())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .env("OBSCURA_PROXY", proxy.as_deref().unwrap_or(""))
                .env("OBSCURA_STEALTH", if stealth { "1" } else { "" })
                .env("OBSCURA_OBEY_ROBOTS", if obey_robots { "1" } else { "" })
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    return serde_json::json!({
                        "url": url,
                        "error": format!("Failed to spawn worker: {}", e),
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };

            let mut stdin = match child.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    return serde_json::json!({
                        "url": url,
                        "error": "Failed to open worker stdin",
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };
            let stdout = match child.stdout.take() {
                Some(stdout) => stdout,
                None => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    return serde_json::json!({
                        "url": url,
                        "error": "Failed to open worker stdout",
                        "time_ms": task_start.elapsed().as_millis(),
                    });
                }
            };
            let mut reader = BufReader::new(stdout);

            let worker_result: Result<serde_json::Value, String> =
                match timeout(worker_timeout, async {
                    let nav_cmd = serde_json::json!({"cmd": "navigate", "url": url});
                    let mut line = serde_json::to_string(&nav_cmd).unwrap();
                    line.push('\n');
                    if stdin.write_all(line.as_bytes()).await.is_err() {
                        return Err("Write failed".to_string());
                    }
                    if stdin.flush().await.is_err() {
                        return Err("Write failed".to_string());
                    }

                    let mut resp_line = String::new();
                    match timeout(read_timeout, reader.read_line(&mut resp_line)).await {
                        Ok(Ok(bytes)) if bytes > 0 => {}
                        Ok(Ok(_)) | Ok(Err(_)) => return Err("Read failed".to_string()),
                        Err(_) => return Err("timeout".to_string()),
                    };

                    let nav_resp: serde_json::Value = serde_json::from_str(resp_line.trim())
                        .unwrap_or(serde_json::json!({"ok": false}));

                    if !nav_resp["ok"].as_bool().unwrap_or(false) {
                        return Err(nav_resp["error"]
                            .as_str()
                            .unwrap_or("navigate failed")
                            .to_string());
                    }

                    let title = nav_resp["result"]["title"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();

                    let eval_result = if let Some(ref expr) = *eval {
                        let eval_cmd = serde_json::json!({"cmd": "evaluate", "expression": expr});
                        let mut line = serde_json::to_string(&eval_cmd).unwrap();
                        line.push('\n');
                        if stdin.write_all(line.as_bytes()).await.is_err() {
                            return Err("Write failed".to_string());
                        }
                        if stdin.flush().await.is_err() {
                            return Err("Write failed".to_string());
                        }

                        let mut resp_line = String::new();
                        match timeout(read_timeout, reader.read_line(&mut resp_line)).await {
                            Ok(Ok(bytes)) if bytes > 0 => {
                                let resp: serde_json::Value =
                                    serde_json::from_str(resp_line.trim())
                                        .unwrap_or(serde_json::json!({"ok": false}));
                                resp["result"].clone()
                            }
                            Ok(Ok(_)) | Ok(Err(_)) => return Err("Read failed".to_string()),
                            Err(_) => return Err("timeout".to_string()),
                        }
                    } else {
                        serde_json::Value::Null
                    };

                    let shutdown_cmd = serde_json::json!({"cmd": "shutdown"});
                    let mut line = serde_json::to_string(&shutdown_cmd).unwrap();
                    line.push('\n');
                    let _ = stdin.write_all(line.as_bytes()).await;
                    let _ = stdin.flush().await;
                    let _ = timeout(shutdown_timeout, child.wait()).await;

                    Ok(serde_json::json!({
                        "url": url,
                        "title": title,
                        "eval": eval_result,
                        "time_ms": task_start.elapsed().as_millis(),
                        "worker": i,
                    }))
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("timeout".to_string()),
                };

            match worker_result {
                Ok(result) => result,
                Err(error) => {
                    let _ = timeout(shutdown_timeout, child.kill()).await;
                    serde_json::json!({
                        "url": url,
                        "error": error,
                        "time_ms": task_start.elapsed().as_millis(),
                    })
                }
            }
        });

        handles.push(handle);
    }

    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(serde_json::json!({"error": e.to_string()})),
        }
    }

    let total_time = start.elapsed();

    if format == "json" {
        let output = serde_json::json!({
            "total_urls": total,
            "concurrency": concurrency,
            "total_time_ms": total_time.as_millis(),
            "avg_time_ms": total_time.as_millis() as f64 / total as f64,
            "results": results,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        for r in &results {
            let url = r["url"].as_str().unwrap_or("?");
            let title = r["title"].as_str().unwrap_or("");
            let time = r["time_ms"].as_u64().unwrap_or(0);
            let eval = &r["eval"];
            if eval.is_null() {
                println!("{}ms\t{}\t{}", time, url, title);
            } else {
                println!("{}ms\t{}\t{}", time, url, eval);
            }
        }
        if !quiet {
            eprintln!(
                "\nTotal: {}ms for {} URLs ({} concurrent)",
                total_time.as_millis(),
                total,
                concurrency
            );
        }
    }

    Ok(())
}

fn dump_links(page: &Page) -> String {
    let base_url = page.url.clone();
    page.with_dom(|dom| {
        let mut rendered = Vec::new();
        let links = dom.query_selector_all("a").unwrap_or_default();
        for link_id in links {
            if let Some(node) = dom.get_node(link_id) {
                let href = node.get_attribute("href").unwrap_or_default().to_string();
                let text = dom.text_content(link_id);
                let text = text.trim();

                let full_url = if href.starts_with("http://") || href.starts_with("https://") {
                    href.clone()
                } else if let Some(ref base) = base_url {
                    base.join(&href)
                        .map(|u| u.to_string())
                        .unwrap_or(href.clone())
                } else {
                    href.clone()
                };

                if !full_url.is_empty() {
                    if text.is_empty() {
                        rendered.push(full_url);
                    } else {
                        rendered.push(format!("{}\t{}", full_url, text));
                    }
                }
            }
        }
        rendered.join("\n")
    })
    .unwrap_or_default()
}

/// Selectors paired with the attribute whose URL we extract and the
/// asset kind we surface. Order is stable so the output of
/// `--dump assets` is deterministic across runs.
const ASSET_SELECTORS: &[(&str, &str, &str)] = &[
    ("script[src]", "src", "script"),
    ("link[href]", "href", "link"),
    ("img[src]", "src", "image"),
    ("iframe[src]", "src", "iframe"),
    ("source[src]", "src", "media"),
    ("video[src]", "src", "video"),
    ("audio[src]", "src", "audio"),
    ("embed[src]", "src", "embed"),
    ("object[data]", "data", "object"),
];

/// Map a `<link>` element's `rel` token to a more specific asset
/// kind so consumers can filter (e.g. just stylesheets, just icons).
/// Unknown / missing `rel` falls back to a generic "link" so the
/// caller still sees the URL.
fn link_kind_from_rel(rel: &str) -> &'static str {
    match rel
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "stylesheet" => "stylesheet",
        "icon" | "shortcut" => "icon",
        "manifest" => "manifest",
        "preload" => "preload",
        "prefetch" => "prefetch",
        "modulepreload" => "modulepreload",
        "dns-prefetch" => "dns-prefetch",
        "preconnect" => "preconnect",
        "alternate" => "alternate",
        _ => "link",
    }
}

/// Resolve a raw `src`/`href`/`data` attribute against the page's
/// base URL. Mirrors `dump_links`'s logic so `--dump assets` and
/// `--dump links` agree on absolute-URL semantics.
fn resolve_asset_url(raw: &str, base_url: Option<&url::Url>) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(trimmed.to_string());
    }
    if let Some(base) = base_url {
        if let Ok(joined) = base.join(trimmed) {
            return Some(joined.to_string());
        }
    }
    Some(trimmed.to_string())
}

/// Walk the rendered DOM and emit one NDJSON line per discoverable
/// sub-resource. Pure over `DomTree`/`Url` so unit tests can drive
/// it from a fixture HTML without standing up a browser.
fn extract_assets(dom: &obscura_dom::DomTree, base_url: Option<&url::Url>) -> String {
    let mut out: Vec<String> = Vec::new();
    for (selector, attr, default_kind) in ASSET_SELECTORS {
        let nodes = dom.query_selector_all(selector).unwrap_or_default();
        for node_id in nodes {
            let Some(node) = dom.get_node(node_id) else {
                continue;
            };
            let raw = node.get_attribute(attr).unwrap_or_default().to_string();
            let Some(url) = resolve_asset_url(&raw, base_url) else {
                continue;
            };

            let kind = if *default_kind == "link" {
                let rel = node.get_attribute("rel").unwrap_or_default().to_string();
                link_kind_from_rel(&rel)
            } else {
                *default_kind
            };

            let record = serde_json::json!({
                "url": url,
                "type": kind,
            });
            out.push(record.to_string());
        }
    }
    out.join("\n")
}

fn dump_assets(page: &Page) -> String {
    let base_url = page.url.clone();
    let dom_ndjson = page
        .with_dom(|dom| extract_assets(dom, base_url.as_ref()))
        .unwrap_or_default();

    let mut lines: Vec<String> = dom_ndjson
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    // URLs already listed from static DOM attributes, so a resource the script
    // fetches that the markup also references is not emitted twice.
    let mut seen: std::collections::HashSet<String> = lines
        .iter()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
        .collect();

    // Resources pulled in by JS fetch()/XHR, which leave no static DOM tag
    // (issue #301).
    for url in page.fetched_urls() {
        if seen.insert(url.clone()) {
            lines.push(serde_json::json!({ "url": url, "type": "fetch" }).to_string());
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        configure_fetch_navigation_timeout, effective_v8_flags, extract_assets,
        extract_readable_text, fetch_original_bytes, is_quiet_command, link_kind_from_rel,
        merge_proxy, normalize_v8_flags, read_urls_from_file, resolve_asset_url, select_log_filter,
        write_or_print, write_or_print_bytes, Args, Command, DumpFormat, DEFAULT_V8_FLAGS,
    };
    use clap::Parser;
    use obscura_dom::parse_html;

    // Issue #117 — `--dump original` short-circuits the browser stack and
    // streams the raw response body verbatim, including for binary payloads.
    //
    // Two tests below pin the behaviour:
    //   1. clap accepts `--dump original` as a valid DumpFormat variant.
    //   2. `fetch_original_bytes` returns the exact bytes a `file://` URL
    //      points at (binary-safe round-trip — no UTF-8 decode, no trailing
    //      newline, no DOM mutation).
    //   3. `write_or_print_bytes` writes raw bytes to a file without the
    //      trailing newline that `println!` would add.
    #[test]
    fn parsed_fetch_dump_original_is_accepted_by_clap() {
        let args = Args::try_parse_from([
            "obscura",
            "fetch",
            "--dump",
            "original",
            "https://example.com/image.jpg",
        ])
        .expect("clap should accept --dump original");
        match args.command {
            Some(Command::Fetch { dump, .. }) => {
                assert_eq!(dump, Some(DumpFormat::Original));
            }
            _ => panic!("expected Fetch command"),
        }
    }

    // Issue #349 — batch mode: `fetch --file urls.txt --dump original
    // --concurrency N` with no positional URL.
    #[test]
    fn parsed_fetch_file_and_concurrency() {
        let args = Args::try_parse_from([
            "obscura",
            "fetch",
            "--file",
            "urls.txt",
            "--dump",
            "original",
            "--concurrency",
            "25",
        ])
        .expect("clap should accept --file with --concurrency and no positional URL");
        match args.command {
            Some(Command::Fetch {
                url,
                file,
                concurrency,
                dump,
                ..
            }) => {
                assert!(url.is_none());
                assert_eq!(file, Some(std::path::PathBuf::from("urls.txt")));
                assert_eq!(concurrency.get(), 25);
                assert_eq!(dump, Some(DumpFormat::Original));
            }
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn concurrency_rejects_zero() {
        // NonZeroUsize means --concurrency 0 is a parse error, not a silent hang
        // on a zero-permit semaphore.
        let err =
            Args::try_parse_from(["obscura", "fetch", "--file", "u.txt", "--concurrency", "0"]);
        assert!(err.is_err());
    }

    #[test]
    fn read_urls_skips_blanks_and_comments() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("obscura_urls_{}.txt", std::process::id()));
        std::fs::write(
            &path,
            "https://a.example/one.js\n\n  # a comment\n   https://b.example/two.css  \nhttps://c.example/three.json\n",
        )
        .unwrap();
        let urls = read_urls_from_file(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            urls,
            vec![
                "https://a.example/one.js".to_string(),
                "https://b.example/two.css".to_string(),
                "https://c.example/three.json".to_string(),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_original_bytes_returns_file_contents_verbatim() {
        // A real binary payload: a 1×1 transparent PNG (89 50 4E 47 …) —
        // exactly the kind of resource #117 wants to stream without HTML/
        // JS rendering.
        const PNG_BYTES: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let path = std::env::temp_dir().join(format!(
            "obscura-fetch-original-test-{}.png",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        tokio::fs::write(&path, PNG_BYTES)
            .await
            .expect("seed temp PNG fixture");

        let file_url = format!("file://{}", path.display());
        let bytes = fetch_original_bytes(&file_url, None, None, 5, false)
            .await
            .expect("fetch_original_bytes should round-trip the file body");

        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(
            bytes, PNG_BYTES,
            "raw response body must match the file byte-for-byte"
        );
    }

    // A stealth-enabled build routes `--dump original` through
    // StealthHttpClient (wreq), which only speaks http(s). file:// must keep
    // working the same as without --stealth instead of being handed to wreq
    // (issue #482).
    #[tokio::test(flavor = "current_thread")]
    async fn fetch_original_bytes_file_url_ignores_stealth_flag() {
        const PNG_BYTES: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let path = std::env::temp_dir().join(format!(
            "obscura-fetch-original-stealth-test-{}.png",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;
        tokio::fs::write(&path, PNG_BYTES)
            .await
            .expect("seed temp PNG fixture");

        let file_url = format!("file://{}", path.display());
        let bytes = fetch_original_bytes(&file_url, None, None, 5, true)
            .await
            .expect("fetch_original_bytes should still round-trip file:// with stealth=true");

        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(bytes, PNG_BYTES, "stealth=true must not change file:// handling");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_or_print_bytes_writes_without_trailing_newline() {
        // Regression guard for #117: stdout must receive raw bytes. The file
        // path used here exercises the file-output branch — println!-style
        // output (used by write_or_print) would append a 0x0A byte and
        // corrupt binary payloads. write_or_print_bytes must not.
        let payload: &[u8] = &[0x00, 0xFF, b'h', b'i', 0x00];
        let path = std::env::temp_dir().join(format!(
            "obscura-write-bytes-test-{}.bin",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;

        write_or_print_bytes(payload, Some(&path))
            .await
            .expect("write_or_print_bytes should write the file");

        let read_back = tokio::fs::read(&path).await.expect("read back");
        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(
            read_back, payload,
            "file bytes must match the payload exactly"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn write_or_print_writes_output_file_with_tokio_fs() {
        let path = std::env::temp_dir().join(format!(
            "obscura-fetch-output-test-{}.txt",
            std::process::id()
        ));
        let _ = tokio::fs::remove_file(&path).await;

        write_or_print("rendered output".to_string(), Some(&path))
            .await
            .expect("write output file");

        let content = tokio::fs::read_to_string(&path)
            .await
            .expect("read output file");
        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(content, "rendered output");
    }

    #[test]
    fn default_filter_is_warn() {
        assert_eq!(select_log_filter(false, false), "warn");
    }

    #[test]
    fn verbose_filter_is_debug() {
        assert_eq!(select_log_filter(true, false), "debug");
    }

    #[test]
    fn quiet_filter_is_off() {
        assert_eq!(select_log_filter(false, true), "off");
    }

    #[test]
    fn verbose_wins_over_quiet() {
        assert_eq!(select_log_filter(true, true), "debug");
    }

    #[test]
    fn parsed_fetch_with_quiet_flag_is_detected() {
        let args = Args::try_parse_from(["obscura", "fetch", "--quiet", "https://example.com"])
            .expect("clap should accept --quiet on fetch");
        assert!(is_quiet_command(&args.command));
    }

    #[test]
    fn parsed_fetch_without_quiet_is_not_detected() {
        let args = Args::try_parse_from(["obscura", "fetch", "https://example.com"])
            .expect("clap should accept fetch without --quiet");
        assert!(!is_quiet_command(&args.command));
    }

    #[test]
    fn parsed_serve_command_is_not_quiet() {
        let args = Args::try_parse_from(["obscura", "serve"]).expect("clap should accept serve");
        assert!(!is_quiet_command(&args.command));
    }

    #[test]
    fn no_subcommand_is_not_quiet() {
        assert!(!is_quiet_command(&None));
    }

    #[test]
    fn parsed_v8_flags_global_arg() {
        let args = Args::try_parse_from([
            "obscura",
            "--v8-flags",
            "--max-old-space-size=4096 --max-semi-space-size=64",
            "fetch",
            "https://example.com",
        ])
        .expect("clap should accept --v8-flags as a global arg");
        assert_eq!(
            args.v8_flags.as_deref(),
            Some("--max-old-space-size=4096 --max-semi-space-size=64"),
        );
    }

    #[test]
    fn v8_flags_default_is_none() {
        let args = Args::try_parse_from(["obscura", "fetch", "https://example.com"])
            .expect("clap should accept fetch without --v8-flags");
        assert!(args.v8_flags.is_none());
    }

    #[test]
    fn parsed_v8_flags_with_serve_subcommand() {
        let args = Args::try_parse_from([
            "obscura",
            "--v8-flags",
            "--max-old-space-size=2048",
            "serve",
            "--port",
            "9333",
        ])
        .expect("clap should accept --v8-flags with serve");
        assert_eq!(args.v8_flags.as_deref(), Some("--max-old-space-size=2048"));
    }

    #[test]
    fn parsed_v8_flags_with_scrape_subcommand() {
        let args = Args::try_parse_from([
            "obscura",
            "--v8-flags",
            "--expose-gc",
            "scrape",
            "https://a.com",
            "https://b.com",
        ])
        .expect("clap should accept --v8-flags with scrape");
        assert_eq!(args.v8_flags.as_deref(), Some("--expose-gc"));
    }

    #[test]
    fn parsed_v8_flags_empty_string_is_accepted() {
        let args =
            Args::try_parse_from(["obscura", "--v8-flags", "", "fetch", "https://example.com"])
                .expect("clap should accept empty --v8-flags value");
        assert_eq!(args.v8_flags.as_deref(), Some(""));
    }

    #[test]
    fn normalize_v8_flags_returns_none_when_unset() {
        assert_eq!(normalize_v8_flags(None), None);
    }

    #[test]
    fn normalize_v8_flags_returns_none_for_empty_or_whitespace() {
        assert_eq!(normalize_v8_flags(Some("")), None);
        assert_eq!(normalize_v8_flags(Some("   ")), None);
        assert_eq!(normalize_v8_flags(Some("\t\n")), None);
    }

    #[test]
    fn normalize_v8_flags_trims_surrounding_whitespace() {
        assert_eq!(
            normalize_v8_flags(Some("  --max-old-space-size=4096  ")).as_deref(),
            Some("--max-old-space-size=4096"),
        );
    }

    #[test]
    fn normalize_v8_flags_preserves_multi_flag_string() {
        let input = "--max-old-space-size=4096 --max-semi-space-size=64 --expose-gc";
        assert_eq!(normalize_v8_flags(Some(input)).as_deref(), Some(input));
    }

    #[test]
    fn effective_v8_flags_returns_default_when_unset() {
        assert_eq!(effective_v8_flags(None), DEFAULT_V8_FLAGS);
        assert_eq!(effective_v8_flags(Some("")), DEFAULT_V8_FLAGS);
        assert_eq!(effective_v8_flags(Some("   ")), DEFAULT_V8_FLAGS);
    }

    #[test]
    fn effective_v8_flags_user_overrides_default() {
        // V8 parses left-to-right and later wins, so the user value must
        // come after the default in the merged string.
        let user = "--max-old-space-size=8192";
        let merged = effective_v8_flags(Some(user));
        assert!(merged.starts_with(DEFAULT_V8_FLAGS));
        assert!(merged.ends_with(user));
    }

    #[test]
    fn effective_v8_flags_appends_user_extras() {
        let merged = effective_v8_flags(Some("--expose-gc"));
        assert!(merged.contains(DEFAULT_V8_FLAGS));
        assert!(merged.contains("--expose-gc"));
    }

    #[test]
    fn parsed_fetch_quiet_resolves_to_off_filter() {
        let args =
            Args::try_parse_from(["obscura", "fetch", "--quiet", "https://example.com"]).unwrap();
        let filter = select_log_filter(args.verbose, is_quiet_command(&args.command));
        assert_eq!(filter, "off");
    }

    #[test]
    fn fetch_wait_distinguishes_adaptive_default_from_fixed_delay() {
        let default = Args::try_parse_from(["obscura", "fetch", "https://example.com"]).unwrap();
        match default.command {
            Some(Command::Fetch { wait, .. }) => assert_eq!(wait, None),
            _ => panic!("expected Fetch command"),
        }

        let fixed =
            Args::try_parse_from(["obscura", "fetch", "https://example.com", "--wait", "0"])
                .unwrap();
        match fixed.command {
            Some(Command::Fetch { wait, .. }) => assert_eq!(wait, Some(0)),
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn fetch_screenshot_has_a_short_alias_and_rejects_batch_mode() {
        let args = Args::try_parse_from([
            "obscura",
            "fetch",
            "https://example.com",
            "-s",
            "page.png",
        ])
        .unwrap();
        match args.command {
            Some(Command::Fetch { screenshot, .. }) => {
                assert_eq!(screenshot, Some(std::path::PathBuf::from("page.png")));
            }
            _ => panic!("expected Fetch command"),
        }

        assert!(Args::try_parse_from([
            "obscura",
            "fetch",
            "--file",
            "urls.txt",
            "--screenshot",
            "page.png",
        ])
        .is_err());
    }

    fn configured_fetch_timeout(args: Args) -> std::time::Duration {
        let timeout = match args.command {
            Some(Command::Fetch { timeout, .. }) => timeout,
            _ => panic!("expected Fetch command"),
        };
        let context = std::sync::Arc::new(
            obscura_browser::BrowserContext::with_storage_and_network(
                "cli-timeout-test".to_string(),
                None,
                false,
                None,
                None,
                true,
            ),
        );
        let mut page = obscura_browser::Page::new("cli-timeout-test".to_string(), context);
        configure_fetch_navigation_timeout(&mut page, timeout);
        page.navigation_timeout()
    }

    #[test]
    fn fetch_timeout_sets_the_page_navigation_budget() {
        let args = Args::try_parse_from([
            "obscura",
            "fetch",
            "https://example.com",
            "--timeout",
            "50",
        ])
        .unwrap();
        assert_eq!(
            configured_fetch_timeout(args),
            std::time::Duration::from_secs(50)
        );
    }

    #[test]
    fn fetch_default_navigation_budget_remains_thirty_seconds() {
        let args = Args::try_parse_from(["obscura", "fetch", "https://example.com"]).unwrap();
        assert_eq!(
            configured_fetch_timeout(args),
            std::time::Duration::from_secs(30)
        );
    }

    #[test]
    fn matcher_still_uses_fetch_variant() {
        let cmd = Some(Command::Fetch {
            url: Some("https://x".to_string()),
            dump: Some(super::DumpFormat::Html),
            selector: None,
            file: None,
            concurrency: std::num::NonZeroUsize::new(1).unwrap(),
            wait: Some(5),
            timeout: 30,
            wait_until: "load".to_string(),
            user_agent: None,
            eval: None,
            quiet: true,
            output: None,
            storage_dir: None,
            screenshot: None,
        });
        assert!(is_quiet_command(&cmd));
    }

    fn body_text(html: &str) -> String {
        let dom = parse_html(html);
        let body = dom
            .query_selector("body")
            .ok()
            .flatten()
            .expect("body must exist");
        extract_readable_text(&dom, body)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn skips_nav_header_footer_aside() {
        let text = body_text(
            r#"<html><body>
                <header>SITE HEADER</header>
                <nav>NAV LINKS</nav>
                <aside>SIDEBAR</aside>
                <main><p>Article body.</p></main>
                <footer>FOOTER</footer>
            </body></html>"#,
        );
        assert!(text.contains("Article body."), "main content kept: {text}");
        for boilerplate in ["SITE HEADER", "NAV LINKS", "SIDEBAR", "FOOTER"] {
            assert!(
                !text.contains(boilerplate),
                "boilerplate '{boilerplate}' leaked through: {text}"
            );
        }
    }

    #[test]
    fn still_skips_script_and_style() {
        // Regression guard for the original skip list.
        let text = body_text(
            r#"<html><body>
                <p>Hello.</p>
                <script>console.log("nope")</script>
                <style>.x { color: red }</style>
            </body></html>"#,
        );
        assert!(text.contains("Hello."));
        assert!(!text.contains("console.log"));
        assert!(!text.contains("color: red"));
    }

    #[test]
    fn command_proxy_overrides_global_proxy() {
        let proxy = merge_proxy(
            Some("http://global.example:8080".to_string()),
            Some("socks5://127.0.0.1:1080".to_string()),
        );

        assert_eq!(proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
    }

    #[test]
    fn global_proxy_is_used_when_command_proxy_is_absent() {
        let proxy = merge_proxy(Some("http://global.example:8080".to_string()), None);

        assert_eq!(proxy.as_deref(), Some("http://global.example:8080"));
    }

    #[test]
    fn parsed_fetch_dump_assets_is_accepted_by_clap() {
        let args = Args::try_parse_from([
            "obscura",
            "fetch",
            "--dump",
            "assets",
            "https://example.com",
        ])
        .expect("clap should accept --dump assets");
        match args.command {
            Some(Command::Fetch { dump, .. }) => {
                assert_eq!(dump, Some(DumpFormat::Assets));
            }
            _ => panic!("expected Fetch command"),
        }
    }

    #[test]
    fn resolve_asset_url_keeps_absolute_unchanged() {
        let base = url::Url::parse("https://page.test/a/b").unwrap();
        let abs = "https://cdn.test/x.js";
        assert_eq!(resolve_asset_url(abs, Some(&base)).as_deref(), Some(abs));
    }

    #[test]
    fn resolve_asset_url_joins_relative_against_base() {
        let base = url::Url::parse("https://page.test/a/b").unwrap();
        let rel = "/static/x.js";
        assert_eq!(
            resolve_asset_url(rel, Some(&base)).as_deref(),
            Some("https://page.test/static/x.js"),
        );
    }

    #[test]
    fn resolve_asset_url_drops_empty() {
        let base = url::Url::parse("https://page.test/").unwrap();
        assert!(resolve_asset_url("", Some(&base)).is_none());
        assert!(resolve_asset_url("   ", Some(&base)).is_none());
    }

    #[test]
    fn link_kind_from_rel_handles_common_values() {
        assert_eq!(link_kind_from_rel("stylesheet"), "stylesheet");
        assert_eq!(link_kind_from_rel("icon"), "icon");
        // First token wins for multi-token rel (e.g. "shortcut icon").
        assert_eq!(link_kind_from_rel("shortcut icon"), "icon");
        assert_eq!(link_kind_from_rel("manifest"), "manifest");
        assert_eq!(link_kind_from_rel("preload"), "preload");
        assert_eq!(link_kind_from_rel("prefetch"), "prefetch");
        assert_eq!(link_kind_from_rel("modulepreload"), "modulepreload");
        assert_eq!(link_kind_from_rel("dns-prefetch"), "dns-prefetch");
        assert_eq!(link_kind_from_rel("preconnect"), "preconnect");
        assert_eq!(link_kind_from_rel("alternate"), "alternate");
        // Empty / unknown falls back to generic "link" so URL is still emitted.
        assert_eq!(link_kind_from_rel(""), "link");
        assert_eq!(link_kind_from_rel("noopener"), "link");
    }

    #[test]
    fn extract_assets_covers_every_resource_tag() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/site.css">
            <link rel="icon" href="/favicon.ico">
            <link rel="preload" href="/font.woff2">
            <link href="/no-rel.css">
            <script src="/app.js"></script>
        </head><body>
            <img src="/logo.png">
            <iframe src="/frame.html"></iframe>
            <video src="/clip.mp4"><source src="/clip.webm"></video>
            <audio src="/track.mp3"></audio>
            <embed src="/widget.swf">
            <object data="/doc.pdf"></object>
        </body></html>"#;
        let dom = obscura_dom::parse_html(html);
        let base = url::Url::parse("https://example.test/page").unwrap();
        let ndjson = extract_assets(&dom, Some(&base));
        let records: Vec<serde_json::Value> = ndjson
            .lines()
            .map(|line| serde_json::from_str(line).expect("each line must be valid JSON"))
            .collect();

        // Every emitted record must have an absolute URL on example.test
        // and a non-empty type string. Pin specific entries so a regression
        // in selectors or kind mapping fails loudly.
        for r in &records {
            let url = r["url"].as_str().unwrap();
            assert!(
                url.starts_with("https://example.test/"),
                "url not absolute: {url}",
            );
            assert!(!r["type"].as_str().unwrap().is_empty());
        }

        let pairs: Vec<(String, String)> = records
            .iter()
            .map(|r| {
                (
                    r["url"].as_str().unwrap().to_string(),
                    r["type"].as_str().unwrap().to_string(),
                )
            })
            .collect();

        assert!(pairs.contains(&(
            "https://example.test/app.js".to_string(),
            "script".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/site.css".to_string(),
            "stylesheet".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/favicon.ico".to_string(),
            "icon".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/font.woff2".to_string(),
            "preload".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/no-rel.css".to_string(),
            "link".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/logo.png".to_string(),
            "image".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/frame.html".to_string(),
            "iframe".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/clip.mp4".to_string(),
            "video".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/clip.webm".to_string(),
            "media".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/track.mp3".to_string(),
            "audio".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/widget.swf".to_string(),
            "embed".to_string(),
        )));
        assert!(pairs.contains(&(
            "https://example.test/doc.pdf".to_string(),
            "object".to_string(),
        )));
    }

    #[test]
    fn extract_assets_skips_empty_attributes() {
        let html = r#"<html><body>
            <script src=""></script>
            <img src="   ">
            <iframe src="/ok.html"></iframe>
        </body></html>"#;
        let dom = obscura_dom::parse_html(html);
        let base = url::Url::parse("https://example.test/").unwrap();
        let ndjson = extract_assets(&dom, Some(&base));
        let lines: Vec<&str> = ndjson.lines().collect();
        // Only the iframe with a non-empty src survives.
        assert_eq!(lines.len(), 1, "got {lines:?}");
        assert!(lines[0].contains("\"https://example.test/ok.html\""));
        assert!(lines[0].contains("\"iframe\""));
    }
}
