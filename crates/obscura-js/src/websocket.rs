//! Client transport behind the page-facing `WebSocket` interface.
//!
//! bootstrap.js owns the WHATWG state machine (readyState, events,
//! bufferedAmount, argument validation). This module owns the sockets: it
//! dials, performs the opening handshake with the page's cookies and origin,
//! and moves frames between V8 and the network. Sockets live in a per-runtime
//! table keyed by a small integer id, so a runtime teardown drops every
//! connection with it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use deno_core::{op2, JsBuffer, OpState};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use obscura_net::{same_site, SameSiteContext};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::ops::SharedState;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Upper bound for TCP connect plus the opening handshake. A peer that accepts
/// the TCP connection but never answers the upgrade must still surface as a
/// failed connection instead of leaving the page in CONNECTING forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// After the peer's Close frame arrives, keep reading briefly so the echoed
/// Close frame tungstenite queues is flushed before the socket is dropped.
const CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

struct Connection {
    sink: Mutex<SplitSink<Socket, Message>>,
    stream: Mutex<SplitStream<Socket>>,
}

enum Slot {
    /// Handshake in flight. Notified when the page aborts it with `close()`.
    Connecting(Rc<Notify>),
    Open(Rc<Connection>),
}

#[derive(Default)]
struct WebSocketTable {
    next_id: u32,
    slots: HashMap<u32, Slot>,
}

fn with_table<R>(state: &mut OpState, f: impl FnOnce(&mut WebSocketTable) -> R) -> R {
    if !state.has::<WebSocketTable>() {
        state.put(WebSocketTable::default());
    }
    f(state.borrow_mut::<WebSocketTable>())
}

fn connection(state: &Rc<RefCell<OpState>>, id: u32) -> Option<Rc<Connection>> {
    let mut state = state.borrow_mut();
    with_table(&mut state, |table| match table.slots.get(&id) {
        Some(Slot::Open(conn)) => Some(conn.clone()),
        _ => None,
    })
}

fn remove_slot(state: &Rc<RefCell<OpState>>, id: u32) -> Option<Slot> {
    let mut state = state.borrow_mut();
    with_table(&mut state, |table| table.slots.remove(&id))
}

/// Reserve an id for a new socket. The id exists before the handshake starts
/// so `close()` during CONNECTING can abort the dial.
#[op2(fast)]
pub fn op_ws_create(state: &mut OpState) -> u32 {
    with_table(state, |table| {
        table.next_id = table.next_id.wrapping_add(1).max(1);
        let id = table.next_id;
        table
            .slots
            .insert(id, Slot::Connecting(Rc::new(Notify::new())));
        id
    })
}

/// What the handshake needs from the page, captured before the first await so
/// no `RefCell` borrow is held across the network round trip.
struct PageNetworkContext {
    blocked_urls: Vec<String>,
    cookie_jar: Option<Arc<obscura_net::CookieJar>>,
    user_agent: Option<String>,
    allow_private_network: bool,
    proxy_configured: bool,
    page_in_flight: Arc<AtomicU32>,
}

fn page_network_context(state: &Rc<RefCell<OpState>>) -> PageNetworkContext {
    let state = state.borrow();
    let shared = state.borrow::<SharedState>().clone();
    let page = shared.borrow();
    let client = page.http_client.as_ref();
    PageNetworkContext {
        blocked_urls: page.blocked_urls.clone(),
        cookie_jar: page.cookie_jar.clone(),
        user_agent: client.and_then(|c| c.user_agent.try_read().ok().map(|ua| ua.clone())),
        allow_private_network: client.is_some_and(|c| c.allow_private_network)
            || obscura_net::env_allows_private_network(),
        proxy_configured: client.is_some_and(|c| c.proxy_url().is_some()),
        page_in_flight: Arc::clone(&page.page_in_flight),
    }
}

/// The http(s) URL a ws(s) URL shares cookies, SSRF policy and site with.
fn http_equivalent(url: &url::Url) -> Option<url::Url> {
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return None,
    };
    let mut http = url.clone();
    http.set_scheme(scheme).ok()?;
    Some(http)
}

fn is_blocked(patterns: &[String], url: &str) -> bool {
    patterns.iter().any(|pattern| {
        pattern == "*" || url.contains(pattern.as_str()) || crate::ops::glob_match(pattern, url)
    })
}

/// Resolve the target and apply the same private-network policy scripted
/// fetch() uses. Checking resolved addresses (not only the host string) keeps
/// a public name that resolves to 127.0.0.1 or a metadata address out.
async fn resolve_allowed(url: &url::Url, allow_private: bool) -> Result<Vec<SocketAddr>, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "WebSocket URL has no host".to_string())?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "WebSocket URL has no port".to_string())?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("DNS resolution failed for {host}: {e}"))?
        .collect();
    let allowed: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|addr| allow_private || !obscura_net::is_forbidden_ip(addr.ip()))
        .collect();
    if allowed.is_empty() {
        return Err(format!(
            "Access to private/internal address for {host} is not allowed"
        ));
    }
    Ok(allowed)
}

async fn dial(addrs: &[SocketAddr]) -> Result<TcpStream, String> {
    let mut last_error = String::from("no address to connect to");
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last_error = format!("connect to {addr} failed: {e}"),
        }
    }
    Err(last_error)
}

struct Handshake {
    socket: Socket,
    protocol: String,
}

async fn handshake(
    url: &url::Url,
    http_url: &url::Url,
    protocols: &str,
    origin: &str,
    ctx: &PageNetworkContext,
) -> Result<Handshake, String> {
    let addrs = resolve_allowed(url, ctx.allow_private_network).await?;
    let tcp = dial(&addrs).await?;

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("invalid WebSocket request: {e}"))?;
    let headers = request.headers_mut();
    let mut set = |name: &'static str, value: &str| {
        if value.is_empty() {
            return;
        }
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    };
    set("Origin", origin);
    set("Sec-WebSocket-Protocol", protocols);
    if let Some(ua) = &ctx.user_agent {
        set("User-Agent", ua);
    }
    if let Some(jar) = &ctx.cookie_jar {
        let context = match url::Url::parse(origin) {
            Ok(initiator) if same_site(&initiator, http_url) => SameSiteContext::SameSite,
            Ok(_) => SameSiteContext::CrossSite,
            // An opaque origin (about:blank, data:) has no site to share.
            Err(_) => SameSiteContext::CrossSite,
        };
        set(
            "Cookie",
            &jar.get_cookie_header_in_context(http_url, context),
        );
    }

    let (socket, response) =
        tokio_tungstenite::client_async_tls_with_config(request, tcp, None, None)
            .await
            .map_err(|e| format!("WebSocket handshake failed: {e}"))?;

    if let Some(jar) = &ctx.cookie_jar {
        for value in response.headers().get_all("set-cookie") {
            if let Ok(value) = value.to_str() {
                jar.set_cookie(value, http_url);
            }
        }
    }
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    Ok(Handshake { socket, protocol })
}

fn failure(error: impl Into<String>) -> String {
    serde_json::json!({ "ok": false, "error": error.into() }).to_string()
}

/// Dial `url` and run the opening handshake. Resolves to JSON:
/// `{"ok":true,"protocol":..,"extensions":..}` or `{"ok":false,"error":..}`.
/// A failure is a normal result, not an exception: the page reports it as an
/// `error` event followed by `close` with code 1006.
#[op2]
#[string]
pub async fn op_ws_connect(
    state: Rc<RefCell<OpState>>,
    id: u32,
    #[string] url: String,
    #[string] protocols: String,
    #[string] origin: String,
) -> String {
    let cancel = {
        let mut s = state.borrow_mut();
        with_table(&mut s, |table| match table.slots.get(&id) {
            Some(Slot::Connecting(cancel)) => Some(cancel.clone()),
            _ => None,
        })
    };
    let Some(cancel) = cancel else {
        return failure("WebSocket was closed before connecting");
    };

    let parsed = match url::Url::parse(&url) {
        Ok(parsed) => parsed,
        Err(e) => {
            remove_slot(&state, id);
            return failure(format!("invalid WebSocket URL: {e}"));
        }
    };
    let Some(http_url) = http_equivalent(&parsed) else {
        remove_slot(&state, id);
        return failure("WebSocket URL scheme must be ws or wss");
    };
    let ctx = page_network_context(&state);
    if is_blocked(&ctx.blocked_urls, &url) {
        remove_slot(&state, id);
        return failure("WebSocket URL is blocked");
    }
    // Proxy tunnelling (HTTP CONNECT / SOCKS) is not implemented. Failing closed
    // keeps a proxied context from leaking its real address over a socket.
    if ctx.proxy_configured {
        remove_slot(&state, id);
        return failure("WebSocket connections through a proxy are not supported");
    }

    // Count the handshake as page network activity so load/settle waits for
    // the socket to open the way it waits for an in-flight fetch().
    struct InFlight(Arc<AtomicU32>);
    impl Drop for InFlight {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    ctx.page_in_flight.fetch_add(1, Ordering::Relaxed);
    let _in_flight = InFlight(Arc::clone(&ctx.page_in_flight));

    let outcome = tokio::select! {
        result = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            handshake(&parsed, &http_url, &protocols, &origin, &ctx),
        ) => match result {
            Ok(result) => result,
            Err(_) => Err("WebSocket opening handshake timed out".to_string()),
        },
        _ = cancel.notified() => Err("WebSocket was closed before connecting".to_string()),
    };

    let handshake = match outcome {
        Ok(handshake) => handshake,
        Err(error) => {
            remove_slot(&state, id);
            tracing::debug!("WebSocket {url}: {error}");
            return failure(error);
        }
    };

    let (sink, stream) = handshake.socket.split();
    let conn = Rc::new(Connection {
        sink: Mutex::new(sink),
        stream: Mutex::new(stream),
    });
    let still_wanted = {
        let mut s = state.borrow_mut();
        with_table(&mut s, |table| match table.slots.get_mut(&id) {
            Some(slot @ Slot::Connecting(_)) => {
                *slot = Slot::Open(conn);
                true
            }
            _ => false,
        })
    };
    if !still_wanted {
        return failure("WebSocket was closed before connecting");
    }
    serde_json::json!({
        "ok": true,
        "protocol": handshake.protocol,
        // No extension is negotiated: permessage-deflate is not offered.
        "extensions": "",
    })
    .to_string()
}

/// Send one message. Text arrives as UTF-8 bytes (the page encodes with
/// TextEncoder, which already replaced lone surrogates). Resolves to false
/// when the socket is gone or the write failed; the read side reports the
/// resulting closure.
#[op2]
pub async fn op_ws_send(
    state: Rc<RefCell<OpState>>,
    id: u32,
    #[buffer] data: JsBuffer,
    is_text: bool,
) -> bool {
    let Some(conn) = connection(&state, id) else {
        return false;
    };
    let message = if is_text {
        match String::from_utf8(data.to_vec()) {
            Ok(text) => Message::text(text),
            Err(_) => return false,
        }
    } else {
        Message::binary(data.to_vec())
    };
    let mut sink = conn.sink.lock().await;
    sink.send(message).await.is_ok()
}

/// Start the closing handshake, or abort a handshake still in flight.
/// `code == 0` sends a Close frame without a status code.
#[op2]
pub async fn op_ws_close(
    state: Rc<RefCell<OpState>>,
    id: u32,
    code: u32,
    #[string] reason: String,
) {
    let conn = {
        let mut s = state.borrow_mut();
        with_table(&mut s, |table| match table.slots.get(&id) {
            Some(Slot::Connecting(cancel)) => {
                cancel.notify_one();
                table.slots.remove(&id);
                None
            }
            Some(Slot::Open(conn)) => Some(conn.clone()),
            None => None,
        })
    };
    let Some(conn) = conn else {
        return;
    };
    let frame = (code != 0).then(|| CloseFrame {
        code: (code as u16).into(),
        reason: reason.into(),
    });
    let mut sink = conn.sink.lock().await;
    let _ = sink.send(Message::Close(frame)).await;
}

/// Wait for the next event on an open socket. Resolves to JSON:
/// `{"type":"text","data":..}`, `{"type":"binary","data":<base64>}`, or
/// `{"type":"close","code":..,"reason":..,"clean":bool}` as the final event.
#[op2]
#[string]
pub async fn op_ws_next(state: Rc<RefCell<OpState>>, id: u32) -> String {
    let Some(conn) = connection(&state, id) else {
        return close_event(1006, "", false);
    };
    let mut stream = conn.stream.lock().await;
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::json!({ "type": "text", "data": text.as_str() }).to_string();
            }
            Some(Ok(Message::Binary(bytes))) => {
                return serde_json::json!({ "type": "binary", "data": BASE64.encode(&bytes) })
                    .to_string();
            }
            Some(Ok(Message::Close(frame))) => {
                // Reading on flushes the Close echo tungstenite queued.
                let _ = tokio::time::timeout(CLOSE_DRAIN_TIMEOUT, async {
                    while stream.next().await.is_some() {}
                })
                .await;
                remove_slot(&state, id);
                let (code, reason) = match frame {
                    Some(frame) => (u16::from(frame.code), frame.reason.to_string()),
                    // RFC 6455 §7.1.5: no status code present.
                    None => (1005, String::new()),
                };
                return close_event(code, &reason, true);
            }
            // Ping/Pong are answered by tungstenite; raw frames never surface
            // from a client stream.
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => {
                remove_slot(&state, id);
                return close_event(1006, "", false);
            }
        }
    }
}

fn close_event(code: u16, reason: &str, clean: bool) -> String {
    serde_json::json!({ "type": "close", "code": code, "reason": reason, "clean": clean })
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_urls_map_to_their_http_equivalent() {
        let ws = url::Url::parse("ws://example.com:8080/chat?x=1").unwrap();
        assert_eq!(
            http_equivalent(&ws).unwrap().as_str(),
            "http://example.com:8080/chat?x=1"
        );
        let wss = url::Url::parse("wss://example.com/").unwrap();
        assert_eq!(
            http_equivalent(&wss).unwrap().as_str(),
            "https://example.com/"
        );
        let http = url::Url::parse("http://example.com/").unwrap();
        assert!(http_equivalent(&http).is_none());
    }

    #[tokio::test]
    async fn loopback_is_refused_without_private_network_opt_in() {
        let url = url::Url::parse("ws://127.0.0.1:9/").unwrap();
        assert!(resolve_allowed(&url, false).await.is_err());
        assert!(resolve_allowed(&url, true).await.is_ok());
    }
}
