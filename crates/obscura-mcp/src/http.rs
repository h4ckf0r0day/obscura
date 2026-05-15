use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::{dispatch, BrowserState};

/// MCP Streamable HTTP transport (POST /mcp → JSON response).
///
/// Connections are handled sequentially on the current thread — the browser
/// session (including the V8 runtime) is single-threaded and `!Send`, so we
/// never need to move state across threads.
pub async fn run(port: u16, proxy: Option<String>, user_agent: Option<String>, stealth: bool) -> Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("MCP HTTP server on http://127.0.0.1:{}/mcp", port);

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

        match method {
            "OPTIONS" => {
                let hdr = "HTTP/1.1 204 No Content\r\n\
                    Access-Control-Allow-Origin: *\r\n\
                    Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                    Access-Control-Allow-Headers: Content-Type\r\n\
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
        404 => "Not Found",
        405 => "Method Not Allowed",
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
