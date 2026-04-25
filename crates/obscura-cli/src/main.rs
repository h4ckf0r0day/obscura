use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::{Parser, Subcommand};
use obscura_browser::{BrowserContext, Page};
use obscura_net::FileUrlPolicy;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};

#[derive(Parser)]
#[command(name = "obscura", about = "Obscura - A lightweight headless browser for web scraping and automation")]
struct Args {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long, default_value_t = 9222)]
    port: u16,

    #[arg(long, global = true, default_value = "127.0.0.1")]
    host: IpAddr,

    #[arg(long, global = true, value_name = "DIR")]
    allow_file_access: Vec<PathBuf>,

    #[arg(long, global = true)]
    allow_all_file_urls: bool,

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

        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
    },

}


#[derive(Clone, Debug, clap::ValueEnum)]
enum DumpFormat {
    Html,
    Text,
    Links,
}

fn print_banner(host: IpAddr, port: u16) {
    println!(r#"
   ____  _                              
  / __ \| |                             
 | |  | | |__  ___  ___ _   _ _ __ __ _ 
 | |  | | '_ \/ __|/ __| | | | '__/ _` |
 | |__| | |_) \__ \ (__| |_| | | | (_| |
  \____/|_.__/|___/\___|\__,_|_|  \__,_|
                   
  Headless Browser v0.1.0
  CDP bind address: {}:{}
"#, host, port);
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let host = args.host;
    let file_url_policy =
        build_file_url_policy(args.allow_all_file_urls, &args.allow_file_access)?;

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
            print_banner(host, port);
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
                run_multi_worker_serve(host, port, workers, proxy, stealth, file_url_policy).await?;
            } else {
                obscura_cdp::start_with_bind_and_file_url_policy(
                    port,
                    host,
                    proxy,
                    file_url_policy,
                )
                .await?;
            }
        }
        Some(Command::Fetch { url, dump, selector, wait, wait_until, user_agent, stealth, eval, quiet }) => {
            run_fetch(
                &url,
                dump,
                selector,
                wait,
                &wait_until,
                user_agent,
                stealth,
                eval,
                quiet,
                file_url_policy,
            )
            .await?;
        }
        Some(Command::Scrape { urls, eval, concurrency, format, timeout }) => {
            run_parallel_scrape(urls, eval, concurrency, &format, timeout, file_url_policy).await?;
        }
        None => {
            print_banner(host, args.port);
            if let Some(ref proxy) = args.proxy {
                tracing::info!("Using proxy: {}", proxy);
            }
            obscura_cdp::start_with_bind_and_file_url_policy(
                args.port,
                host,
                args.proxy,
                file_url_policy,
            )
            .await?;
        }
    }

    Ok(())
}

fn build_file_url_policy(
    allow_all_file_urls: bool,
    allow_file_access: &[PathBuf],
) -> anyhow::Result<FileUrlPolicy> {
    if allow_all_file_urls && !allow_file_access.is_empty() {
        anyhow::bail!(
            "--allow-all-file-urls cannot be combined with --allow-file-access <DIR>"
        );
    }

    if allow_all_file_urls {
        Ok(FileUrlPolicy::AllowAll)
    } else {
        Ok(FileUrlPolicy::allow_roots(allow_file_access.iter())?)
    }
}

fn add_file_policy_args(cmd: &mut std::process::Command, file_url_policy: &FileUrlPolicy) {
    match file_url_policy {
        FileUrlPolicy::Deny => {}
        FileUrlPolicy::AllowAll => {
            cmd.arg("--allow-all-file-urls");
        }
        FileUrlPolicy::AllowRoots(roots) => {
            for root in roots {
                cmd.arg("--allow-file-access").arg(root);
            }
        }
    }
}

fn add_file_policy_env(cmd: &mut TokioCommand, file_url_policy: &FileUrlPolicy) {
    const ALLOW_ALL_ENV: &str = "OBSCURA_ALLOW_ALL_FILE_URLS";
    const ROOTS_ENV: &str = "OBSCURA_FILE_ACCESS_ROOTS_JSON";

    match file_url_policy {
        FileUrlPolicy::Deny => {
            cmd.env_remove(ALLOW_ALL_ENV);
            cmd.env_remove(ROOTS_ENV);
        }
        FileUrlPolicy::AllowAll => {
            cmd.env(ALLOW_ALL_ENV, "1");
            cmd.env_remove(ROOTS_ENV);
        }
        FileUrlPolicy::AllowRoots(roots) => {
            let roots: Vec<String> = roots
                .iter()
                .map(|root| root.to_string_lossy().into_owned())
                .collect();
            if let Ok(roots_json) = serde_json::to_string(&roots) {
                cmd.env(ALLOW_ALL_ENV, "0");
                cmd.env(ROOTS_ENV, roots_json);
            }
        }
    }
}

async fn run_multi_worker_serve(
    host: IpAddr,
    port: u16,
    workers: u16,
    proxy: Option<String>,
    stealth: bool,
    file_url_policy: FileUrlPolicy,
) -> anyhow::Result<()> {
    use tokio::net::TcpListener;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let exe = std::env::current_exe()?;
    let mut children = Vec::new();

    for i in 0..workers {
        let worker_port = port + 1 + i;
        let mut cmd = std::process::Command::new(&exe);
        add_file_policy_args(&mut cmd, &file_url_policy);
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

    let addr = std::net::SocketAddr::new(host, port);
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
                let worker_addr = format!("127.0.0.1:{}", port + 1);
                if let Ok(mut worker_stream) = tokio::net::TcpStream::connect(&worker_addr).await {
                    tokio::spawn(async move {
                        let _ = tokio::io::copy_bidirectional(&mut tokio::net::TcpStream::from_std(client_stream.into_std().unwrap()).unwrap(), &mut worker_stream).await;
                    });
                }
                continue;
            }
        }

        let worker_addr = format!("127.0.0.1:{}", worker_port);
        tokio::spawn(async move {
            if let Ok(mut worker_stream) = tokio::net::TcpStream::connect(&worker_addr).await {
                let mut client = client_stream;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut worker_stream).await;
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
    file_url_policy: FileUrlPolicy,
) -> anyhow::Result<()> {
    let context = Arc::new(BrowserContext::with_options_and_file_url_policy(
        "fetch".to_string(),
        None,
        stealth,
        file_url_policy,
    ));
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
    timeout_secs: u64,
    file_url_policy: FileUrlPolicy,
) -> anyhow::Result<()> {
    let total = urls.len();
    let start = Instant::now();

    eprintln!(
        "Scraping {} URLs with {} concurrent workers (per-worker timeout: {}s)...",
        total, concurrency, timeout_secs
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
    let file_url_policy = Arc::new(file_url_policy);
    let worker_timeout = Duration::from_secs(timeout_secs);
    let read_timeout = Duration::from_secs(timeout_secs.min(30));
    let shutdown_timeout = Duration::from_secs(5);

    let mut handles = Vec::new();

    for (i, url) in urls.into_iter().enumerate() {
        let sem = semaphore.clone();
        let eval = eval.clone();
        let worker_path = worker_path.clone();
        let file_url_policy = file_url_policy.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let task_start = Instant::now();

            let mut worker_cmd = TokioCommand::new(worker_path.as_ref());
            add_file_policy_env(&mut worker_cmd, &file_url_policy);
            let mut child = match worker_cmd
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
