use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::{dispatch, BrowserState};

/// Cap on a single MCP request body (MCP-02). Without it, an attacker-supplied
/// `Content-Length` makes the server `vec![0u8; len]` up to `usize::MAX`,
/// instantly OOM-killing the process. 16 MiB is far above any real JSON-RPC call.
const MAX_MCP_BODY: usize = 16 * 1024 * 1024;

/// DNS-rebinding defense (MCP-03): the Host header must name loopback, the bound
/// host, or an IP literal. A rebinding attack points an attacker *domain* at
/// 127.0.0.1, so rejecting domain-name Hosts (other than the bind host) blunts
/// it. Empty Host is allowed (HTTP/1.0 / native clients).
fn header_host_is_safe(host_header: &str, bind_host: &str) -> bool {
    let h = host_header.trim();
    let hostname = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest) // [::1]:port
    } else {
        h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h)
    };
    hostname.eq_ignore_ascii_case("localhost")
        || hostname.eq_ignore_ascii_case(bind_host)
        || hostname.parse::<std::net::IpAddr>().is_ok()
}

/// Cross-origin defense (MCP-03): a browser page ALWAYS sends an Origin header,
/// so any Origin that is not explicitly allowlisted is rejected — this is the
/// A1-page → 127.0.0.1 pivot the threat model calls out. Native clients (curl,
/// Claude Desktop) send no Origin and are allowed. Allowlist is opt-in via
/// `OBSCURA_MCP_ALLOWED_ORIGINS` (comma-separated).
fn origin_allowed(origin: &str) -> bool {
    if origin.is_empty() {
        return true;
    }
    match std::env::var("OBSCURA_MCP_ALLOWED_ORIGINS") {
        Ok(list) => list
            .split(',')
            .map(str::trim)
            .any(|a| !a.is_empty() && a.eq_ignore_ascii_case(origin)),
        Err(_) => false,
    }
}

/// MCP Streamable HTTP transport (POST /mcp → JSON response).
///
/// Connections are handled sequentially on the current thread — the browser
/// session (including the V8 runtime) is single-threaded and `!Send`, so we
/// never need to move state across threads.
pub async fn run(host: String, port: u16, proxy: Option<String>, user_agent: Option<String>, stealth: bool, allow_file_access: bool) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{}:{}", host, port).parse()?;
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("MCP HTTP server on http://{}:{}/mcp", host, port);

    let mut state = BrowserState::new(proxy, user_agent, stealth, allow_file_access);

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!("MCP HTTP connection from {}", peer);
        if let Err(e) = handle_connection(stream, &mut state, &host).await {
            tracing::debug!("connection closed: {}", e);
        }
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: &mut BrowserState,
    bind_host: &str,
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
        let mut origin = String::new();
        let mut host_hdr = String::new();

        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            let trimmed = line.trim_end_matches("\r\n").trim_end_matches('\n');
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                let value = value.trim();
                match name.trim().to_ascii_lowercase().as_str() {
                    "content-length" => content_length = value.parse().ok(),
                    "origin" => origin = value.to_string(),
                    "host" => host_hdr = value.to_string(),
                    "accept" if value.to_ascii_lowercase().contains("text/event-stream") => {
                        accept_sse = true;
                    }
                    "connection" if value.to_ascii_lowercase().contains("keep-alive") => {
                        keep_alive = true;
                    }
                    _ => {}
                }
            }
        }

        // ── routing ──────────────────────────────────────────────────────────
        if path != "/mcp" {
            respond(&mut writer, 404, b"{\"error\":\"not found\"}").await?;
            break;
        }

        // ── security gates (MCP-03) ────────────────────────────────────────────
        // DNS-rebinding: reject a Host that names an attacker domain.
        if !host_hdr.is_empty() && !header_host_is_safe(&host_hdr, bind_host) {
            respond(&mut writer, 403, b"{\"error\":\"forbidden host\"}").await?;
            break;
        }
        // Cross-origin: reject an actual request (POST / SSE GET) from a browser
        // page whose Origin is not allowlisted. The preflight (OPTIONS) is not
        // hard-rejected — it simply receives no Access-Control-Allow-Origin, so
        // the browser blocks the real request itself.
        let origin_ok = origin_allowed(&origin);
        if method != "OPTIONS" && !origin_ok {
            respond(&mut writer, 403, b"{\"error\":\"forbidden origin\"}").await?;
            break;
        }
        // The only value we echo as Access-Control-Allow-Origin: the validated
        // request Origin, never `*` (empty when the Origin is absent or denied).
        let acao = if origin_ok && !origin.is_empty() { origin.clone() } else { String::new() };

        match method {
            "OPTIONS" => {
                // mcp-protocol-version is part of the MCP spec, Authorization /
                // X-API-Key are common for hosted deployments. Without these
                // listed the browser preflight check fails and blocks the actual
                // request. ACAO is echoed only for an allowlisted Origin (never
                // `*`); an unlisted Origin gets no ACAO and the browser blocks.
                let acao_line = if acao.is_empty() {
                    String::new()
                } else {
                    format!("Access-Control-Allow-Origin: {acao}\r\n")
                };
                let hdr = format!(
                    "HTTP/1.1 204 No Content\r\n\
                    {acao_line}\
                    Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Content-Type, Authorization, X-API-Key, mcp-protocol-version\r\n\
                    Access-Control-Max-Age: 86400\r\n\
                    \r\n"
                );
                writer.write_all(hdr.as_bytes()).await?;
            }

            "GET" if accept_sse => {
                // SSE stream: hold open and send periodic keep-alive comments
                let acao_line = if acao.is_empty() {
                    String::new()
                } else {
                    format!("Access-Control-Allow-Origin: {acao}\r\n")
                };
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\n\
                    Content-Type: text/event-stream\r\n\
                    Cache-Control: no-cache\r\n\
                    Connection: keep-alive\r\n\
                    {acao_line}\
                    \r\n"
                );
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
                    // MCP-02: cap the body so a forged Content-Length can't
                    // allocate an arbitrarily large Vec and OOM the process.
                    Some(n) if n <= MAX_MCP_BODY => n,
                    Some(_) => {
                        respond(&mut writer, 413, b"{\"error\":\"payload too large\"}").await?;
                        break;
                    }
                    None => {
                        respond(&mut writer, 400, b"{\"error\":\"missing Content-Length\"}").await?;
                        break;
                    }
                };

                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).await?;

                let response = process_body(&body, state).await;
                let bytes = serde_json::to_vec(&response)?;
                respond_json(&mut writer, &bytes, &acao).await?;

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

async fn respond_json(writer: &mut (impl AsyncWriteExt + Unpin), body: &[u8], acao: &str) -> Result<()> {
    let acao_line = if acao.is_empty() {
        String::new()
    } else {
        format!("Access-Control-Allow-Origin: {acao}\r\n")
    };
    let hdr = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         {acao_line}\
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
