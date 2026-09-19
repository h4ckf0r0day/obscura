use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::Duration;

use crate::{dispatch, BrowserState};

/// Hard cap on a single MCP request body. The client-supplied `Content-Length`
/// is used to pre-size the read buffer; without a ceiling a request advertising
/// e.g. `Content-Length: 4294967296` makes the server allocate and zero-fill
/// that many bytes before reading any body — an unauthenticated OOM/DoS. 16 MiB
/// is far above any real JSON-RPC tool call.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Maximum time allowed to receive one complete HTTP request (request line,
/// headers, and body). The deadline is shared across all reads so a client
/// cannot keep the sequential MCP server occupied by slowly dribbling data.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);

enum RequestBody {
    NotRead,
    MissingLength,
    TooLarge,
    Read(Vec<u8>),
}

struct HttpRequest {
    method: String,
    path: String,
    accept_sse: bool,
    keep_alive: bool,
    origin: Option<String>,
    body: RequestBody,
}

enum RequestRead {
    Closed,
    Invalid,
    Request(HttpRequest),
}

async fn read_request(
    reader: &mut (impl AsyncBufRead + Unpin),
    allowed_origins: Option<&str>,
) -> Result<RequestRead> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await? == 0 {
        return Ok(RequestRead::Closed);
    }
    let request_line = request_line.trim();
    if request_line.is_empty() {
        return Ok(RequestRead::Closed);
    }

    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() < 3 {
        return Ok(RequestRead::Invalid);
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

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

    // Only consume a body for a POST that can reach the MCP route. Invalid
    // paths and forbidden origins retain the existing early-response behavior.
    let body = if method == "POST"
        && path == "/mcp"
        && origin_allowed(origin.as_deref(), allowed_origins)
    {
        match content_length {
            None => RequestBody::MissingLength,
            // Reject the client-supplied size before allocating the body.
            Some(len) if len > MAX_BODY_BYTES => RequestBody::TooLarge,
            Some(len) => {
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).await?;
                RequestBody::Read(body)
            }
        }
    } else {
        RequestBody::NotRead
    };

    Ok(RequestRead::Request(HttpRequest {
        method,
        path,
        accept_sse,
        keep_alive,
        origin,
        body,
    }))
}

async fn read_request_with_timeout(
    reader: &mut (impl AsyncBufRead + Unpin),
    allowed_origins: Option<&str>,
    timeout: Duration,
) -> Result<RequestRead> {
    tokio::time::timeout(timeout, read_request(reader, allowed_origins))
        .await
        .map_err(|_| anyhow::anyhow!("request read timed out"))?
}

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

/// CORS `Access-Control-Allow-Origin` value for a response. With no allowlist
/// we keep the permissive `*` (issue #175). With an allowlist the request's
/// origin has already passed `origin_allowed`, so echo it back plus `Vary:
/// Origin` instead of advertising `*`; a native client with no `Origin` needs
/// no CORS header at all.
fn cors_header(origin: Option<&str>, allowlist: Option<&str>) -> String {
    match allowlist {
        None => "Access-Control-Allow-Origin: *\r\n".to_string(),
        Some(_) => match origin {
            Some(o) => format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\n"),
            None => String::new(),
        },
    }
}

/// MCP Streamable HTTP transport (POST /mcp → JSON response).
///
/// Connections are handled sequentially on the current thread — the browser
/// session (including the V8 runtime) is single-threaded and `!Send`, so we
/// never need to move state across threads.
pub async fn run(
    host: String,
    port: u16,
    proxy: Option<String>,
    user_agent: Option<String>,
    stealth: bool,
) -> Result<()> {
    run_with_storage(host, port, proxy, user_agent, stealth, None).await
}

pub async fn run_with_storage(
    host: String,
    port: u16,
    proxy: Option<String>,
    user_agent: Option<String>,
    stealth: bool,
    storage_dir: Option<PathBuf>,
) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{}:{}", host, port).parse()?;
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("MCP HTTP server on http://{}:{}/mcp", host, port);

    let mut state = BrowserState::with_storage(proxy, user_agent, stealth, storage_dir);
    let allowed_origins = allowed_origins_env();

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!("MCP HTTP connection from {}", peer);
        if let Err(e) = handle_connection(stream, &mut state, allowed_origins.as_deref()).await {
            tracing::debug!("connection closed: {}", e);
        }
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    state: &mut BrowserState,
    allowed_origins: Option<&str>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    loop {
        let request = match read_request_with_timeout(
            &mut reader,
            allowed_origins,
            REQUEST_READ_TIMEOUT,
        )
        .await?
        {
            RequestRead::Closed | RequestRead::Invalid => break,
            RequestRead::Request(request) => request,
        };
        let HttpRequest {
            method,
            path,
            accept_sse,
            keep_alive,
            origin,
            body,
        } = request;

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
        if !origin_allowed(origin.as_deref(), allowed_origins) {
            respond(&mut writer, 403, b"{\"error\":\"origin not allowed\"}").await?;
            break;
        }
        let cors = cors_header(origin.as_deref(), allowed_origins);

        match method.as_str() {
            "OPTIONS" => {
                // mcp-protocol-version is part of the MCP spec, Authorization /
                // X-API-Key are common for hosted deployments. Without these
                // listed the browser preflight check fails and blocks the actual
                // request.
                let hdr = format!(
                    "HTTP/1.1 204 No Content\r\n\
                    {cors}\
                    Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Content-Type, Authorization, X-API-Key, mcp-protocol-version\r\n\
                    Access-Control-Max-Age: 86400\r\n\
                    \r\n"
                );
                writer.write_all(hdr.as_bytes()).await?;
            }

            "GET" if accept_sse => {
                // SSE stream: hold open and send periodic keep-alive comments.
                // Connections are served sequentially (the browser session is
                // `!Send`), so holding this infinite keep-alive loop inline would
                // never return to the accept loop and would wedge every later
                // request. The keep-alive touches no browser state and the write
                // half is `Send + 'static`, so detach the ping loop onto its own
                // task and return — the accept loop stays free while the stream
                // lives on independently.
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\n\
                    Content-Type: text/event-stream\r\n\
                    Cache-Control: no-cache\r\n\
                    Connection: keep-alive\r\n\
                    {cors}\
                    \r\n"
                );
                writer.write_all(hdr.as_bytes()).await?;
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                        if writer.write_all(b": ping\n\n").await.is_err() {
                            break;
                        }
                        let _ = writer.flush().await;
                    }
                });
                return Ok(());
            }

            "POST" => {
                let body = match body {
                    RequestBody::MissingLength => {
                        respond(&mut writer, 400, b"{\"error\":\"missing Content-Length\"}").await?;
                        break;
                    }
                    RequestBody::TooLarge => {
                        respond(&mut writer, 413, b"{\"error\":\"payload too large\"}").await?;
                        break;
                    }
                    RequestBody::Read(body) => body,
                    RequestBody::NotRead => unreachable!("valid MCP POST body was not read"),
                };

                let response = process_body(&body, state).await;
                let bytes = serde_json::to_vec(&response)?;
                respond_json(&mut writer, &bytes, &cors).await?;

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

async fn respond_json(writer: &mut (impl AsyncWriteExt + Unpin), body: &[u8], cors: &str) -> Result<()> {
    let hdr = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         {cors}\
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
    use super::{
        origin_allowed, read_request_with_timeout, MAX_BODY_BYTES,
        REQUEST_READ_TIMEOUT,
    };
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::time::Duration;

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

    #[test]
    fn request_read_timeout_is_generous_but_bounded() {
        assert!(REQUEST_READ_TIMEOUT >= Duration::from_secs(10));
        assert!(REQUEST_READ_TIMEOUT <= Duration::from_secs(60));
    }

    #[tokio::test]
    async fn stalled_request_line_hits_deadline() {
        let (_client, server) = tokio::io::duplex(64);
        let mut reader = BufReader::new(server);
        let err = match read_request_with_timeout(&mut reader, None, Duration::from_millis(20))
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("stalled request line should time out"),
        };
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn stalled_request_body_hits_deadline() {
        let (mut client, server) = tokio::io::duplex(64);
        client
            .write_all(
                b"POST /mcp HTTP/1.1\r\n\
                  Content-Length: 8\r\n\
                  \r\n\
                  {}",
            )
            .await
            .unwrap();

        let mut reader = BufReader::new(server);
        let err = match read_request_with_timeout(&mut reader, None, Duration::from_millis(20))
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("stalled request body should time out"),
        };
        assert!(err.to_string().contains("timed out"));
    }
}
