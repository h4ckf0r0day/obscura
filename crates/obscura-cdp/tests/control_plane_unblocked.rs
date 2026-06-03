//! Issue #62 regression test: `/json/version` (HTTP control plane) must
//! respond promptly even while V8 JS evaluation blocks the LocalSet thread.
//!
//! Before the fix, the HTTP accept loop competed with the CDP processor on
//! the same `LocalSet`, so a synchronous JS `while` loop starved every other
//! task — including the async `TcpListener::accept()`. The dedicated accept
//! thread (std::net::TcpListener on a separate OS thread) fixes this.
//!
//! Run with `cargo test -p obscura-cdp --test control_plane_unblocked
//! -- --nocapture --ignored`.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const JS_DURATION_MS: u64 = 5000;

// Pick a free port for the test server. There is a small TOCTOU window
// between dropping the listener here and the server's own bind a few lines
// later — under heavy CI parallelism another process could steal the port
// in between. The test is `#[ignore]` and opt-in via `--ignored`, so it
// runs serially in practice. If we ever flip it to a default-on integration
// test, replace this with a SO_REUSEPORT-aware port lease or pass the
// already-bound listener into the server.
async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

#[tokio::test(flavor = "current_thread")]

async fn http_control_plane_unblocked_during_long_js() {
    let port = pick_port().await;
    let port_clone = port;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            // Give the listener + accept thread time to bind.
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Connect via WebSocket, create a target, then send a
            // long-running JS evaluation on that target's session.
            let url = format!("ws://127.0.0.1:{}/devtools/browser", port);
            let (mut ws, _) = connect_async(&url).await.unwrap();

            let create = json!({
                "id": 1,
                "method": "Target.createTarget",
                "params": {"url": "about:blank"},
            });
            ws.send(Message::Text(create.to_string().into()))
                .await
                .unwrap();

            let mut session_id: Option<String> = None;
            while session_id.is_none() {
                let msg = ws.next().await.unwrap().unwrap();
                if let Message::Text(t) = msg {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    session_id = v
                        .get("params")
                        .and_then(|p| p.get("sessionId"))
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string());
                }
            }
            let sid = session_id.unwrap();

            // Fire a synchronous JS loop that holds V8 for JS_DURATION_MS.
            let code = format!(
                "var s=Date.now();while(Date.now()-s<{}){{}}'done'",
                JS_DURATION_MS
            );
            let eval = json!({
                "id": 2,
                "method": "Runtime.evaluate",
                "sessionId": sid,
                "params": {
                    "expression": code,
                    "awaitPromise": false,
                    "returnByValue": true
                }
            });
            ws.send(Message::Text(eval.to_string().into()))
                .await
                .unwrap();

            // Give the JS evaluation a moment to enter V8.
            tokio::time::sleep(Duration::from_millis(300)).await;

            // Issue the HTTP request from a separate OS thread so the
            // LocalSet isn't blocked by the synchronous TCP connect/read.
            let handle = std::thread::spawn(move || {
                let start = Instant::now();
                let mut stream = std::net::TcpStream::connect_timeout(
                    &format!("127.0.0.1:{}", port_clone)
                        .parse()
                        .unwrap(),
                    HTTP_TIMEOUT,
                )?;
                stream.set_read_timeout(Some(HTTP_TIMEOUT))?;

                let request = format!(
                    "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                    port_clone
                );
                stream.write_all(request.as_bytes())?;

                let mut response = String::new();
                stream.read_to_string(&mut response)?;
                let elapsed = start.elapsed();
                Ok::<(String, Duration), std::io::Error>((response, elapsed))
            });

            let result = handle.join().unwrap();
            let (response, elapsed) = result.unwrap();

            assert!(
                response.contains("webSocketDebuggerUrl"),
                "/json/version must return valid JSON with webSocketDebuggerUrl.\nResponse: {}",
                &response[..response.len().min(500)]
            );
            assert!(
                elapsed < Duration::from_secs(2),
                "/json/version response too slow during JS eval: {:?}",
                elapsed
            );
        })
        .await;
}
