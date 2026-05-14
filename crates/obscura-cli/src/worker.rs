
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
enum WorkerCommand {
    #[serde(rename = "navigate")]
    Navigate { url: String },
    #[serde(rename = "evaluate")]
    Evaluate { expression: String },
    #[serde(rename = "title")]
    Title,
    #[serde(rename = "dump_html")]
    DumpHtml,
    #[serde(rename = "dump_text")]
    DumpText,
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Serialize)]
struct WorkerResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl WorkerResponse {
    fn success(result: serde_json::Value) -> Self {
        WorkerResponse { ok: true, result: Some(result), error: None }
    }
    fn error(msg: String) -> Self {
        WorkerResponse { ok: false, result: None, error: Some(msg) }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_writer(std::io::stderr)
        .init();

    let proxy = std::env::var("OBSCURA_PROXY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let context = Arc::new(BrowserContext::with_options("worker".to_string(), proxy, false));
    let mut page = Page::new("page-1".to_string(), context);

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("Worker stdin error: {}", e);
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cmd: WorkerCommand = match serde_json::from_str(trimmed) {
            Ok(c) => c,
            Err(e) => {
                let resp = WorkerResponse::error(format!("Invalid command: {}", e));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
                continue;
            }
        };

        let resp = match cmd {
            WorkerCommand::Navigate { url } => {
                match page.navigate(&url).await {
                    Ok(()) => WorkerResponse::success(serde_json::json!({
                        "title": page.title,
                        "url": page.url_string(),
                    })),
                    Err(e) => WorkerResponse::error(e.to_string()),
                }
            }
            WorkerCommand::Evaluate { expression } => {
                let result = page.evaluate(&expression);
                WorkerResponse::success(result)
            }
            WorkerCommand::Title => {
                WorkerResponse::success(serde_json::json!(page.title))
            }
            WorkerCommand::DumpHtml => {
                let html = page.with_dom(|dom| {
                    if let Ok(Some(html_node)) = dom.query_selector("html") {
                        dom.outer_html(html_node)
                    } else {
                        dom.inner_html(dom.document())
                    }
                }).unwrap_or_default();
                WorkerResponse::success(serde_json::json!(html))
            }
            WorkerCommand::DumpText => {
                let text = page.with_dom(|dom| {
                    if let Ok(Some(body)) = dom.query_selector("body") {
                        dom.text_content(body)
                    } else {
                        String::new()
                    }
                }).unwrap_or_default();
                WorkerResponse::success(serde_json::json!(text))
            }
            WorkerCommand::Shutdown => {
                let resp = WorkerResponse::success(serde_json::json!("bye"));
                let mut out = serde_json::to_string(&resp).unwrap();
                out.push('\n');
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
                break;
            }
        };

        let mut out = serde_json::to_string(&resp).unwrap();
        out.push('\n');
        let _ = stdout.write_all(out.as_bytes()).await;
        let _ = stdout.flush().await;
    }
}
