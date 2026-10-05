use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::time::Duration;

use crate::{dispatch, BrowserState};

/// Hard cap on a single MCP request body. The client-supplied `Content-Length`
/// is used to pre-size the read buffer; without a ceiling a request advertising
/// e.g. `Content-Length: 4294967296` makes the server allocate and zero-fill
/// that many bytes before reading any body — an unauthenticated OOM/DoS. One
/// MiB is far above any real JSON-RPC tool call.
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;
const MAX_HEADER_LINE_BYTES: usize = 16 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BATCH_ITEMS: usize = 64;
const MAX_ID_BYTES: usize = 1024;
const MAX_CONNECTIONS: usize = 128;
const MAX_PENDING_REQUESTS: usize = 32;

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
    authorized: bool,
    content_type_is_json: bool,
    body: RequestBody,
}

enum RequestRead {
    Closed,
    Invalid,
    Request(HttpRequest),
}

struct PendingRequest {
    body: Vec<u8>,
    reply: oneshot::Sender<Value>,
}

fn token_from_env() -> Result<Option<String>> {
    let token = std::env::var("OBSCURA_MCP_TOKEN")
        .ok()
        .filter(|value| !value.is_empty());
    if token.as_ref().is_some_and(|value| value.len() < 32) {
        anyhow::bail!("OBSCURA_MCP_TOKEN must be at least 32 bytes");
    }
    Ok(token)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn bearer_authorized(header: Option<&str>, expected: Option<&str>) -> bool {
    match expected {
        None => true,
        Some(expected) => header
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|provided| constant_time_eq(provided, expected)),
    }
}

async fn read_line_limited(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len() + consumed > limit {
            anyhow::bail!("HTTP line too long");
        }
        bytes.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if bytes.last() == Some(&b'\n') {
            break;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| anyhow::anyhow!("HTTP head is not UTF-8"))
}

async fn read_request(
    reader: &mut (impl AsyncBufRead + Unpin),
    allowed_origins: Option<&str>,
    auth_token: Option<&str>,
) -> Result<RequestRead> {
    let Some(request_line) = read_line_limited(reader, MAX_REQUEST_LINE_BYTES).await? else {
        return Ok(RequestRead::Closed);
    };
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
    let mut authorization: Option<String> = None;
    let mut content_type_is_json = false;
    let mut header_bytes = 0usize;

    loop {
        let Some(line) = read_line_limited(reader, MAX_HEADER_LINE_BYTES).await? else {
            return Ok(RequestRead::Invalid);
        };
        header_bytes = header_bytes.saturating_add(line.len());
        if header_bytes > MAX_HEADER_BYTES {
            anyhow::bail!("HTTP headers too large");
        }
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
        if lower.starts_with("authorization:") {
            if let Some(idx) = trimmed.find(':') {
                authorization = Some(trimmed[idx + 1..].trim().to_string());
            }
        }
        if let Some(value) = lower.strip_prefix("content-type:") {
            content_type_is_json = value
                .split(';')
                .next()
                .is_some_and(|media_type| media_type.trim() == "application/json");
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
    let authorized = bearer_authorized(authorization.as_deref(), auth_token);
    let body = if method == "POST"
        && path == "/mcp"
        && origin_allowed(origin.as_deref(), allowed_origins)
        && authorized
        && content_type_is_json
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
        authorized,
        content_type_is_json,
        body,
    }))
}

async fn read_request_with_timeout(
    reader: &mut (impl AsyncBufRead + Unpin),
    allowed_origins: Option<&str>,
    auth_token: Option<&str>,
    timeout: Duration,
) -> Result<RequestRead> {
    tokio::time::timeout(timeout, read_request(reader, allowed_origins, auth_token))
        .await
        .map_err(|_| anyhow::anyhow!("request read timed out"))?
}

/// Origin allowlist for browser callers, read from `OBSCURA_MCP_ALLOWED_ORIGINS`
/// (comma-separated). Unset/empty refuses browser callers; native clients do
/// not send Origin and remain unaffected.
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
    match origin {
        None => true,
        Some(o) => match allowlist {
            None => false,
            Some(list) => {
                let o = o.trim();
                list.split(',')
                    .map(str::trim)
                    .any(|a| !a.is_empty() && a.eq_ignore_ascii_case(o))
            }
        },
    }
}

/// CORS response for an already-authorized browser caller. A wildcard is never
/// emitted for this privileged endpoint.
fn cors_header(origin: Option<&str>, allowlist: Option<&str>) -> String {
    match (origin, allowlist) {
        (Some(origin), Some(_)) => {
            format!("Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\n")
        }
        _ => String::new(),
    }
}

/// MCP Streamable HTTP transport (POST /mcp → JSON response).
///
pub async fn run(host: String, port: u16, proxy: Option<String>, user_agent: Option<String>, stealth: bool) -> Result<()> {
    let addr: std::net::SocketAddr = format!("{}:{}", host, port).parse()?;
    let auth_token = token_from_env()?;
    if !addr.ip().is_loopback() && auth_token.is_none() {
        anyhow::bail!(
            "refusing to expose MCP without authentication; set OBSCURA_MCP_TOKEN to at least 32 bytes"
        );
    }
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("MCP HTTP server on http://{}:{}/mcp", host, port);
    if auth_token.is_some() {
        tracing::info!("MCP bearer authentication enabled");
    }

    let mut state = BrowserState::new(proxy, user_agent, stealth);
    let allowed_origins = Arc::new(allowed_origins_env());
    let auth_token = Arc::new(auth_token);
    let connection_slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let (request_tx, mut request_rx) = mpsc::channel::<PendingRequest>(MAX_PENDING_REQUESTS);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let Ok(permit) = connection_slots.clone().try_acquire_owned() else {
                    tracing::warn!("refusing MCP connection: connection limit reached");
                    continue;
                };
                tracing::debug!("MCP HTTP connection from {}", peer);
                let request_tx = request_tx.clone();
                let allowed_origins = allowed_origins.clone();
                let auth_token = auth_token.clone();
                tokio::spawn(async move {
                    let result = handle_connection(
                        stream,
                        request_tx,
                        allowed_origins.as_deref().as_deref(),
                        auth_token.as_deref().as_deref(),
                    ).await;
                    drop(permit);
                    if let Err(error) = result {
                        tracing::debug!("connection closed: {}", error);
                    }
                });
            }
            Some(request) = request_rx.recv() => {
                let response = process_body(&request.body, &mut state).await;
                let _ = request.reply.send(response);
            }
        }
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    request_tx: mpsc::Sender<PendingRequest>,
    allowed_origins: Option<&str>,
    auth_token: Option<&str>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    loop {
        let request = match read_request_with_timeout(
            &mut reader,
            allowed_origins,
            auth_token,
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
            authorized,
            content_type_is_json,
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
        if method != "OPTIONS" && !authorized {
            respond(&mut writer, 401, b"{\"error\":\"authentication required\"}").await?;
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
                    Access-Control-Allow-Headers: Content-Type, Authorization, mcp-protocol-version\r\n\
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
                if !content_type_is_json {
                    respond(
                        &mut writer,
                        415,
                        b"{\"error\":\"Content-Type must be application/json\"}",
                    )
                    .await?;
                    break;
                }
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

                let (reply, response) = oneshot::channel();
                request_tx
                    .send(PendingRequest { body, reply })
                    .await
                    .map_err(|_| anyhow::anyhow!("MCP dispatcher stopped"))?;
                let response = response
                    .await
                    .map_err(|_| anyhow::anyhow!("MCP dispatcher dropped response"))?;
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
        if batch.is_empty() || batch.len() > MAX_BATCH_ITEMS {
            return json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}});
        }
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
    if matches!(id, Value::Array(_) | Value::Object(_))
        || serde_json::to_vec(&id).map_or(true, |encoded| encoded.len() > MAX_ID_BYTES)
    {
        return Some(
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid Request"}}),
        );
    }
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
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
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
        bearer_authorized, cors_header, origin_allowed, read_request_with_timeout, MAX_BODY_BYTES,
        REQUEST_READ_TIMEOUT,
    };
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::time::Duration;

    #[test]
    fn no_allowlist_refuses_browser_callers() {
        assert!(!origin_allowed(Some("https://evil.example"), None));
        assert!(!origin_allowed(Some("null"), None));
        assert!(origin_allowed(None, None));
        assert!(!cors_header(Some("https://evil.example"), None).contains('*'));
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
    fn bearer_token_is_required_when_configured() {
        let token = "01234567890123456789012345678901";
        assert!(bearer_authorized(
            Some(&format!("Bearer {token}")),
            Some(token)
        ));
        assert!(!bearer_authorized(None, Some(token)));
        assert!(!bearer_authorized(Some("Bearer wrong"), Some(token)));
        assert!(bearer_authorized(None, None));
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
        let err = match read_request_with_timeout(&mut reader, None, None, Duration::from_millis(20))
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("stalled request line should time out"),
        };
        assert!(err.to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn stalled_request_body_hits_deadline() {
        let (mut client, server) = tokio::io::duplex(256);
        client
            .write_all(
                b"POST /mcp HTTP/1.1\r\n\
                  Content-Length: 8\r\n\
                  Content-Type: application/json\r\n\
                  \r\n\
                  {}",
            )
            .await
            .unwrap();

        let mut reader = BufReader::new(server);
        let err = match read_request_with_timeout(&mut reader, None, None, Duration::from_millis(20))
            .await
        {
            Err(err) => err,
            Ok(_) => panic!("stalled request body should time out"),
        };
        assert!(err.to_string().contains("timed out"));
    }
}
