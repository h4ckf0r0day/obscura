use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::{dispatch, BrowserState};

/// Hard cap on a single MCP request body. The client-supplied `Content-Length`
/// is used to pre-size the read buffer; without a ceiling a request advertising
/// e.g. `Content-Length: 4294967296` makes the server allocate and zero-fill
/// that many bytes before reading any body — an unauthenticated OOM/DoS. 16 MiB
/// is far above any real JSON-RPC tool call.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Origin allowlist for browser callers, read from `OBSCURA_MCP_ALLOWED_ORIGINS`
/// (comma-separated). Unset/empty → permissive (unchanged `*`) so hosted
/// dashboards keep working (issue #175).
fn allowed_origins_env() -> Option<String> {
    std::env::var("OBSCURA_MCP_ALLOWED_ORIGINS")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Whether a request's `Origin` is permitted. A request with no `Origin`
/// (native, non-browser MCP clients) is always allowed — the same-origin
/// policy only constrains browser callers. When an allowlist is configured, a
/// browser `Origin` must match one of its entries (case-insensitive); this
/// stops a malicious local web page from driving the loopback MCP port.
fn origin_allowed(origin: Option<&str>, allowlist: Option<&str>) -> bool {
    match allowlist {
        None => true,
        Some(list) => match origin {
            None => true,
            Some(o) => {
                let o = o.trim();
                list.split(',')
                    .map(str::trim)
                    .any(|a| !a.is_empty() && a.eq_ignore_ascii_case(o))
            }
        },
    }
}

/// MCP Streamable HTTP transport (POST /mcp → JSON response).
///
/// Connections are handled sequentially on the current thread — the browser
/// session (including the V8 runtime) is single-threaded and `!Send`, so we
/// never need to move state across threads.
pub async fn run(host: String, port: u16, proxy: Option<String>, user_agent: Option<String>, stealth: bool) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{}:{}", host, port).parse()?;
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("MCP HTTP server on http://{}:{}/mcp", host, port);

    let mut state = BrowserState::new(proxy, user_agent, stealth);

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!("MCP HTTP connection from {}", peer);
        if let Err(e) = handle_connection(stream, &mut state).await {
            tracing::debug!("connection closed: {}", e);
        }
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: &mut BrowserState,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    loop {
        // ── request line ─────────────────────────────────────────────────────
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await? == 0 {
            break;
        }
        let request_line = request_line.trim().to_string();
        if request_line.is_empty() {
            break;
        }

        let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
        if parts.len() < 3 {
            break;
        }
        let method = parts[0];
        let path = parts[1];

        // ── headers ──────────────────────────────────────────────────────────
        let mut content_length: Option<usize> = None;
        let mut accept_sse = false;
        let mut keep_alive = false;
        let mut origin: Option<String> = None;

        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            let trimmed = line.trim_end_matches("\r\n").trim_end_matches('\n');
            if trimmed.is_empty() {
                break;
            }
            let lower = trimmed.to_lowercase();
            if let Some(v) = lower.strip_prefix("content-length: ") {
                content_length = v.trim().parse().ok();
            }
            if lower.starts_with("origin:") {
                if let Some(idx) = trimmed.find(':') {
                    origin = Some(trimmed[idx + 1..].trim().to_string());
                }
            }
            if lower.contains("text/event-stream") {
                accept_sse = true;
            }
            if lower.starts_with("connection: ") && lower.contains("keep-alive") {
                keep_alive = true;
            }
        }

        // ── routing ──────────────────────────────────────────────────────────
        if path != "/mcp" {
            respond(&mut writer, 404, b"{\"error\":\"not found\"}").await?;
            break;
        }

        // Origin gate: when OBSCURA_MCP_ALLOWED_ORIGINS is configured, a browser
        // request from a non-listed origin is refused before it can drive the
        // browser session (mitigates a malicious local web page issuing
        // cross-origin POSTs to the loopback MCP port). The permissive default
        // and no-Origin native clients are unaffected.
        if !origin_allowed(origin.as_deref(), allowed_origins_env().as_deref()) {
            respond(&mut writer, 403, b"{\"error\":\"origin not allowed\"}").await?;
            break;
        }

        match method {
            "OPTIONS" => {
                // mcp-protocol-version is part of the MCP spec, Authorization /
                // X-API-Key are common for hosted deployments. Without these
                // listed the browser preflight check fails and blocks the actual
                // request.
                let hdr = "HTTP/1.1 204 No Content\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Content-Type, Authorization, X-API-Key, mcp-protocol-version\r\n\
                    Access-Control-Max-Age: 86400\r\n\
                    \r\n";
                writer.write_all(hdr.as_bytes()).await?;
            }

            "GET" if accept_sse => {
                // SSE stream: hold open and send periodic keep-alive comments
                let hdr = "HTTP/1.1 200 OK\r\n\
                    Content-Type: text/event-stream\r\n\
                    Cache-Control: no-cache\r\n\
                    Connection: keep-alive\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    \r\n";
                writer.write_all(hdr.as_bytes()).await?;
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                    if writer.write_all(b": ping\n\n").await.is_err() {
                        break;
                    }
                    let _ = writer.flush().await;
                }
                break;
            }

            "POST" => {
                let len = match content_length {
                    Some(n) => n,
                    None => {
                        respond(&mut writer, 400, b"{\"error\":\"missing Content-Length\"}").await?;
                        break;
                    }
                };

                // Reject oversized bodies BEFORE allocating: `vec![0u8; len]`
                // commits `len` bytes up front, so an attacker-chosen
                // Content-Length would otherwise OOM the process (DoS).
                if len > MAX_BODY_BYTES {
                    respond(&mut writer, 413, b"{\"error\":\"payload too large\"}").await?;
                    break;
                }

                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).await?;

                let response = process_body(&body, state).await;
                let bytes = serde_json::to_vec(&response)?;
                respond_json(&mut writer, &bytes).await?;

                if !keep_alive {
                    break;
                }
            }

            _ => {
                respond(&mut writer, 405, b"{\"error\":\"method not allowed\"}").await?;
                break;
            }
        }
    }

    Ok(())
}

async fn process_body(body: &[u8], state: &mut BrowserState) -> Value {
    let msg: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
    };

    if let Some(batch) = msg.as_array() {
        let mut results = Vec::new();
        for item in batch {
            if let Some(r) = process_one(item, state).await {
                results.push(r);
            }
        }
        return Value::Array(results);
    }

    process_one(&msg, state).await
        .unwrap_or_else(|| json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}}))
}

async fn process_one(msg: &Value, state: &mut BrowserState) -> Option<Value> {
    let id = msg.get("id").cloned()?; // notifications have no id — return None
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").unwrap_or(&Value::Null);
    let resp = dispatch(method, id, params, state).await;
    Some(serde_json::to_value(resp).unwrap())
}

async fn respond_json(writer: &mut (impl AsyncWriteExt + Unpin), body: &[u8]) -> Result<()> {
    let hdr = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: keep-alive\r\n\
         \r\n",
        body.len()
    );
    writer.write_all(hdr.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

async fn respond(writer: &mut (impl AsyncWriteExt + Unpin), status: u16, body: &[u8]) -> Result<()> {
    let status_text = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "OK",
    };
    let hdr = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         \r\n",
        body.len()
    );
    writer.write_all(hdr.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod mcp_hardening_tests {
    use super::{origin_allowed, MAX_BODY_BYTES};

    #[test]
    fn no_allowlist_is_permissive() {
        assert!(origin_allowed(Some("https://evil.example"), None));
        assert!(origin_allowed(None, None));
    }

    #[test]
    fn allowlist_matches_case_insensitively_and_rejects_others() {
        let list = Some("http://localhost:3000, https://app.example.com");
        assert!(origin_allowed(Some("http://localhost:3000"), list));
        assert!(origin_allowed(Some("https://APP.example.com"), list));
        assert!(!origin_allowed(Some("https://evil.example"), list));
        // A native client (no Origin header) is always allowed.
        assert!(origin_allowed(None, list));
    }

    #[test]
    fn body_cap_is_sane() {
        // Far above a real JSON-RPC tool call, far below an OOM-inducing value.
        assert!(MAX_BODY_BYTES >= 1 << 20);
        assert!(MAX_BODY_BYTES <= 64 << 20);
    }
}
