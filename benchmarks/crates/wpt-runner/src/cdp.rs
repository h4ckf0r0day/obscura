//! Minimal Chrome DevTools Protocol client over a single WebSocket connection.
//!
//! Each connection is used for one request at a time, so response matching is
//! just "send a command with id N, then read frames until a frame with id N
//! arrives" while ignoring protocol events and stray frames. Obscura derives
//! the CDP session id deterministically as `{targetId}-session`, so we never
//! call Target.attachToTarget.
//!
//! Protocol events seen while waiting for a response are buffered so the bug
//! catcher can drain console messages and uncaught exceptions after a test.

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

pub struct Cdp {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: i64,
    /// Protocol events (frames carrying a `method` and no matching id) buffered
    /// since the last `take_events()`.
    events: Vec<Value>,
}

impl Cdp {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let (ws, _resp) = connect_async(ws_url)
            .await
            .with_context(|| format!("websocket connect to {ws_url}"))?;
        Ok(Self {
            ws,
            next_id: 0,
            events: Vec::new(),
        })
    }

    /// Browser-level command (no session).
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.call_inner(method, params, None).await
    }

    /// Session-scoped command. `session` is `{targetId}-session`.
    pub async fn call_session(&mut self, session: &str, method: &str, params: Value) -> Result<Value> {
        self.call_inner(method, params, Some(session)).await
    }

    /// Drain and return protocol events buffered since the last call.
    pub fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.events)
    }

    /// Enable the domains the bug catcher needs (console, exceptions, log).
    /// Errors are ignored: some builds may not implement every domain.
    pub async fn enable_capture(&mut self, session: &str) -> Result<()> {
        let _ = self.call_session(session, "Runtime.enable", json!({})).await;
        let _ = self.call_session(session, "Log.enable", json!({})).await;
        let _ = self.call_session(session, "Page.enable", json!({})).await;
        Ok(())
    }

    async fn call_inner(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::Text(msg.to_string())).await?;

        loop {
            let frame = self
                .ws
                .next()
                .await
                .ok_or_else(|| anyhow!("websocket closed while waiting for `{method}`"))??;
            let text = match frame {
                Message::Text(t) => t,
                Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                Message::Close(_) => return Err(anyhow!("websocket closed during `{method}`")),
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
            };
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(anyhow!("CDP error for `{method}`: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // A protocol event or an unrelated id. Buffer real events so the
            // caller can inspect console output / exceptions after the test.
            if v.get("method").is_some() {
                self.events.push(v);
            }
        }
    }
}

/// Query `/json/version` over plain HTTP to discover the browser WebSocket URL.
/// Obscura serves this on the CDP port; we fall back to the well-known endpoint
/// if the field is missing.
pub async fn discover_ws_url(host: &str, port: u16) -> Result<String> {
    let mut stream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connect to {host}:{port}"))?;
    let req = format!(
        "GET /json/version HTTP/1.0\r\nHost: {host}:{port}\r\nAccept: application/json\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim();
    let v: Value = serde_json::from_str(body).context("parse /json/version body")?;
    match v.get("webSocketDebuggerUrl").and_then(Value::as_str) {
        Some(u) => Ok(u.to_string()),
        None => Ok(format!("ws://{host}:{port}/devtools/browser")),
    }
}
