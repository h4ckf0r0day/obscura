use std::io::{self, BufRead, Read, Write as IoWrite};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

// ── JSON-RPC helpers ─────────────────────────────────────────────────────

pub fn parse_request(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

pub fn make_response(id: &Value, result: Value) -> String {
    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    format!("{resp}\n")
}

pub fn make_error(id: &Value, code: i32, message: &str) -> String {
    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    format!("{resp}\n")
}

// ── Tool definitions ─────────────────────────────────────────────────────

pub fn tools_list() -> Value {
    json!([
        {
            "name": "obscura_fetch",
            "description": "Fetch a URL using Obscura headless browser. Returns HTML, text content, or links. Supports JavaScript rendering, CSS selector waiting, and JS expression evaluation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch" },
                    "dump": { "type": "string", "enum": ["html", "text", "links"], "description": "Output format (default: html)" },
                    "eval": { "type": "string", "description": "JavaScript expression to evaluate" },
                    "wait_until": { "type": "string", "enum": ["load", "domcontentloaded", "networkidle0"], "description": "Wait condition (default: load)" },
                    "selector": { "type": "string", "description": "CSS selector to wait for" },
                    "stealth": { "type": "boolean", "description": "Enable anti-detection mode" },
                    "user_agent": { "type": "string", "description": "Custom User-Agent" },
                    "quiet": { "type": "boolean", "description": "Suppress banner" }
                },
                "required": ["url"]
            }
        },
        {
            "name": "obscura_scrape",
            "description": "Scrape multiple URLs in parallel with Obscura. Returns JSON or text with configurable concurrency.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "urls": { "type": "array", "items": { "type": "string" }, "description": "List of URLs to scrape" },
                    "eval": { "type": "string", "description": "JavaScript expression per page" },
                    "concurrency": { "type": "number", "description": "Parallel workers (default: 10)" },
                    "format": { "type": "string", "enum": ["json", "text"], "description": "Output format (default: json)" }
                },
                "required": ["urls"]
            }
        },
        {
            "name": "obscura_serve",
            "description": "Start an Obscura CDP WebSocket server. Returns connection info. Runs as a background process.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": { "type": "number", "description": "WebSocket port (default: 9222)" },
                    "stealth": { "type": "boolean", "description": "Anti-detection + tracker blocking" },
                    "proxy": { "type": "string", "description": "HTTP/SOCKS5 proxy URL" },
                    "workers": { "type": "number", "description": "Parallel worker processes (default: 1)" }
                }
            }
        },
        {
            "name": "obscura_screenshot",
            "description": "Fetch a page and extract structured data using a JS expression. Convenience wrapper around fetch with eval.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch" },
                    "expression": { "type": "string", "description": "JavaScript expression to extract data" },
                    "wait_until": { "type": "string", "enum": ["load", "domcontentloaded", "networkidle0"], "description": "Wait condition (default: networkidle0)" },
                    "stealth": { "type": "boolean", "description": "Enable anti-detection mode" }
                },
                "required": ["url", "expression"]
            }
        },
        {
            "name": "obscura_extract_markdown",
            "description": "Fetch a URL and convert its content to clean text/markdown. Strips scripts, styles, and navigation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch" },
                    "stealth": { "type": "boolean", "description": "Enable anti-detection mode" },
                    "selector": { "type": "string", "description": "CSS selector for content area" }
                },
                "required": ["url"]
            }
        }
    ])
}

// ── Tool execution ───────────────────────────────────────────────────────

fn obscura_bin() -> String {
    std::env::var("OBSCURA_BIN").unwrap_or_else(|_| "obscura".into())
}

pub fn text_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

pub fn error_result(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
}

fn run_obscura(args: &[&str], timeout_ms: u64) -> Result<String, String> {
    let mut cmd = Command::new(obscura_bin());
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    // Create a new process group so we can kill obscura + its chromium children together
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute obscura: {e}"))?;

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let exit_status;

    loop {
        match child.try_wait() {
            Ok(status @ Some(_)) => {
                exit_status = status;
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let pgid = child.id();
                    // Kill entire process group (obscura + chromium)
                    #[cfg(unix)]
                    {
                        let _ = Command::new("kill")
                            .args(["-9", &format!("-{pgid}")])
                            .output();
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = child.kill();
                    }
                    let _ = child.wait();
                    return Err(format!("obscura timed out after {}ms", timeout_ms));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("Process error: {e}")),
        }
    }

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout_buf);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut stderr_buf);
    }

    let stdout = String::from_utf8_lossy(&stdout_buf);
    let stderr = String::from_utf8_lossy(&stderr_buf);
    let status = exit_status.unwrap(); // guaranteed set by loop

    if !status.success() && stdout.is_empty() {
        Err(stderr.into())
    } else {
        Ok(if stdout.is_empty() {
            stderr.into()
        } else {
            stdout.into()
        })
    }
}

pub fn call_tool(name: &str, args: &Value) -> Value {
    let get_str =
        |key: &str| -> String { args.get(key).and_then(|v| v.as_str()).unwrap_or("").into() };
    let get_bool = |key: &str| -> bool { args.get(key).and_then(|v| v.as_bool()).unwrap_or(false) };
    let get_num = |key: &str| -> Option<u64> { args.get(key).and_then(|v| v.as_u64()) };

    match name {
        "obscura_fetch" => {
            let url = get_str("url");
            if url.is_empty() {
                return error_result("Missing required parameter: url");
            }

            let mut cmd: Vec<&str> = vec!["fetch", &url];
            let dump = get_str("dump");
            let eval_expr = get_str("eval");
            let wait = get_str("wait_until");
            let selector = get_str("selector");
            let ua = get_str("user_agent");

            if !dump.is_empty() {
                cmd.push("--dump");
                cmd.push(&dump);
            }
            if !eval_expr.is_empty() {
                cmd.push("--eval");
                cmd.push(&eval_expr);
            }
            if !wait.is_empty() {
                cmd.push("--wait-until");
                cmd.push(&wait);
            }
            if !selector.is_empty() {
                cmd.push("--selector");
                cmd.push(&selector);
            }
            if get_bool("stealth") {
                cmd.push("--stealth");
            }
            if !ua.is_empty() {
                cmd.push("--user-agent");
                cmd.push(&ua);
            }
            if args.get("quiet").is_none() || get_bool("quiet") {
                cmd.push("--quiet");
            }

            match run_obscura(&cmd, 30_000) {
                Ok(out) => text_result(&out),
                Err(e) => error_result(&e),
            }
        }

        "obscura_scrape" => {
            let urls = match args.get("urls").and_then(|v| v.as_array()) {
                Some(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>(),
                None => return error_result("Missing required parameter: urls"),
            };
            if urls.is_empty() {
                return error_result("urls array is empty");
            }

            let mut cmd: Vec<String> = vec!["scrape".into()];
            cmd.extend(urls);

            let eval_expr = get_str("eval");
            if !eval_expr.is_empty() {
                cmd.push("--eval".into());
                cmd.push(eval_expr);
            }
            if let Some(n) = get_num("concurrency") {
                cmd.push("--concurrency".into());
                cmd.push(n.to_string());
            }
            let fmt = get_str("format");
            if !fmt.is_empty() {
                cmd.push("--format".into());
                cmd.push(fmt);
            }

            let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
            match run_obscura(&refs, 60_000) {
                Ok(out) => text_result(&out),
                Err(e) => error_result(&e),
            }
        }

        "obscura_serve" => {
            let port = get_num("port").unwrap_or(9222);

            let mut cmd = vec![
                obscura_bin(),
                "serve".into(),
                "--port".into(),
                port.to_string(),
            ];
            if get_bool("stealth") {
                cmd.push("--stealth".into());
            }
            let proxy = get_str("proxy");
            if !proxy.is_empty() {
                cmd.push("--proxy".into());
                cmd.push(proxy);
            }
            if let Some(w) = get_num("workers") {
                cmd.push("--workers".into());
                cmd.push(w.to_string());
            }

            match Command::new(&cmd[0]).args(&cmd[1..]).spawn() {
                Ok(child) => {
                    let _ = child;
                    let workers = get_num("workers").unwrap_or(1);
                    text_result(&serde_json::to_string_pretty(&json!({
                        "status": "started",
                        "port": port,
                        "wsEndpoint": format!("ws://127.0.0.1:{port}/devtools/browser"),
                        "httpEndpoint": format!("http://127.0.0.1:{port}/json/list"),
                        "stealth": get_bool("stealth"),
                        "workers": workers,
                        "note": "CDP server is running in background. Use wsEndpoint with Puppeteer/Playwright to connect."
                    })).unwrap())
                }
                Err(e) => error_result(&format!("Failed to start server: {e}")),
            }
        }

        "obscura_screenshot" => {
            let url = get_str("url");
            let expression = get_str("expression");
            if url.is_empty() || expression.is_empty() {
                return error_result("Missing required parameters: url, expression");
            }
            let wait = get_str("wait_until");
            let wait_val = if wait.is_empty() {
                "networkidle0"
            } else {
                &wait
            };

            let mut cmd: Vec<&str> = vec![
                "fetch",
                &url,
                "--eval",
                &expression,
                "--wait-until",
                wait_val,
            ];
            if get_bool("stealth") {
                cmd.push("--stealth");
            }
            cmd.push("--quiet");

            match run_obscura(&cmd, 30_000) {
                Ok(out) => text_result(&out),
                Err(e) => error_result(&e),
            }
        }

        "obscura_extract_markdown" => {
            let url = get_str("url");
            if url.is_empty() {
                return error_result("Missing required parameter: url");
            }
            let selector = get_str("selector");
            let expr = if selector.is_empty() {
                "document.body?.innerText || ''"
            } else {
                &format!(
                    "document.querySelector('{}')?.innerText || ''",
                    selector.replace('\'', "\\'")
                )
            };

            let mut cmd: Vec<&str> = vec![
                "fetch",
                &url,
                "--eval",
                expr,
                "--wait-until",
                "networkidle0",
            ];
            if get_bool("stealth") {
                cmd.push("--stealth");
            }
            cmd.push("--quiet");

            match run_obscura(&cmd, 30_000) {
                Ok(out) => text_result(&out),
                Err(e) => error_result(&e),
            }
        }

        _ => error_result(&format!("Unknown tool: {name}")),
    }
}

// ── Request handling (shared by both transports) ──────────────────────────

pub fn handle_request(req: &Value) -> Option<String> {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|v| v.as_str())?;

    match method {
        "initialize" => Some(make_response(
            &id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "obscura-mcp", "version": "1.2.0" }
            }),
        )),
        "notifications/initialized" => None,
        "tools/list" => Some(make_response(&id, json!({ "tools": tools_list() }))),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            let result = call_tool(tool_name, &arguments);
            Some(make_response(&id, result))
        }
        _ => Some(make_error(
            &id,
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

// ── Stdio transport ──────────────────────────────────────────────────────

pub fn run() {
    eprintln!("obscura-mcp: starting server on stdio");

    let stdin = io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        if stdin
            .lock()
            .read_line(&mut line)
            .ok()
            .is_none_or(|n| n == 0)
        {
            break;
        }
        let Some(req) = parse_request(&line) else {
            continue;
        };
        if let Some(resp) = handle_request(&req) {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(resp.as_bytes());
            let _ = stdout.flush();
        }
    }
}
