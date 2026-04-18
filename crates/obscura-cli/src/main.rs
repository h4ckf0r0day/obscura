use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, Subcommand};
use obscura_browser::{BrowserContext, Page};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;

#[derive(Parser)]
#[command(name = "obscura", about = "Obscura - A lightweight headless browser for web scraping and automation")]
struct Args {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long, default_value_t = 9222)]
    port: u16,

    #[arg(long)]
    proxy: Option<String>,

    #[arg(long)]
    obey_robots: bool,

    #[arg(long)]
    user_agent: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(short, long, default_value_t = 9222)]
        port: u16,

        #[arg(long)]
        proxy: Option<String>,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long)]
        stealth: bool,

        #[arg(long, default_value_t = 1)]
        workers: u16,
    },

    Fetch {
        url: String,

        #[arg(long, default_value = "html")]
        dump: DumpFormat,

        #[arg(long)]
        selector: Option<String>,

        #[arg(long, default_value_t = 5)]
        wait: u64,

        #[arg(long, default_value = "load")]
        wait_until: String,

        #[arg(long)]
        user_agent: Option<String>,

        #[arg(long)]
        stealth: bool,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, short)]
        quiet: bool,
    },

    Scrape {
        urls: Vec<String>,

        #[arg(long, short)]
        eval: Option<String>,

        #[arg(long, default_value_t = 10)]
        concurrency: usize,

        #[arg(long, default_value = "json")]
        format: String,
    },

}


#[derive(Clone, Debug, clap::ValueEnum)]
enum DumpFormat {
    Html,
    Text,
    Links,
}

fn print_banner(port: u16) {
    println!(r#"
   ____  _                              
  / __ \| |                             
 | |  | | |__  ___  ___ _   _ _ __ __ _ 
 | |  | | '_ \/ __|/ __| | | | '__/ _` |
 | |__| | |_) \__ \ (__| |_| | | | (_| |
  \____/|_.__/|___/\___|\__,_|_|  \__,_|
                   
  Headless Browser v0.1.0
  CDP server: ws://127.0.0.1:{}/devtools/browser
"#, port);
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let filter = if args.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    match args.command {
        Some(Command::Serve { port, proxy, user_agent, stealth, workers }) => {
            print_banner(port);
            if let Some(ref proxy) = proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            if let Some(ref ua) = user_agent {
                tracing::info!("User-Agent: {}", ua);
            }
            if stealth {
                tracing::info!("Stealth mode enabled (TLS fingerprint spoofing)");
            }
            let _ = stealth;

            if workers > 1 {
                tracing::info!("{} worker processes", workers);
                run_multi_worker_serve(port, workers, proxy, stealth).await?;
            } else {
                obscura_cdp::start_with_options(port, proxy).await?;
            }
        }
        Some(Command::Fetch { url, dump, selector, wait, wait_until, user_agent, stealth, eval, quiet }) => {
            run_fetch(&url, dump, selector, wait, &wait_until, user_agent, stealth, eval, quiet).await?;
        }
        Some(Command::Scrape { urls, eval, concurrency, format }) => {
            run_parallel_scrape(urls, eval, concurrency, &format).await?;
        }
        None => {
            print_banner(args.port);
            if let Some(ref proxy) = args.proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            obscura_cdp::start_with_options(args.port, args.proxy).await?;
        }
    }

    Ok(())
}

async fn run_multi_worker_serve(
    port: u16,
    workers: u16,
    proxy: Option<String>,
    stealth: bool,
) -> anyhow::Result<()> {
    use tokio::net::TcpListener;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let exe = std::env::current_exe()?;
    let mut children = Vec::new();

    for i in 0..workers {
        let worker_port = port + 1 + i;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("serve").arg("--port").arg(worker_port.to_string());
        if let Some(ref p) = proxy {
            cmd.arg("--proxy").arg(p);
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
                if let Ok(mut worker_stream) = tokio::net::TcpStream::connect(&worker_addr).await {
                    tokio::spawn(async move {
                        let std_stream = match client_stream.into_std() {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::error!("Failed to convert client stream to std: {}", e);
                                return;
                            }
                        };
                        let mut client = tokio::net::TcpStream::from_std(std_stream);
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut worker_stream).await;
                    });
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
                    tracing::warn!("Worker {} unreachable: {}", worker_addr, e);
                    let response = b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n";
                    let _ = client_stream.write_all(response).await;
                    let _ = client_stream.shutdown().await;
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
    wait_until: &str,
    user_agent: Option<String>,
    stealth: bool,
    eval: Option<String>,
    quiet: bool,
) -> anyhow::Result<()> {
    let context = Arc::new(BrowserContext::with_options("fetch".to_string(), None, stealth));
    let mut page = Page::new("fetch-page".to_string(), context);

    if let Some(ref ua) = user_agent {
        page.http_client.set_user_agent(ua).await;
    }

    let wait_condition = obscura_browser::lifecycle::WaitUntil::from_str(wait_until);

    if !quiet {
        eprintln!("Fetching {}...", url_str);
    }

    page.navigate_with_wait(url_str, wait_condition).await.map_err(|e| {
        anyhow::anyhow!("Failed to navigate to {}: {}", url_str, e)
    })?;

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
        match result {
            serde_json::Value::String(s) => println!("{}", s),
            serde_json::Value::Null => println!("null"),
            other => println!("{}", other),
        }
        return Ok(());
    }

    match dump {
        DumpFormat::Html => {
            dump_html(&page);
        }
        DumpFormat::Text => {
            dump_text(&mut page);
        }
        DumpFormat::Links => {
            dump_links(&page);
        }
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

fn dump_html(page: &Page) {
    page.with_dom(|dom| {
        if let Ok(Some(html_node)) = dom.query_selector("html") {
            let html = dom.outer_html(html_node);
            println!("<!DOCTYPE html>");
            println!("{}", html);
        } else {
            let doc = dom.document();
            let html = dom.inner_html(doc);
            println!("{}", html);
        }
    });
}

fn dump_text(page: &mut Page) {
    page.with_dom(|dom| {
        if let Ok(Some(body)) = dom.query_selector("body") {
            let text = extract_readable_text(dom, body);
            println!("{}", text.trim());
        }
    });
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

            if tag == "script" || tag == "style" {
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
) -> anyhow::Result<()> {
    let total = urls.len();
    let start = Instant::now();

    eprintln!(
        "Scraping {} URLs with {} concurrent workers...",
        total, concurrency
    );

    let worker_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("obscura-worker")))
        .unwrap_or_else(|| std::path::PathBuf::from("obscura-worker"));

    if !worker_path.exists() {
        anyhow::bail!(
            "Worker binary not found at {}. Build with: cargo build --release",
            worker_path.display()
        );
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let eval = Arc::new(eval);
    let worker_path = Arc::new(worker_path);

    let mut handles = Vec::new();

    for (i, url) in urls.into_iter().enumerate() {
        let sem = semaphore.clone();
        let eval = eval.clone();
        let worker_path = worker_path.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let task_start = Instant::now();

            let mut child = match TokioCommand::new(worker_path.as_ref())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
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

            let stdin = child.stdin.as_mut().unwrap();
            let stdout = child.stdout.take().unwrap();
            let mut reader = BufReader::new(stdout);

            let nav_cmd = serde_json::json!({"cmd": "navigate", "url": url});
            let mut line = serde_json::to_string(&nav_cmd).unwrap();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() {
                let _ = child.kill().await;
                return serde_json::json!({"url": url, "error": "Write failed"});
            }
            let _ = stdin.flush().await;

            let mut resp_line = String::new();
            if reader.read_line(&mut resp_line).await.is_err() {
                let _ = child.kill().await;
                return serde_json::json!({"url": url, "error": "Read failed"});
            }

            let nav_resp: serde_json::Value =
                serde_json::from_str(resp_line.trim()).unwrap_or(serde_json::json!({"ok": false}));

            if !nav_resp["ok"].as_bool().unwrap_or(false) {
                let _ = child.kill().await;
                return serde_json::json!({
                    "url": url,
                    "error": nav_resp["error"].as_str().unwrap_or("navigate failed"),
                    "time_ms": task_start.elapsed().as_millis(),
                });
            }

            let title = nav_resp["result"]["title"]
                .as_str()
                .unwrap_or("")
                .to_string();

            let eval_result = if let Some(ref expr) = *eval {
                let eval_cmd = serde_json::json!({"cmd": "evaluate", "expression": expr});
                let mut line = serde_json::to_string(&eval_cmd).unwrap();
                line.push('\n');
                let _ = stdin.write_all(line.as_bytes()).await;
                let _ = stdin.flush().await;

                let mut resp_line = String::new();
                if reader.read_line(&mut resp_line).await.is_ok() {
                    let resp: serde_json::Value = serde_json::from_str(resp_line.trim())
                        .unwrap_or(serde_json::json!({"ok": false}));
                    resp["result"].clone()
                } else {
                    serde_json::Value::Null
                }
            } else {
                serde_json::Value::Null
            };

            let shutdown_cmd = serde_json::json!({"cmd": "shutdown"});
            let mut line = serde_json::to_string(&shutdown_cmd).unwrap();
            line.push('\n');
            let _ = stdin.write_all(line.as_bytes()).await;
            let _ = stdin.flush().await;
            let _ = child.wait().await;

            let elapsed = task_start.elapsed().as_millis();

            serde_json::json!({
                "url": url,
                "title": title,
                "eval": eval_result,
                "time_ms": elapsed,
                "worker": i,
            })
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
        eprintln!(
            "\nTotal: {}ms for {} URLs ({} concurrent)",
            total_time.as_millis(),
            total,
            concurrency
        );
    }

    Ok(())
}

fn dump_links(page: &Page) {
    let base_url = page.url.clone();
    page.with_dom(|dom| {
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
                        println!("{}", full_url);
                    } else {
                        println!("{}\t{}", full_url, text);
                    }
                }
            }
        }
    });
}
