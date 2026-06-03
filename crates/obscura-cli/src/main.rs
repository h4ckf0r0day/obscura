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
    version = env!("CARGO_PKG_VERSION"),
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

    #[arg(long)]
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

        #[arg(long)]
        stealth: bool,

        #[arg(long, default_value_t = 1)]
        workers: u16,

        /// Allow CDP clients to navigate to file:// URLs. Off by
        /// default so a CDP connection cannot read arbitrary local
        /// files. Enable only when serving local HTML for testing
        /// and the port is on a trusted network.
        #[arg(long)]
        allow_file_access: bool,

        #[arg(long)]
        storage_dir: Option<std::path::PathBuf>,
    },

    Fetch {
        url: String,

        #[arg(long, default_value = "html")]
        dump: DumpFormat,

        #[arg(long)]
        selector: Option<String>,

        #[arg(long, default_value_t = 5)]
        wait: u64,

        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,

        #[arg(long, default_value = "load")]
        wait_until: String,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long)]
        stealth: bool,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, short = 'o')]
        output: Option<std::path::PathBuf>,

        #[arg(long, short)]
        quiet: bool,

        #[arg(long)]
        storage_dir: Option<std::path::PathBuf>,
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

        #[arg(long, default_value_t = 3000)]
        port: u16,

        #[arg(long)]
        proxy: Option<String>,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long)]
        stealth: bool,
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
}

fn print_banner(port: u16) {
    println!(r#"
   ____  _                              
  / __ \| |                             
 | |  | | |__  ___  ___ _   _ _ __ __ _ 
 | |  | | '_ \/ __|/ __| | | | '__/ _` |
 | |__| | |_) \__ \ (__| |_| | | | (_| |
  \____/|_.__/|___/\___|\__,_|_|  \__,_|
                   
  Headless Browser v0.1.6
  CDP server: ws://127.0.0.1:{}/devtools/browser
"#, port);
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
        Some(Command::Fetch { quiet: true, .. }) | Some(Command::Scrape { quiet: true, .. })
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
#[cfg(target_pointer_width = "64")]
const DEFAULT_V8_FLAGS: &str = "--max-old-space-size=4096";
#[cfg(not(target_pointer_width = "64"))]
const DEFAULT_V8_FLAGS: &str = "--max-old-space-size=1024";

fn effective_v8_flags(user: Option<&str>) -> String {
    match normalize_v8_flags(user) {
        Some(u) => format!("{} {}", DEFAULT_V8_FLAGS, u),
        None => DEFAULT_V8_FLAGS.to_string(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

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

    let global_proxy = args.proxy.clone();

    match args.command {
        Some(Command::Serve { port, host, proxy, user_agent, stealth, workers, allow_file_access, storage_dir }) => {
            let proxy = merge_proxy(global_proxy.clone(), proxy);
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
                run_multi_worker_serve(port, workers, proxy, stealth, user_agent).await?;
            } else {
                obscura_cdp::start_with_full_serve_options(
                    port, &host, proxy, stealth, user_agent, allow_file_access, storage_dir,
                    args.allow_private_network,
                ).await?;
            }
        }
        Some(Command::Fetch { url, dump, selector, wait, timeout, wait_until, user_agent, stealth, eval, output, quiet, storage_dir }) => {
            run_fetch(&url, dump, selector, wait, timeout, &wait_until, user_agent, stealth, eval, output, quiet, global_proxy, storage_dir, args.allow_private_network).await?;
        }
        Some(Command::Scrape { urls, eval, concurrency, format, timeout, quiet }) => {
            run_parallel_scrape(urls, eval, concurrency.get(), &format, timeout, quiet, global_proxy).await?;
        }
        Some(Command::Mcp { http, port, proxy, user_agent, stealth }) => {
            let mcp_proxy = merge_proxy(global_proxy.clone(), proxy);
            if http {
                obscura_mcp::http::run(port, mcp_proxy, user_agent, stealth).await?;
            } else {
                obscura_mcp::run(mcp_proxy, user_agent, stealth).await?;
            }
        }
        None => {
            print_banner(args.port);
            if let Some(ref proxy) = args.proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            obscura_cdp::start_with_options(args.port, args.proxy, false).await?;
        }
    }

    Ok(())
}

async fn run_multi_worker_serve(
    port: u16,
    workers: u16,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
) -> anyhow::Result<()> {
    use tokio::net::TcpListener;
    use tokio::io::AsyncWriteExt as _;

    let exe = std::env::current_exe()?;
    let mut children = Vec::new();

    for i in 0..workers {
        let worker_port = port + 1 + i;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("serve").arg("--port").arg(worker_port.to_string());
        if let Some(ref p) = proxy {
            cmd.arg("--proxy").arg(p);
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

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Load balancer on port {}, {} workers", port, workers);

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
                            let _ = tokio::io::copy_bidirectional(
                                &mut client,
                                &mut worker_stream,
                            )
                            .await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!("/json worker {} unreachable: {}", worker_addr, e);
                        tokio::spawn(async move {
                            let mut s = client_stream;
                            let _ = s
                                .write_all(
                                    b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n",
                                )
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
                    let _ =
                        tokio::io::copy_bidirectional(&mut client, &mut worker_stream).await;
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

async fn run_fetch(
    url_str: &str,
    dump: DumpFormat,
    selector: Option<String>,
    wait_secs: u64,
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
) -> anyhow::Result<()> {
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
        )
        .await?;
        write_or_print_bytes(&bytes, output.as_ref()).await?;
        return Ok(());
    }

    let context = Arc::new(BrowserContext::with_storage_and_network(
        "fetch".to_string(),
        proxy,
        stealth,
        user_agent.clone(),
        storage_dir.clone(),
        allow_private_network,
    ));
    let mut page = Page::new("fetch-page".to_string(), context.clone());

    if let Some(ref ua) = user_agent {
        page.http_client.set_user_agent(ua).await;
    }

    let wait_condition = obscura_browser::lifecycle::WaitUntil::from_str(wait_until);

    if !quiet {
        eprintln!("Fetching {}...", url_str);
    }

    match timeout(Duration::from_secs(timeout_secs), page.navigate_with_wait(url_str, wait_condition)).await {
        Ok(result) => result.map_err(|e| anyhow::anyhow!("Failed to navigate to {}: {}", url_str, e))?,
        Err(_) => anyhow::bail!(
            "Timed out navigating to {} after {}s",
            url_str,
            timeout_secs
        ),
    }

    if !quiet {
        eprintln!("Page loaded: {} - \"{}\"", page.url_string(), page.title);
    }

    if let Some(ref sel) = selector {
        let found = wait_for_selector(&mut page, sel, wait_secs).await;
        if !found {
            eprintln!("Warning: selector '{}' not found after {}s", sel, wait_secs);
        }
    }

    if let Some(ref expr) = eval {
        let result = page.evaluate(expr);
        let rendered = match result {
            serde_json::Value::String(s) => s,
            serde_json::Value::Null => "null".to_string(),
            other => other.to_string(),
        };
        write_or_print(rendered, output.as_ref()).await?;
        context.save_cookies();
        return Ok(());
    }

    let rendered = match dump {
        DumpFormat::Html => dump_html(&page),
        DumpFormat::Text => dump_text(&mut page),
        DumpFormat::Links => dump_links(&page),
        DumpFormat::Markdown => dump_markdown(&mut page),
        DumpFormat::Assets => dump_assets(&page),
        // Handled above via the short-circuit branch; unreachable here.
        DumpFormat::Original => unreachable!("Original dump handled before page navigation"),
    };
    write_or_print(rendered, output.as_ref()).await?;

    // Save cookies to disk if storage_dir is configured
    context.save_cookies();

    Ok(())
}

async fn fetch_original_bytes(
    url_str: &str,
    proxy: Option<String>,
    user_agent: Option<String>,
    timeout_secs: u64,
) -> anyhow::Result<Vec<u8>> {
    let url = url::Url::parse(url_str)
        .map_err(|e| anyhow::anyhow!("Invalid URL '{}': {}", url_str, e))?;

    let client = obscura_net::ObscuraHttpClient::with_options(
        Arc::new(obscura_net::CookieJar::new()),
        proxy.as_deref(),
    );
    if let Some(ua) = user_agent {
        client.set_user_agent(&ua).await;
    }

    let response = match timeout(Duration::from_secs(timeout_secs), client.fetch(&url)).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => anyhow::bail!("Failed to fetch {}: {}", url_str, e),
        Err(_) => anyhow::bail!("Timed out fetching {} after {}s", url_str, timeout_secs),
    };

    Ok(response.body)
}

async fn write_or_print(content: String, output: Option<&std::path::PathBuf>) -> anyhow::Result<()> {
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
        let found = page.with_dom(|dom| {
            dom.query_selector(selector).ok().flatten().is_some()
        }).unwrap_or(false);

        if found {
            return true;
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
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
    }).unwrap_or_default()
}

fn dump_text(page: &mut Page) -> String {
    page.with_dom(|dom| {
        if let Ok(Some(body)) = dom.query_selector("body") {
            let text = extract_readable_text(dom, body);
            text.trim().to_string()
        } else {
            String::new()
        }
    }).unwrap_or_default()
}

fn dump_markdown(page: &mut Page) -> String {
    let result = page.evaluate(obscura_browser::HTML_TO_MARKDOWN_JS);
    result.as_str().unwrap_or_default().to_string()
}

fn extract_readable_text(dom: &obscura_dom::DomTree, node_id: obscura_dom::NodeId) -> String {
    use obscura_dom::NodeData;

    let mut result = String::new();
    let node = match dom.get_node(node_id) {
        Some(n) => n,
        None => return result,
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
            let is_block = matches!(
                tag,
                "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "li" | "tr" | "br" | "hr" | "blockquote" | "pre"
                    | "section" | "article" | "header" | "footer" | "nav"
                    | "main" | "aside" | "figure" | "figcaption" | "table"
                    | "thead" | "tbody" | "tfoot" | "dl" | "dt" | "dd"
                    | "ul" | "ol"
            );

            // Boilerplate elements rarely contain content the user wants to
            // scrape — strip them so `--dump text` returns the article body
            // instead of menus, footers, and cookie banners.
            if matches!(
                tag,
                "script" | "style" | "nav" | "header" | "footer" | "aside"
            ) {
                return result;
            }

            if is_block {
                result.push('\n');
            }

            for child_id in dom.children(node_id) {
                result.push_str(&extract_readable_text(dom, child_id));
            }

            if is_block {
                result.push('\n');
            }
        }
        _ => {
            for child_id in dom.children(node_id) {
                result.push_str(&extract_readable_text(dom, child_id));
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

    let worker_name = if cfg!(windows) { "obscura-worker.exe" } else { "obscura-worker" };
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

            let worker_result: Result<serde_json::Value, String> = match timeout(worker_timeout, async {
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

                let nav_resp: serde_json::Value =
                    serde_json::from_str(resp_line.trim()).unwrap_or(serde_json::json!({"ok": false}));

                if !nav_resp["ok"].as_bool().unwrap_or(false) {
                    return Err(
                        nav_resp["error"]
                            .as_str()
                            .unwrap_or("navigate failed")
                            .to_string(),
                    );
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
                            let resp: serde_json::Value = serde_json::from_str(resp_line.trim())
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
                    base.join(&href).map(|u| u.to_string()).unwrap_or(href.clone())
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
    }).unwrap_or_default()
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
    match rel.split_ascii_whitespace().next().unwrap_or("").to_ascii_lowercase().as_str() {
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
            let Some(node) = dom.get_node(node_id) else { continue };
            let raw = node.get_attribute(attr).unwrap_or_default().to_string();
            let Some(url) = resolve_asset_url(&raw, base_url) else { continue };

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
    page.with_dom(|dom| extract_assets(dom, base_url.as_ref())).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        effective_v8_flags, extract_assets, extract_readable_text, fetch_original_bytes,
        is_quiet_command, link_kind_from_rel, merge_proxy, normalize_v8_flags,
        resolve_asset_url, select_log_filter, write_or_print,
        write_or_print_bytes, Args, Command, DumpFormat, DEFAULT_V8_FLAGS,
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
                assert_eq!(dump, DumpFormat::Original);
            }
            _ => panic!("expected Fetch command"),
        }
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
        let bytes = fetch_original_bytes(&file_url, None, None, 5)
            .await
            .expect("fetch_original_bytes should round-trip the file body");

        let _ = tokio::fs::remove_file(&path).await;

        assert_eq!(bytes, PNG_BYTES, "raw response body must match the file byte-for-byte");
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

        assert_eq!(read_back, payload, "file bytes must match the payload exactly");
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
        let args = Args::try_parse_from([
            "obscura",
            "fetch",
            "--quiet",
            "https://example.com",
        ])
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
        let args = Args::try_parse_from(["obscura", "serve"])
            .expect("clap should accept serve");
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
        let args = Args::try_parse_from([
            "obscura",
            "--v8-flags",
            "",
            "fetch",
            "https://example.com",
        ])
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
        let args = Args::try_parse_from([
            "obscura",
            "fetch",
            "--quiet",
            "https://example.com",
        ])
        .unwrap();
        let filter = select_log_filter(args.verbose, is_quiet_command(&args.command));
        assert_eq!(filter, "off");
    }

    #[test]
    fn matcher_still_uses_fetch_variant() {
        let cmd = Some(Command::Fetch {
            url: "https://x".to_string(),
            dump: super::DumpFormat::Html,
            selector: None,
            wait: 5,
            timeout: 30,
            wait_until: "load".to_string(),
            user_agent: None,
            stealth: false,
            eval: None,
            quiet: true,
            output: None,
            storage_dir: None,
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
        extract_readable_text(&dom, body).split_whitespace().collect::<Vec<_>>().join(" ")
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
                assert_eq!(dump, DumpFormat::Assets);
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
