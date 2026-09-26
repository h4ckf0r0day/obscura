//! Ops behind the page `WebSocket` class. Each socket lives in
//! [`WsRegistry`] on the op state; dropping the registry (the runtime going
//! away) drops every command channel, which closes the sockets.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use deno_core::{op2, OpState};
use obscura_net::websocket::{WsCommand, WsConnectOptions, WsEvent};
use tokio::sync::{mpsc, Mutex};

use crate::ops::SharedState;

struct WsConn {
    request_id: String,
    commands: mpsc::UnboundedSender<WsCommand>,
    events: Rc<Mutex<mpsc::UnboundedReceiver<WsEvent>>>,
}

#[derive(Default)]
pub(crate) struct WsRegistry {
    next_id: u32,
    next_request: u64,
    conns: HashMap<u32, WsConn>,
}

const MAX_WS_CDP_EVENTS: usize = 4096;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Queue a CDP `Network.webSocket*` event for the page.
fn record(state: &OpState, method: &str, params: serde_json::Value) {
    let gs = state.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();
    gs.ws_cdp_events.push((method.to_string(), params));
    if gs.ws_cdp_events.len() > MAX_WS_CDP_EVENTS {
        let overflow = gs.ws_cdp_events.len() - MAX_WS_CDP_EVENTS;
        gs.ws_cdp_events.drain(0..overflow);
    }
}

fn frame_event(state: &OpState, method: &str, request_id: &str, opcode: u8, mask: bool, payload: String) {
    record(
        state,
        method,
        serde_json::json!({
            "requestId": request_id,
            "timestamp": now(),
            "response": {"opcode": opcode, "mask": mask, "payloadData": payload},
        }),
    );
}

fn registry(state: &mut OpState) -> &mut WsRegistry {
    if !state.has::<WsRegistry>() {
        state.put(WsRegistry::default());
    }
    state.borrow_mut::<WsRegistry>()
}

fn js_error(msg: impl Into<String>) -> deno_error::JsErrorBox {
    deno_error::JsErrorBox::generic(msg.into())
}

/// Connect and register a socket. Resolves to
/// `{"id", "protocol", "extensions"}`; rejects when the connection or the
/// handshake fails, which the page sees as `error` then `close` (1006).
#[op2]
#[string]
pub(crate) async fn op_ws_open(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[string] protocols_json: String,
    #[string] origin: String,
) -> Result<String, deno_error::JsErrorBox> {
    let url = url::Url::parse(&url).map_err(|e| js_error(e.to_string()))?;
    let http_url = obscura_net::websocket::http_equivalent(&url)
        .ok_or_else(|| js_error("WebSocket URL must use ws: or wss:"))?;
    let protocols: Vec<String> = serde_json::from_str(&protocols_json).unwrap_or_default();

    let (jar, http_client, blocked) = {
        let state = state.borrow();
        let gs = state.borrow::<SharedState>().clone();
        let gs = gs.borrow();
        #[cfg(feature = "stealth")]
        let stealth = gs.stealth_client.clone();
        #[cfg(not(feature = "stealth"))]
        let stealth: Option<()> = None;
        let blocked = gs
            .blocked_urls
            .iter()
            .any(|p| p == "*" || url.as_str().contains(p.as_str()));
        (gs.cookie_jar.clone(), gs.http_client.clone(), (blocked, stealth))
    };
    let (blocked, stealth) = blocked;
    if blocked {
        return Err(js_error("WebSocket URL blocked by request policy"));
    }

    let (user_agent, proxy, allow_private_network) = match &http_client {
        Some(client) => (
            client.user_agent.read().await.clone(),
            client.proxy_url().map(str::to_string),
            client.allow_private_network,
        ),
        None => (String::new(), None, false),
    };
    let cookie_header = jar
        .as_ref()
        .map(|j| j.get_cookie_header(&http_url))
        .unwrap_or_default();

    let request_id = {
        let mut state = state.borrow_mut();
        let reg = registry(&mut state);
        reg.next_request += 1;
        let request_id = format!("ws-{}", reg.next_request);
        record(
            &state,
            "Network.webSocketCreated",
            serde_json::json!({"requestId": request_id, "url": url.as_str(), "initiator": {"type": "script"}}),
        );
        let mut headers = serde_json::Map::new();
        if !user_agent.is_empty() {
            headers.insert("User-Agent".into(), user_agent.clone().into());
        }
        if !origin.is_empty() && origin != "null" {
            headers.insert("Origin".into(), origin.clone().into());
        }
        if !cookie_header.is_empty() {
            headers.insert("Cookie".into(), cookie_header.clone().into());
        }
        if !protocols.is_empty() {
            headers.insert("Sec-WebSocket-Protocol".into(), protocols.join(", ").into());
        }
        record(
            &state,
            "Network.webSocketWillSendHandshakeRequest",
            serde_json::json!({
                "requestId": request_id,
                "timestamp": now(),
                "wallTime": now(),
                "request": {"headers": headers},
            }),
        );
        request_id
    };

    let opts = WsConnectOptions {
        url: url.clone(),
        protocols,
        origin,
        user_agent,
        cookie_header,
        proxy,
        allow_private_network,
    };
    #[cfg(feature = "stealth")]
    let handle = match stealth {
        Some(client) => client.connect_websocket(opts).await,
        None => obscura_net::websocket::connect(opts).await,
    };
    #[cfg(not(feature = "stealth"))]
    let handle = {
        let _ = stealth;
        obscura_net::websocket::connect(opts).await
    };
    let handle = match handle {
        Ok(handle) => handle,
        Err(message) => {
            let state = state.borrow();
            record(
                &state,
                "Network.webSocketFrameError",
                serde_json::json!({"requestId": request_id, "timestamp": now(), "errorMessage": message}),
            );
            record(&state, "Network.webSocketClosed", serde_json::json!({"requestId": request_id, "timestamp": now()}));
            return Err(js_error(message));
        }
    };

    if let Some(jar) = &jar {
        for set_cookie in &handle.set_cookies {
            jar.set_cookie(set_cookie, &http_url);
        }
    }

    let mut state = state.borrow_mut();
    let mut response_headers = serde_json::Map::new();
    response_headers.insert("Upgrade".into(), "websocket".into());
    response_headers.insert("Connection".into(), "Upgrade".into());
    if !handle.protocol.is_empty() {
        response_headers.insert("Sec-WebSocket-Protocol".into(), handle.protocol.clone().into());
    }
    if !handle.extensions.is_empty() {
        response_headers.insert("Sec-WebSocket-Extensions".into(), handle.extensions.clone().into());
    }
    record(
        &state,
        "Network.webSocketHandshakeResponseReceived",
        serde_json::json!({
            "requestId": request_id,
            "timestamp": now(),
            "response": {"status": 101, "statusText": "Switching Protocols", "headers": response_headers},
        }),
    );
    let reg = registry(&mut state);
    reg.next_id = reg.next_id.wrapping_add(1).max(1);
    let id = reg.next_id;
    reg.conns.insert(
        id,
        WsConn {
            request_id,
            commands: handle.commands,
            events: Rc::new(Mutex::new(handle.events)),
        },
    );
    Ok(serde_json::json!({
        "id": id,
        "protocol": handle.protocol,
        "extensions": handle.extensions,
    })
    .to_string())
}

#[op2(fast)]
pub(crate) fn op_ws_send_text(state: &mut OpState, id: u32, #[string] data: &str) -> bool {
    let Some(conn) = registry(state).conns.get(&id) else {
        return false;
    };
    let request_id = conn.request_id.clone();
    let sent = conn.commands.send(WsCommand::Text(data.to_string())).is_ok();
    if sent {
        frame_event(state, "Network.webSocketFrameSent", &request_id, 1, true, data.to_string());
    }
    sent
}

#[op2(fast)]
pub(crate) fn op_ws_send_binary(state: &mut OpState, id: u32, #[buffer] data: &[u8]) -> bool {
    let Some(conn) = registry(state).conns.get(&id) else {
        return false;
    };
    let request_id = conn.request_id.clone();
    let sent = conn.commands.send(WsCommand::Binary(data.to_vec())).is_ok();
    if sent {
        frame_event(state, "Network.webSocketFrameSent", &request_id, 2, true, BASE64.encode(data));
    }
    sent
}

#[op2(fast)]
pub(crate) fn op_ws_close(state: &mut OpState, id: u32, code: u32, #[string] reason: &str) {
    if let Some(conn) = registry(state).conns.get(&id) {
        let _ = conn.commands.send(WsCommand::Close {
            code: code as u16,
            reason: reason.to_string(),
        });
    }
}

/// Next event for a socket: `{"t":"text","d"}`, `{"t":"binary","d":base64}`
/// or `{"t":"close","code","reason","clean"}`. The close event also removes
/// the socket from the registry.
#[op2]
#[string]
pub(crate) async fn op_ws_recv(state: Rc<RefCell<OpState>>, id: u32) -> String {
    let (events, request_id) = {
        let mut state = state.borrow_mut();
        match registry(&mut state).conns.get(&id) {
            Some(conn) => (conn.events.clone(), conn.request_id.clone()),
            None => {
                return serde_json::json!({"t": "close", "code": 1006, "reason": "", "clean": false})
                    .to_string()
            }
        }
    };
    let event = events.lock().await.recv().await;
    let json = match event {
        Some(WsEvent::Text(d)) => {
            frame_event(&state.borrow(), "Network.webSocketFrameReceived", &request_id, 1, false, d.clone());
            serde_json::json!({"t": "text", "d": d})
        }
        Some(WsEvent::Binary(b)) => {
            let d = BASE64.encode(b);
            frame_event(&state.borrow(), "Network.webSocketFrameReceived", &request_id, 2, false, d.clone());
            serde_json::json!({"t": "binary", "d": d})
        }
        Some(WsEvent::Close { code, reason, clean }) => {
            serde_json::json!({"t": "close", "code": code, "reason": reason, "clean": clean})
        }
        None => serde_json::json!({"t": "close", "code": 1006, "reason": "", "clean": false}),
    };
    if json["t"] == "close" {
        let mut state = state.borrow_mut();
        registry(&mut state).conns.remove(&id);
        record(&state, "Network.webSocketClosed", serde_json::json!({"requestId": request_id, "timestamp": now()}));
    }
    json.to_string()
}

