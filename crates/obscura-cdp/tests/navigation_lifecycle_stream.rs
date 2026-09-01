use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn pick_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn connect_cdp(
    port: u16,
) -> tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
> {
    let url = format!("ws://127.0.0.1:{port}/devtools/browser");
    let mut last_error = None;
    for _ in 0..100 {
        match connect_async(&url).await {
            Ok((ws, _)) => return ws,
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("CDP server did not become ready: {:?}", last_error);
}

async fn serve_delayed_load_fixture(
    listener: TcpListener,
    slow_requested: mpsc::UnboundedSender<()>,
    mut release_slow: oneshot::Receiver<()>,
) {
    let release = async move {
        let _ = (&mut release_slow).await;
    };
    tokio::pin!(release);
    loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 2048];
        let read = socket.read(&mut buffer).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..read]);
        if request.starts_with("GET /slow.js ") {
            let _ = slow_requested.send(());
            release.as_mut().await;
            let body = "globalThis.__slowScriptDone = true;";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = socket.write_all(response.as_bytes()).await;
            continue;
        }
        if request.starts_with("GET /after-load ") {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let body = "ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = socket.write_all(response.as_bytes()).await;
            continue;
        }
        if request.starts_with("GET /never ") {
            tokio::time::sleep(Duration::from_secs(10)).await;
            continue;
        }
        let dcl_script = if request.starts_with("GET /timeout ") {
            "const script = document.createElement('script'); script.src = '/never'; document.head.appendChild(script);"
        } else if request.starts_with("GET /post-load-timeout ") {
            ""
        } else {
            "const script = document.createElement('script'); script.src = '/slow.js'; document.head.appendChild(script);"
        };
        let post_load_fetch = if request.starts_with("GET /post-load-timeout ") {
            "/never"
        } else {
            "/after-load"
        };
        let onload_extra = if request.starts_with("GET /post-load-timeout ") {
            "setInterval(() => {}, 0);"
        } else {
            ""
        };
        let body = r#"<!doctype html><script>
            document.addEventListener('DOMContentLoaded', () => {
                globalThis.__dclSeen = (globalThis.__dclSeen || 0) + 1;
                __DCL_SCRIPT__
            });
            window.onload = () => {
                globalThis.__loadSeen = (globalThis.__loadSeen || 0) + 1;
                fetch('__POST_LOAD_FETCH__');
                __ONLOAD_EXTRA__
            };
        </script><p>ready</p>"#
            .replace("__DCL_SCRIPT__", dcl_script)
            .replace("__POST_LOAD_FETCH__", post_load_fetch)
            .replace("__ONLOAD_EXTRA__", onload_extra);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
        let _ = socket.write_all(response.as_bytes()).await;
    }
}

async fn next_json<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = tokio::time::timeout(Duration::from_secs(7), ws.next())
        .await
        .expect("CDP message timeout")
        .expect("CDP WebSocket closed")
        .expect("CDP WebSocket error");
    match message {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("unexpected WebSocket message: {other:?}"),
    }
}

async fn send<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    value: Value,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    ws.send(Message::Text(value.to_string().into())).await.unwrap();
}

async fn create_target<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    id: i64,
    browser_context_id: Option<&str>,
) -> (String, String)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut params = json!({"url": "about:blank"});
    if let Some(context_id) = browser_context_id {
        params["browserContextId"] = json!(context_id);
    }
    send(ws, json!({"id": id, "method": "Target.createTarget", "params": params})).await;
    let mut session_id = None;
    let mut target_id = None;
    while session_id.is_none() || target_id.is_none() {
        let message = next_json(ws).await;
        if message["method"] == "Target.attachedToTarget" {
            session_id = message["params"]["sessionId"].as_str().map(str::to_string);
        }
        if message["id"] == id {
            target_id = message["result"]["targetId"].as_str().map(str::to_string);
        }
    }
    (target_id.unwrap(), session_id.unwrap())
}

async fn enable_page_lifecycle<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    session_id: &str,
    first_id: i64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send(ws, json!({
        "id": first_id,
        "method": "Page.enable",
        "sessionId": session_id,
        "params": {},
    })).await;
    while next_json(ws).await["id"] != first_id {}
    send(ws, json!({
        "id": first_id + 1,
        "method": "Page.setLifecycleEventsEnabled",
        "sessionId": session_id,
        "params": {"enabled": true},
    })).await;
    while next_json(ws).await["id"] != first_id + 1 {}
}

#[tokio::test(flavor = "current_thread")]
async fn domcontentloaded_returns_before_a_load_delaying_script() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_url = format!("http://{}/", fixture.local_addr().unwrap());
    let (slow_requested_tx, mut slow_requested_rx) = mpsc::unbounded_channel();
    let (release_slow_tx, release_slow_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(
        fixture,
        slow_requested_tx,
        release_slow_rx,
    ));

    let port = pick_port().await;
    tokio::task::LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            let mut ws = connect_cdp(port).await;

            let (target_id, session_id) = create_target(&mut ws, 1, None).await;
            enable_page_lifecycle(&mut ws, &session_id, 2).await;

            send(&mut ws, json!({
                "id": 31,
                "method": "Network.enable",
                "sessionId": session_id,
                "params": {},
            }))
            .await;
            while next_json(&mut ws).await["id"] != 31 {}

            send(&mut ws, json!({
                "id": 4,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {"url": fixture_url},
            }))
            .await;

            tokio::time::timeout(Duration::from_secs(2), slow_requested_rx.recv())
                .await
                .expect("load-delaying script was not requested")
                .expect("fixture request channel closed");

            let mut sequence = Vec::new();
            let mut saw_response = false;
            let mut saw_dcl = false;
            while !saw_response || !saw_dcl {
                let message = next_json(&mut ws).await;
                if message["id"] == 4 {
                    saw_response = true;
                    sequence.push("response");
                } else if message["method"] == "Page.lifecycleEvent"
                    && message["params"]["name"] == "init"
                {
                    sequence.push("init");
                } else if message["method"] == "Page.frameNavigated" {
                    sequence.push("frameNavigated");
                } else if message["method"] == "Page.domContentEventFired" {
                    sequence.push("domContentEventFired");
                } else if message["method"] == "Page.lifecycleEvent"
                    && message["params"]["name"] == "DOMContentLoaded"
                {
                    saw_dcl = true;
                    sequence.push("DOMContentLoaded");
                } else if message["method"] == "Page.lifecycleEvent"
                    && message["params"]["name"] == "load"
                {
                    panic!("load fired before the delayed script completed: {message}");
                }
            }
            assert_eq!(
                sequence,
                [
                    "response",
                    "init",
                    "frameNavigated",
                    "domContentEventFired",
                    "DOMContentLoaded",
                ],
            );

            send(&mut ws, json!({
                "id": 5,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "expression": "[document.readyState, globalThis.__dclSeen || 0, globalThis.__loadSeen || 0, globalThis.__slowScriptDone === true]",
                    "returnByValue": true,
                },
            }))
            .await;
            let evaluated = loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 5 {
                    break message;
                }
            };
            assert_eq!(
                evaluated["result"]["result"]["value"],
                json!(["interactive", 1, 0, false]),
            );

            // A page-owned lifecycle continuation must not head-of-line block
            // commands for another target on the same connection.
            send(&mut ws, json!({
                "id": 50,
                "method": "Target.createTarget",
                "params": {"url": "about:blank"},
            }))
            .await;
            let mut second_session = None;
            let mut second_target = None;
            let mut saw_after_load_response = false;
            while second_session.is_none() || second_target.is_none() {
                let message = next_json(&mut ws).await;
                if message["method"] == "Network.responseReceived"
                    && message["params"]["response"]["url"]
                        .as_str()
                        .is_some_and(|url| url.ends_with("/after-load"))
                {
                    saw_after_load_response = true;
                }
                if message["method"] == "Target.attachedToTarget" {
                    second_session = message["params"]["sessionId"]
                        .as_str()
                        .map(str::to_string);
                }
                if message["id"] == 50 {
                    second_target = message["result"]["targetId"]
                        .as_str()
                        .map(str::to_string);
                }
            }
            let second_session = second_session.unwrap();
            let second_target = second_target.unwrap();
            send(&mut ws, json!({
                "id": 51,
                "method": "Runtime.evaluate",
                "sessionId": second_session,
                "params": {"expression": "51"},
            }))
            .await;
            release_slow_tx.send(()).unwrap();
            let mut load_sequence = Vec::new();
            let mut slow_request_id = None;
            let mut other_target_response = None;
            while load_sequence.last() != Some(&"frameStoppedLoading")
                || !saw_after_load_response
                || other_target_response.is_none()
            {
                let message = next_json(&mut ws).await;
                if message["id"] == 51 {
                    assert!(
                        saw_after_load_response,
                        "another target suspended work queued by the load handler",
                    );
                    other_target_response = Some(message);
                } else if message["method"] == "Network.responseReceived"
                    && message["params"]["response"]["url"]
                        .as_str()
                        .is_some_and(|url| url.ends_with("/after-load"))
                {
                    saw_after_load_response = true;
                } else if message["method"] == "Network.responseReceived"
                    && message["params"]["response"]["url"]
                        .as_str()
                        .is_some_and(|url| url.ends_with("/slow.js"))
                {
                    slow_request_id = message["params"]["requestId"]
                        .as_str()
                        .map(str::to_string);
                    load_sequence.push("slowResponse");
                } else if message["method"] == "Network.loadingFinished"
                    && message["params"]["requestId"].as_str() == slow_request_id.as_deref()
                {
                    load_sequence.push("slowFinished");
                } else if message["method"] == "Page.loadEventFired" {
                    if message["sessionId"].as_str() != Some(session_id.as_str()) {
                        continue;
                    }
                    load_sequence.push("loadEventFired");
                } else if message["method"] == "Page.lifecycleEvent"
                    && message["params"]["name"] == "load"
                    && message["sessionId"].as_str() == Some(session_id.as_str())
                {
                    load_sequence.push("load");
                } else if message["method"] == "Page.frameStoppedLoading"
                    && message["sessionId"].as_str() == Some(session_id.as_str())
                {
                    load_sequence.push("frameStoppedLoading");
                }
            }
            assert_eq!(
                load_sequence,
                [
                    "slowResponse",
                    "slowFinished",
                    "loadEventFired",
                    "load",
                    "frameStoppedLoading",
                ],
            );
            assert_eq!(
                other_target_response.unwrap()["result"]["result"]["value"],
                json!("51"),
            );
            send(&mut ws, json!({
                "id": 60,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {"url": format!("{fixture_url}timeout")},
            }))
            .await;
            let mut timeout_page_dcl = false;
            while !timeout_page_dcl {
                let message = next_json(&mut ws).await;
                timeout_page_dcl = message["method"] == "Page.lifecycleEvent"
                    && message["params"]["name"] == "DOMContentLoaded"
                    && message["sessionId"].as_str() == Some(session_id.as_str());
            }
            send(&mut ws, json!({
                "id": 61,
                "method": "Runtime.evaluate",
                "sessionId": second_session,
                "params": {"expression": "2"},
            }))
            .await;
            let deferral_deadline = tokio::time::Instant::now() + Duration::from_millis(150);
            while let Ok(Some(Ok(Message::Text(text)))) =
                tokio::time::timeout_at(deferral_deadline, ws.next()).await
            {
                let message: Value = serde_json::from_str(&text).unwrap();
                assert_ne!(message["id"], 61,
                    "another target entered V8 while the lifecycle owner was pending load");
            }
            send(&mut ws, json!({
                "id": 7,
                "method": "Target.closeTarget",
                "params": {"targetId": target_id},
            }))
            .await;
            let mut close_acknowledged = false;
            let mut resumed = None;
            while !close_acknowledged || resumed.is_none() {
                let message = next_json(&mut ws).await;
                close_acknowledged |= message["id"] == 7;
                if message["id"] == 61 {
                    resumed = Some(message);
                }
            }
            assert_eq!(resumed.unwrap()["result"]["result"]["value"], json!("2"));
            send(&mut ws, json!({
                "id": 62,
                "method": "Runtime.evaluate",
                "sessionId": second_session,
                "params": {"expression": "3"},
            }))
            .await;
            let after_close_started = tokio::time::Instant::now();
            let after_close = loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 62 {
                    break message;
                }
            };
            assert!(
                after_close_started.elapsed() < Duration::from_secs(2),
                "closing the lifecycle owner stranded another target",
            );
            assert_eq!(after_close["result"]["result"]["value"], json!("3"));
            send(&mut ws, json!({
                "id": 8,
                "method": "Target.closeTarget",
                "params": {"targetId": second_target},
            }))
            .await;
            let _ = ws.close(None).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn disposing_context_during_load_releases_deferred_target() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_url = format!("http://{}/timeout", fixture.local_addr().unwrap());
    let (slow_requested_tx, _slow_requested_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(
        fixture,
        slow_requested_tx,
        release_rx,
    ));

    let port = pick_port().await;
    tokio::task::LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            let mut ws = connect_cdp(port).await;
            let (_control_target, control_session) = create_target(&mut ws, 1, None).await;

            send(&mut ws, json!({
                "id": 2,
                "method": "Target.createBrowserContext",
                "params": {},
            })).await;
            let context_id = loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 2 {
                    break message["result"]["browserContextId"]
                        .as_str()
                        .unwrap()
                        .to_string();
                }
            };
            let (owned_target, owned_session) =
                create_target(&mut ws, 3, Some(&context_id)).await;
            enable_page_lifecycle(&mut ws, &owned_session, 4).await;

            send(&mut ws, json!({
                "id": 6,
                "method": "Page.navigate",
                "sessionId": owned_session,
                "params": {"url": fixture_url},
            })).await;
            send(&mut ws, json!({
                "id": 7,
                "method": "Runtime.evaluate",
                "sessionId": control_session,
                "params": {"expression": "7"},
            })).await;
            loop {
                let message = next_json(&mut ws).await;
                assert_ne!(message["id"], 7, "control target entered V8 before DCL");
                if message["method"] == "Page.lifecycleEvent"
                    && message["params"]["name"] == "DOMContentLoaded"
                {
                    break;
                }
            }

            let deferral_deadline = tokio::time::Instant::now() + Duration::from_millis(150);
            while let Ok(Some(Ok(Message::Text(text)))) =
                tokio::time::timeout_at(deferral_deadline, ws.next()).await
            {
                let message: Value = serde_json::from_str(&text).unwrap();
                assert_ne!(message["id"], 7, "control target entered V8 before disposal");
            }

            send(&mut ws, json!({
                "id": 8,
                "method": "Target.disposeBrowserContext",
                "params": {"browserContextId": context_id},
            })).await;
            let mut disposed = false;
            let mut resumed = None;
            let mut destroyed = false;
            while !disposed || resumed.is_none() || !destroyed {
                let message = next_json(&mut ws).await;
                disposed |= message["id"] == 8;
                if message["id"] == 7 {
                    resumed = Some(message.clone());
                }
                destroyed |= message["method"] == "Target.targetDestroyed"
                    && message["params"]["targetId"].as_str() == Some(owned_target.as_str());
            }
            assert_eq!(resumed.unwrap()["result"]["result"]["value"], json!("7"));
            let _ = ws.close(None).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn post_load_drain_releases_other_target_at_absolute_bound() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_url = format!("http://{}/post-load-timeout", fixture.local_addr().unwrap());
    let (slow_requested_tx, _slow_requested_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(
        fixture,
        slow_requested_tx,
        release_rx,
    ));

    let port = pick_port().await;
    tokio::task::LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            let mut ws = connect_cdp(port).await;
            let (owner_target, owner_session) = create_target(&mut ws, 1, None).await;
            let (other_target, other_session) = create_target(&mut ws, 2, None).await;
            enable_page_lifecycle(&mut ws, &owner_session, 3).await;

            send(&mut ws, json!({
                "id": 5,
                "method": "Page.navigate",
                "sessionId": owner_session,
                "params": {"url": fixture_url},
            })).await;
            send(&mut ws, json!({
                "id": 6,
                "method": "Runtime.evaluate",
                "sessionId": other_session,
                "params": {"expression": "6"},
            })).await;

            let mut load_at = None;
            let resumed = loop {
                let message = next_json(&mut ws).await;
                if message["method"] == "Page.lifecycleEvent"
                    && message["params"]["name"] == "load"
                    && message["sessionId"].as_str() == Some(owner_session.as_str())
                {
                    load_at = Some(tokio::time::Instant::now());
                }
                if message["id"] == 6 {
                    assert!(load_at.is_some(), "other target resumed before load");
                    break message;
                }
            };
            let drain_elapsed = load_at.unwrap().elapsed();
            assert!(
                drain_elapsed >= Duration::from_millis(800),
                "post-load drain released too early: {drain_elapsed:?}",
            );
            assert!(
                drain_elapsed < Duration::from_secs(2),
                "post-load drain exceeded its absolute bound: {drain_elapsed:?}",
            );
            assert_eq!(resumed["result"]["result"]["value"], json!("6"));

            send(&mut ws, json!({
                "id": 7,
                "method": "Runtime.evaluate",
                "sessionId": owner_session,
                "params": {"expression": "7"},
            })).await;
            let owner_resumed = loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 7 {
                    break message;
                }
            };
            assert_eq!(owner_resumed["result"]["result"]["value"], json!("7"));
            send(&mut ws, json!({
                "id": 8,
                "method": "Target.closeTarget",
                "params": {"targetId": owner_target},
            })).await;
            send(&mut ws, json!({
                "id": 9,
                "method": "Target.closeTarget",
                "params": {"targetId": other_target},
            })).await;
            let _ = ws.close(None).await;
        })
        .await;
}
