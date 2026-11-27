//! Opt-in retained-target reconnect over the real CDP server.
//!
//! #634 keeps a disconnected client's pages alive and drivable, but the
//! browser-level `Target.setAutoAttach` only announces a connection's own
//! pages. Playwright's `connectOverCDP` discovers pages exclusively through
//! `Target.attachedToTarget`, so after a disconnect it sees none of the
//! retained ones. A client can opt in with the `x-obscura-reconnect: 1`
//! WebSocket upgrade header; only then does the browser-level auto-attach
//! announce retained targets (never an active client's pages). Commands still
//! route to the original owner thread through `RemoteExec`; the Page and its
//! V8 isolate never move.
//!
//! Run with
//! `cargo nextest run -p obscura-cdp --features render -E 'test(reconnect_retained)'`.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn pick_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn connect(port: u16, opt_in: bool) -> Ws {
    let url = format!("ws://127.0.0.1:{port}/devtools/browser");
    let mut request = url.into_client_request().expect("ws request");
    if opt_in {
        request
            .headers_mut()
            .insert("x-obscura-reconnect", HeaderValue::from_static("1"));
    }
    let (ws, _) = tokio::time::timeout(Duration::from_secs(5), connect_async(request))
        .await
        .expect("ws handshake timeout")
        .expect("ws handshake");
    ws
}

async fn send(ws: &mut Ws, message: Value) {
    ws.send(Message::Text(message.to_string().into()))
        .await
        .expect("ws send");
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("cdp message timeout")
            .expect("ws closed")
            .expect("ws error");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("cdp json");
        }
    }
}

/// Wait for the reply to `id`, collecting events that arrive before it.
async fn reply_to(ws: &mut Ws, id: u64) -> (Value, Vec<Value>) {
    let mut events = Vec::new();
    loop {
        let message = next_json(ws).await;
        if message.get("id").and_then(Value::as_u64) == Some(id) {
            return (message, events);
        }
        events.push(message);
    }
}

fn attached_targets(events: &[Value], target_id: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|event| {
            event["method"] == "Target.attachedToTarget"
                && event["params"]["targetInfo"]["targetId"] == target_id
        })
        .cloned()
        .collect()
}

fn runtime_context_created_for(events: &[Value], session_id: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|event| {
            event["method"] == "Runtime.executionContextCreated"
                && event["sessionId"] == session_id
        })
        .cloned()
        .collect()
}

#[test]
fn reconnect_opt_in_reenables_runtime_for_the_new_session() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let port = pick_port().await;
        tokio::task::spawn_local(async move {
            let _ = obscura_cdp::start_with_serve_options_and_limit(
                port,
                "127.0.0.1",
                None,
                false,
                None,
                false,
                None,
                true,
                32,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The owner enables Runtime on its session before disconnecting. This
        // is the state that used to make a forwarded Runtime.enable look like
        // a repeat enable, suppressing executionContextCreated for the
        // reconnect session.
        let mut first = connect(port, false).await;
        send(
            &mut first,
            json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}}),
        )
        .await;
        let (reply, events) = reply_to(&mut first, 1).await;
        let target_id = reply["result"]["targetId"]
            .as_str()
            .expect("createTarget result")
            .to_string();
        let first_session = attached_targets(&events, &target_id)[0]["params"]["sessionId"]
            .as_str()
            .expect("createTarget attached session")
            .to_string();
        send(
            &mut first,
            json!({"id": 2, "method": "Runtime.enable", "sessionId": first_session}),
        )
        .await;
        let (enable_reply, _) = reply_to(&mut first, 2).await;
        assert!(enable_reply.get("error").is_none());
        first.close(None).await.expect("first close");
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut reconnect = connect(port, true).await;
        send(
            &mut reconnect,
            json!({"id": 4, "method": "Target.setAutoAttach",
                   "params": {"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}}),
        )
        .await;
        let (_, events) = reply_to(&mut reconnect, 4).await;
        let attached = attached_targets(&events, &target_id);
        assert_eq!(attached.len(), 1, "retained target announced once: {events:?}");
        let session_id = attached[0]["params"]["sessionId"]
            .as_str()
            .expect("attached session")
            .to_string();

        send(
            &mut reconnect,
            json!({"id": 5, "method": "Runtime.enable", "sessionId": session_id}),
        )
        .await;
        let (enable_reply, events) = reply_to(&mut reconnect, 5).await;
        assert!(enable_reply.get("error").is_none());
        assert!(
            !runtime_context_created_for(&events, &session_id).is_empty(),
            "reconnect session must receive its own execution context: {events:?}"
        );

        let _ = reconnect.close(None).await;
    });
}

#[test]
fn reconnect_opt_in_announces_and_drives_retained_targets() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let port = pick_port().await;
        tokio::task::spawn_local(async move {
            let _ = obscura_cdp::start_with_serve_options_and_limit(
                port,
                "127.0.0.1",
                None,
                false,
                None,
                false,
                None,
                true,
                32,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The first client creates a target and then disconnects without
        // closing it. Its pages stay alive on the owner thread.
        let mut first = connect(port, false).await;
        send(
            &mut first,
            json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}}),
        )
        .await;
        let (reply, _) = reply_to(&mut first, 1).await;
        let target_id = reply["result"]["targetId"]
            .as_str()
            .expect("createTarget result")
            .to_string();
        first.close(None).await.expect("first close");
        // Let the connection processor observe ConnectionClosed and retain the
        // pages before the reconnect handshake.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A connection without the opt-in header still never sees retained
        // targets through browser-level auto-attach.
        let mut plain = connect(port, false).await;
        send(
            &mut plain,
            json!({"id": 2, "method": "Target.setAutoAttach",
                   "params": {"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}}),
        )
        .await;
        let (_, plain_events) = reply_to(&mut plain, 2).await;
        assert!(
            attached_targets(&plain_events, &target_id).is_empty(),
            "retained target must stay hidden without opt-in: {plain_events:?}"
        );

        // The opted-in reconnect gets the retained target through the normal
        // browser-level attach lifecycle.
        let mut reconnect = connect(port, true).await;
        send(
            &mut reconnect,
            json!({"id": 3, "method": "Target.setAutoAttach",
                   "params": {"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}}),
        )
        .await;
        let (_, events) = reply_to(&mut reconnect, 3).await;
        let attached = attached_targets(&events, &target_id);
        assert_eq!(attached.len(), 1, "retained target announced once: {events:?}");
        let session_id = attached[0]["params"]["sessionId"]
            .as_str()
            .expect("attached session")
            .to_string();
        assert_eq!(attached[0]["params"]["targetInfo"]["type"], "page");

        // The retained page is drivable: the command routes to the owner
        // thread that still holds its Page and V8 isolate.
        send(
            &mut reconnect,
            json!({"id": 4, "method": "Page.getFrameTree", "sessionId": session_id}),
        )
        .await;
        let (frame_reply, _) = reply_to(&mut reconnect, 4).await;
        assert!(
            frame_reply.get("error").is_none(),
            "retained page must answer Page.getFrameTree: {frame_reply}"
        );
        assert!(frame_reply["result"]["frameTree"]["frame"]["id"].is_string());

        // Repeated setAutoAttach stays idempotent; Playwright aborts on a
        // duplicate target id.
        send(
            &mut reconnect,
            json!({"id": 5, "method": "Target.setAutoAttach",
                   "params": {"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}}),
        )
        .await;
        let (_, repeated_events) = reply_to(&mut reconnect, 5).await;
        assert!(
            attached_targets(&repeated_events, &target_id).is_empty(),
            "repeated auto-attach must not duplicate the target: {repeated_events:?}"
        );

        // Close the retained target through the reconnect session. It must not
        // be attachable after close, even by another opted-in client.
        send(
            &mut reconnect,
            json!({"id": 6, "method": "Target.closeTarget", "params": {"targetId": target_id}}),
        )
        .await;
        let (close_reply, _) = reply_to(&mut reconnect, 6).await;
        assert!(
            close_reply.get("error").is_none(),
            "closing the retained target must succeed: {close_reply}"
        );

        let mut after_close = connect(port, true).await;
        send(
            &mut after_close,
            json!({"id": 7, "method": "Target.setAutoAttach",
                   "params": {"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}}),
        )
        .await;
        let (_, after_close_events) = reply_to(&mut after_close, 7).await;
        assert!(
            attached_targets(&after_close_events, &target_id).is_empty(),
            "a closed target must never be attached again: {after_close_events:?}"
        );

        let _ = plain.close(None).await;
        let _ = reconnect.close(None).await;
        let _ = after_close.close(None).await;
    });
}
