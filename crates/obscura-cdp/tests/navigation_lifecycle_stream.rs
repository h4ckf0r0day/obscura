use std::time::Duration;
use std::io::Write;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct SharedTraceWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedTraceWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

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
        if request.starts_with("GET /busy-finished ") {
            let _ = slow_requested.send(());
            let body = "ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
        if request.starts_with("GET /stalled-primary ") {
            let _ = slow_requested.send(());
            tokio::time::sleep(Duration::from_secs(10)).await;
            continue;
        }
        if request.starts_with("GET /stalled-fetch ") {
            let _ = slow_requested.send(());
            tokio::time::sleep(Duration::from_secs(10)).await;
            continue;
        }
        if request.starts_with("GET /async-module ") {
            let body = "<!doctype html><script type=module>await fetch('/stalled-fetch')</script>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = socket.write_all(response.as_bytes()).await;
            continue;
        }
        if request.starts_with("GET /infinite.js ") {
            let _ = slow_requested.send(());
            let body = "console.warn('OBSCURA_SYNC_SCRIPT_ENTERED'); while (true) {}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = socket.write_all(response.as_bytes()).await;
            continue;
        }
        if request.starts_with("GET /infinite-parser ") {
            let body = "<!doctype html><script src=\"/infinite.js\"></script>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = socket.write_all(response.as_bytes()).await;
            continue;
        }
        if request.starts_with("GET /intercept-a") {
            let body = "<!doctype html><script>fetch('/a-data')</script><p>A</p>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = socket.write_all(response.as_bytes()).await;
            continue;
        }
        let dcl_script = if request.starts_with("GET /timeout ") {
            "const script = document.createElement('script'); script.src = '/never'; document.head.appendChild(script);"
        } else if request.starts_with("GET /busy-after-dcl ") {
            "setTimeout(() => { globalThis.__busyStarted = true; const until = performance.now() + 300; while (performance.now() < until) {} globalThis.__busyDone = true; fetch('/busy-finished'); }, 0);"
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
        let body = r#"<!doctype html><script>
            document.addEventListener('DOMContentLoaded', () => {
                globalThis.__dclSeen = (globalThis.__dclSeen || 0) + 1;
                __DCL_SCRIPT__
            });
            window.onload = () => {
                globalThis.__loadSeen = (globalThis.__loadSeen || 0) + 1;
                fetch('__POST_LOAD_FETCH__');
            };
        </script><p>ready</p>"#
            .replace("__DCL_SCRIPT__", dcl_script)
            .replace("__POST_LOAD_FETCH__", post_load_fetch);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
        let _ = socket.write_all(response.as_bytes()).await;
    }
}

async fn assert_navigation_teardown(
    path: &str,
    dispose_context: bool,
    wait_for_request: bool,
) {
    let sync_trace = (path == "/infinite-parser").then(|| {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let writer = trace.clone();
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || SharedTraceWriter(writer.clone()))
            .try_init()
            .expect("test process must own the tracing subscriber");
        trace
    });
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_url = format!("http://{}{path}", fixture.local_addr().unwrap());
    let (never_requested_tx, mut never_requested_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(
        fixture,
        never_requested_tx,
        release_rx,
    ));

    let port = pick_port().await;
    tokio::task::LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            let mut ws = connect_cdp(port).await;
            let context_id = if dispose_context {
                send(&mut ws, json!({
                    "id": 1,
                    "method": "Target.createBrowserContext",
                    "params": {},
                })).await;
                Some(loop {
                    let message = next_json(&mut ws).await;
                    if message["id"] == 1 {
                        break message["result"]["browserContextId"]
                            .as_str()
                            .unwrap()
                            .to_string();
                    }
                })
            } else {
                None
            };
            let (target_id, session_id) =
                create_target(&mut ws, 2, context_id.as_deref()).await;
            send(&mut ws, json!({
                "id": 3,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {"url": fixture_url},
            })).await;
            if wait_for_request {
                tokio::time::timeout(Duration::from_secs(7), never_requested_rx.recv())
                    .await
                    .expect("navigation fixture request was not observed")
                    .expect("fixture observation channel closed");
                if path == "/infinite-parser" {
                    let trace = sync_trace.as_ref().unwrap();
                    tokio::time::timeout(Duration::from_secs(2), async {
                        loop {
                            if String::from_utf8_lossy(&trace.lock().unwrap())
                                .contains("OBSCURA_SYNC_SCRIPT_ENTERED")
                            {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    }).await.expect("synchronous script never entered V8");
                }
            }

            let teardown = if let Some(context_id) = context_id.as_ref() {
                json!({
                    "id": 4,
                    "method": "Target.disposeBrowserContext",
                    "params": {"browserContextId": context_id},
                })
            } else {
                json!({
                    "id": 4,
                    "method": "Target.closeTarget",
                    "params": {"targetId": target_id},
                })
            };
            send(&mut ws, teardown).await;
            let close_started = tokio::time::Instant::now();
            let mut close_acknowledged = false;
            let mut destroyed = false;
            while !close_acknowledged || !destroyed {
                let message = next_json(&mut ws).await;
                close_acknowledged |= message["id"] == 4
                    && (dispose_context || message["result"]["success"] == json!(true));
                destroyed |= message["method"] == "Target.targetDestroyed"
                    && message["params"]["targetId"].as_str() == Some(target_id.as_str());
            }
            assert!(
                close_started.elapsed() < Duration::from_secs(2),
                "closing a task-owned target did not cancel navigation promptly",
            );

            let (_replacement_id, replacement_session) =
                create_target(&mut ws, 5, None).await;
            send(&mut ws, json!({
                "id": 6,
                "method": "Runtime.evaluate",
                "sessionId": replacement_session,
                "params": {"expression": "40 + 2", "returnByValue": true},
            })).await;
            let replacement_result = loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 6 {
                    break message;
                }
            };
            assert_eq!(
                replacement_result["result"]["result"]["value"],
                json!(42.0),
            );
            let _ = ws.close(None).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn closing_target_cancels_stalled_primary_navigation() {
    assert_navigation_teardown("/stalled-primary", false, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn disposing_context_cancels_stalled_primary_navigation() {
    assert_navigation_teardown("/stalled-primary", true, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn closing_target_terminates_synchronous_navigation_script() {
    assert_navigation_teardown("/infinite-parser", false, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn disposing_context_terminates_synchronous_navigation_script() {
    assert_navigation_teardown("/infinite-parser", true, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn closing_target_cancels_navigation_parked_in_async_module_fetch() {
    assert_navigation_teardown("/async-module", false, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn close_queued_immediately_after_navigate_cancels_registered_owner() {
    assert_navigation_teardown("/stalled-primary", false, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn closing_connection_cancels_stalled_primary_navigation() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_url = format!("http://{}/stalled-primary", fixture.local_addr().unwrap());
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(fixture, request_tx, release_rx));

    let port = pick_port().await;
    tokio::task::LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start_with_serve_options_and_limit(
                    port,
                    "127.0.0.1",
                    None,
                    false,
                    None,
                    false,
                    None,
                    true,
                    1,
                )
                .await;
            });
            let mut ws = connect_cdp(port).await;
            let (_target_id, session_id) = create_target(&mut ws, 1, None).await;
            send(&mut ws, json!({
                "id": 2,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {"url": fixture_url},
            })).await;
            tokio::time::timeout(Duration::from_secs(7), request_rx.recv())
                .await
                .expect("stalled primary request was not observed")
                .expect("fixture observation channel closed");
            let close_started = tokio::time::Instant::now();
            let _ = ws.close(None).await;
            drop(ws);

            let mut replacement = connect_cdp(port).await;
            assert!(
                close_started.elapsed() < Duration::from_secs(2),
                "connection slot was not released after cancelling navigation",
            );
            let (_replacement_id, replacement_session) =
                create_target(&mut replacement, 3, None).await;
            send(&mut replacement, json!({
                "id": 4,
                "method": "Runtime.evaluate",
                "sessionId": replacement_session,
                "params": {"expression": "6 * 7", "returnByValue": true},
            })).await;
            let result = loop {
                let message = next_json(&mut replacement).await;
                if message["id"] == 4 {
                    break message;
                }
            };
            assert_eq!(result["result"]["result"]["value"], json!(42.0));
            let _ = replacement.close(None).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn closing_target_fails_paused_navigation_interception() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_url = format!("http://{}/intercepted", fixture.local_addr().unwrap());
    let (request_tx, _request_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(fixture, request_tx, release_rx));

    let port = pick_port().await;
    tokio::task::LocalSet::new()
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            let mut ws = connect_cdp(port).await;
            let (target_id, session_id) = create_target(&mut ws, 1, None).await;
            send(&mut ws, json!({
                "id": 2,
                "method": "Fetch.enable",
                "sessionId": session_id,
                "params": {"patterns": [{"urlPattern": "*"}]},
            })).await;
            loop {
                if next_json(&mut ws).await["id"] == 2 {
                    break;
                }
            }
            send(&mut ws, json!({
                "id": 3,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {"url": fixture_url},
            })).await;
            loop {
                let message = next_json(&mut ws).await;
                if message["method"] == "Fetch.requestPaused" {
                    break;
                }
            }
            send(&mut ws, json!({
                "id": 4,
                "method": "Target.closeTarget",
                "params": {"targetId": target_id},
            })).await;
            let close_started = tokio::time::Instant::now();
            let mut acknowledged = false;
            let mut destroyed = false;
            while !acknowledged || !destroyed {
                let message = next_json(&mut ws).await;
                acknowledged |= message["id"] == 4
                    && message["result"]["success"] == json!(true);
                destroyed |= message["method"] == "Target.targetDestroyed"
                    && message["params"]["targetId"].as_str() == Some(target_id.as_str());
            }
            assert!(
                close_started.elapsed() < Duration::from_secs(2),
                "paused interception prevented target teardown",
            );
            let (_replacement_id, replacement_session) =
                create_target(&mut ws, 5, None).await;
            send(&mut ws, json!({
                "id": 6,
                "method": "Runtime.evaluate",
                "sessionId": replacement_session,
                "params": {"expression": "42", "returnByValue": true},
            })).await;
            loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 6 {
                    assert_eq!(message["result"]["result"]["value"], json!(42.0));
                    break;
                }
            }
            let _ = ws.close(None).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn closing_stalled_target_preserves_sibling_paused_interception() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", fixture.local_addr().unwrap());
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(fixture, request_tx, release_rx));

    let port = pick_port().await;
    tokio::task::LocalSet::new().run_until(async move {
        tokio::task::spawn_local(async move {
            let _ = obscura_cdp::server::start(port).await;
        });
        let mut ws = connect_cdp(port).await;
        let (target_a, session_a) = create_target(&mut ws, 1, None).await;
        send(&mut ws, json!({
            "id": 2, "method": "Fetch.enable", "sessionId": session_a,
            "params": {"patterns": [{"urlPattern": "*"}]},
        })).await;
        while next_json(&mut ws).await["id"] != 2 {}
        send(&mut ws, json!({
            "id": 3, "method": "Page.navigate", "sessionId": session_a,
            "params": {"url": format!("{origin}/intercept-a")},
        })).await;
        let mut request_a = None;
        let mut navigation_a_done = false;
        while request_a.is_none() || !navigation_a_done {
            let message = next_json(&mut ws).await;
            navigation_a_done |= message["id"] == 3;
            if message["method"] == "Fetch.requestPaused"
                && message["sessionId"].as_str() == Some(session_a.as_str())
                && message["params"]["request"]["url"].as_str().is_some_and(|url| url.ends_with("/a-data"))
            {
                request_a = message["params"]["requestId"].as_str().map(str::to_string);
            }
        }

        let (target_b, session_b) = create_target(&mut ws, 4, None).await;
        send(&mut ws, json!({
            "id": 40, "method": "Fetch.enable", "sessionId": session_b,
            "params": {"patterns": [{"urlPattern": "*"}]},
        })).await;
        while next_json(&mut ws).await["id"] != 40 {}
        send(&mut ws, json!({
            "id": 41, "method": "Page.navigate", "sessionId": session_b,
            "params": {"url": format!("{origin}/intercept-a?b")},
        })).await;
        let mut request_b = None;
        let mut navigation_b_done = false;
        while request_b.is_none() || !navigation_b_done {
            let message = next_json(&mut ws).await;
            navigation_b_done |= message["id"] == 41;
            if message["method"] == "Fetch.requestPaused"
                && message["sessionId"].as_str() == Some(session_b.as_str())
                && message["params"]["request"]["url"].as_str().is_some_and(|url| url.ends_with("/a-data"))
            {
                request_b = message["params"]["requestId"].as_str().map(str::to_string);
            }
        }
        assert_ne!(request_a.as_deref(), request_b.as_deref());
        send(&mut ws, json!({
            "id": 5, "method": "Page.navigate", "sessionId": session_b,
            "params": {"url": format!("{origin}/stalled-primary")},
        })).await;
        tokio::time::timeout(Duration::from_secs(7), request_rx.recv())
            .await.expect("stalled B request not observed").expect("fixture channel closed");
        send(&mut ws, json!({
            "id": 6, "method": "Target.closeTarget", "params": {"targetId": target_b},
        })).await;
        while next_json(&mut ws).await["id"] != 6 {}

        send(&mut ws, json!({
            "id": 7, "method": "Fetch.continueRequest", "sessionId": session_a,
            "params": {"requestId": request_a.unwrap()},
        })).await;
        let continued = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 7 { break message; }
            }
        }).await.expect("closing B discarded A's paused interception");
        assert!(continued.get("error").is_none(), "{continued}");
        send(&mut ws, json!({
            "id": 8, "method": "Target.closeTarget", "params": {"targetId": target_a},
        })).await;
        let _ = ws.close(None).await;
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn replacing_runtime_aborts_old_pause_and_uses_new_request_identity() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", fixture.local_addr().unwrap());
    let (request_tx, _request_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(fixture, request_tx, release_rx));
    let port = pick_port().await;
    tokio::task::LocalSet::new().run_until(async move {
        tokio::task::spawn_local(async move { let _ = obscura_cdp::server::start(port).await; });
        let mut ws = connect_cdp(port).await;
        let (target_id, session_id) = create_target(&mut ws, 1, None).await;
        send(&mut ws, json!({
            "id": 2, "method": "Fetch.enable", "sessionId": session_id,
            "params": {"patterns": [{"urlPattern": "*"}]},
        })).await;
        while next_json(&mut ws).await["id"] != 2 {}

        async fn navigate_to_pause<S>(
            ws: &mut tokio_tungstenite::WebSocketStream<S>,
            id: i64,
            session_id: &str,
            url: &str,
        ) -> String
        where S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin {
            send(ws, json!({
                "id": id, "method": "Page.navigate", "sessionId": session_id,
                "params": {"url": url},
            })).await;
            let mut request_id = None;
            let mut navigation_done = false;
            while request_id.is_none() || !navigation_done {
                let message = next_json(ws).await;
                navigation_done |= message["id"] == id;
                if message["method"] == "Fetch.requestPaused"
                    && message["sessionId"].as_str() == Some(session_id)
                    && message["params"]["request"]["url"].as_str().is_some_and(|url| url.ends_with("/a-data"))
                {
                    request_id = message["params"]["requestId"].as_str().map(str::to_string);
                }
            }
            request_id.unwrap()
        }

        let first = navigate_to_pause(&mut ws, 3, &session_id, &format!("{origin}/intercept-a")).await;
        let second = navigate_to_pause(&mut ws, 4, &session_id, &format!("{origin}/intercept-a?replacement")).await;
        assert_ne!(first, second, "runtime replacement reused interception identity");
        send(&mut ws, json!({
            "id": 5, "method": "Fetch.continueRequest", "sessionId": session_id,
            "params": {"requestId": first},
        })).await;
        let old = loop { let message = next_json(&mut ws).await; if message["id"] == 5 { break message; } };
        assert!(old.get("error").is_some(), "old document pause remained resolvable: first={first} second={second} response={old}");
        send(&mut ws, json!({
            "id": 6, "method": "Fetch.continueRequest", "sessionId": session_id,
            "params": {"requestId": second},
        })).await;
        let new = loop { let message = next_json(&mut ws).await; if message["id"] == 6 { break message; } };
        assert!(new.get("error").is_none(), "replacement pause was lost: {new}");
        send(&mut ws, json!({"id": 7, "method": "Target.closeTarget", "params": {"targetId": target_id}})).await;
        let _ = ws.close(None).await;
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn closing_loaded_target_aborts_autonomous_paused_request() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/intercept-a", fixture.local_addr().unwrap());
    let (request_tx, _request_rx) = mpsc::unbounded_channel();
    let (_release_tx, release_rx) = oneshot::channel();
    tokio::spawn(serve_delayed_load_fixture(fixture, request_tx, release_rx));
    let port = pick_port().await;
    tokio::task::LocalSet::new().run_until(async move {
        tokio::task::spawn_local(async move { let _ = obscura_cdp::server::start(port).await; });
        let mut ws = connect_cdp(port).await;
        let (target_id, session_id) = create_target(&mut ws, 1, None).await;
        send(&mut ws, json!({
            "id": 2, "method": "Fetch.enable", "sessionId": session_id,
            "params": {"patterns": [{"urlPattern": "*"}]},
        })).await;
        while next_json(&mut ws).await["id"] != 2 {}
        send(&mut ws, json!({
            "id": 3, "method": "Page.navigate", "sessionId": session_id,
            "params": {"url": url},
        })).await;
        let mut request_id = None;
        let mut navigation_done = false;
        while request_id.is_none() || !navigation_done {
            let message = next_json(&mut ws).await;
            navigation_done |= message["id"] == 3;
            if message["method"] == "Fetch.requestPaused" {
                if message["params"]["request"]["url"].as_str().is_some_and(|url| url.ends_with("/a-data")) {
                    request_id = message["params"]["requestId"].as_str().map(str::to_string);
                }
            }
        }
        send(&mut ws, json!({
            "id": 4, "method": "Target.closeTarget", "params": {"targetId": target_id},
        })).await;
        while next_json(&mut ws).await["id"] != 4 {}
        send(&mut ws, json!({
            "id": 5, "method": "Fetch.continueRequest", "sessionId": session_id,
            "params": {"requestId": request_id.unwrap()},
        })).await;
        let stale = loop { let message = next_json(&mut ws).await; if message["id"] == 5 { break message; } };
        assert!(stale.get("error").is_some(), "closed target retained paused request: {stale}");
        let (_other, other_session) = create_target(&mut ws, 6, None).await;
        send(&mut ws, json!({
            "id": 7, "method": "Runtime.evaluate", "sessionId": other_session,
            "params": {"expression": "42", "returnByValue": true},
        })).await;
        let result = loop { let message = next_json(&mut ws).await; if message["id"] == 7 { break message; } };
        assert_eq!(result["result"]["result"]["value"], json!(42.0));
        let _ = ws.close(None).await;
    }).await;
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
async fn same_page_command_wins_the_first_post_dcl_pump() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_url = format!("http://{}/busy-after-dcl", fixture.local_addr().unwrap());
    let (slow_requested_tx, mut busy_finished_rx) = mpsc::unbounded_channel();
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
            let (target_id, session_id) = create_target(&mut ws, 1, None).await;
            enable_page_lifecycle(&mut ws, &session_id, 2).await;
            send(&mut ws, json!({
                "id": 3,
                "method": "Page.startScreencast",
                "sessionId": session_id,
                "params": {},
            })).await;
            loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 3 {
                    break;
                }
            }

            send(&mut ws, json!({
                "id": 4,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {"url": fixture_url},
            })).await;
            loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 4 {
                    break;
                }
            }

            // Playwright's waitUntil=commit returns before DOMContentLoaded.
            // Queue the follow-up while outer response/event finalization still
            // owns routing; it must precede the first post-DCL background poll.
            send(&mut ws, json!({
                "id": 5,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "expression": "[globalThis.__busyStarted === true, globalThis.__busyDone === true]",
                    "returnByValue": true,
                },
            })).await;
            let immediate = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let message = next_json(&mut ws).await;
                    if message["id"] == 5 {
                        break message;
                    }
                }
            })
            .await
            .expect("the same-page command did not complete");
            assert_eq!(
                immediate["result"]["result"]["value"],
                json!([false, false]),
            );

            // The grace is one-shot. Prove autonomous progress out of band,
            // before another CDP command can wake the processor.
            tokio::time::timeout(Duration::from_secs(2), busy_finished_rx.recv())
                .await
                .expect("the background task did not progress under silence")
                .expect("the fixture observation channel closed");
            send(&mut ws, json!({
                "id": 6,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "expression": "globalThis.__busyDone === true",
                    "returnByValue": true,
                },
            })).await;
            let completed = loop {
                let message = next_json(&mut ws).await;
                if message["id"] == 6 {
                    break message;
                }
            };
            assert_eq!(completed["result"]["result"]["value"], json!(true));
            send(&mut ws, json!({
                "id": 7,
                "method": "Target.closeTarget",
                "params": {"targetId": target_id},
            })).await;
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
