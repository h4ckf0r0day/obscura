//! Issue #430 regression: concurrent CDP connections driving a subresource-heavy
//! page must not abort the process.
//!
//! Why this and not `concurrent_navigations` (data: URLs): the #430 abort needs
//! a page whose event loop is still busy when the navigation settle loop's
//! per-tick `tokio::time::timeout` around `run_event_loop` fires. A cancelled
//! `run_event_loop` future left an isolate entered on the shared LocalSet
//! thread; a second connection's `execute_script` then tripped V8's
//! `heap->isolate() == Isolate::TryGetCurrent()` check and `abort(3)`'d. `data:`
//! URLs settle instantly (no subresources, no busy loop), so they never drive
//! it. This test serves a local page with SLOW subresources plus a `setInterval`
//! so the settle loop keeps pumping, then drives it from several independent
//! connections at once.
//!
//! On the single-LocalSet server this aborted deterministically. The
//! thread-per-connection server confines each connection's isolates to their own
//! OS thread, so the abort cannot happen (V8's check is per-thread) and all
//! clients complete.
//!
//! Run with `cargo test -p obscura-cdp --test concurrent_connections_heavy_page
//! -- --nocapture`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const CLIENTS: u64 = 4;

async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Minimal HTTP fixture: a root page with two subresources that respond after a
/// delay (keeping the navigation settle loop pumping) and an inline
/// `setInterval` that keeps the event loop non-idle. Serves every connection it
/// accepts until the test ends.
///
/// `served` counts the requests it answers. The test asserts on it: without
/// that, pointing `page_url` at a dead port still passes (verified), because
/// nothing else here checks the heavy page ever loaded -- and a test whose
/// fixture silently drops out stops reproducing #430 while staying green.
async fn serve_heavy_fixture(listener: TcpListener, served: Arc<AtomicUsize>) {
    let body = "<!DOCTYPE html><html><head>\
        <script src=\"/slow.js\"></script>\
        </head><body><h1>heavy</h1><img src=\"/slow.png\">\
        <script>setInterval(function(){var x=0;for(var i=0;i<2000;i++){x+=i;}}, 3);</script>\
        </body></html>";
    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => return,
        };
        let served = served.clone();
        tokio::task::spawn_local(async move {
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            served.fetch_add(1, Ordering::Relaxed);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_string();

            // Slow subresources: respond after a delay so `active_requests` stays
            // > 0 through several 10ms settle ticks, which is what forces the
            // per-tick timeout to cancel `run_event_loop` mid-poll on the old
            // single-thread server.
            let (ctype, payload): (&str, Vec<u8>) = if path == "/slow.js" {
                tokio::time::sleep(Duration::from_millis(120)).await;
                ("application/javascript", b"void 0;".to_vec())
            } else if path == "/slow.png" {
                tokio::time::sleep(Duration::from_millis(120)).await;
                // 1x1 transparent PNG.
                (
                    "image/png",
                    vec![
                        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
                        0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
                        0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
                        0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
                        0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
                        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
                    ],
                )
            } else {
                ("text/html", body.as_bytes().to_vec())
            };

            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                ctype,
                payload.len()
            );
            let _ = sock.write_all(header.as_bytes()).await;
            let _ = sock.write_all(&payload).await;
            let _ = sock.flush().await;
        });
    }
}

/// One client: open its own CDP connection, create a target at the heavy page,
/// navigate, then repeatedly `Runtime.evaluate` an awaited promise. Returns Err
/// on protocol failure; a V8 abort would take the whole process down instead.
async fn one_client(ws_port: u16, page_url: String, id_base: u64) -> Result<(), String> {
    let url = format!("ws://127.0.0.1:{}/devtools/browser", ws_port);
    let (mut ws, _) = connect_async(&url).await.map_err(|e| e.to_string())?;

    let create = json!({
        "id": id_base,
        "method": "Target.createTarget",
        "params": {"url": page_url},
    });
    ws.send(Message::Text(create.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;

    let mut session_id: Option<String> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while session_id.is_none() {
        if tokio::time::Instant::now() >= deadline {
            return Err("timeout waiting for sessionId".to_string());
        }
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .map_err(|_| "timeout waiting for createTarget".to_string())?
            .ok_or("ws closed")?
            .map_err(|e| e.to_string())?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            _ => continue,
        };
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(s) = v
            .get("params")
            .and_then(|p| p.get("sessionId"))
            .and_then(|s| s.as_str())
        {
            session_id = Some(s.to_string());
        }
    }
    let sid = session_id.unwrap();

    // A few awaited evaluations: each drives resolve_promises_until while the
    // page's setInterval keeps the loop busy — the settle/eval cancellation path.
    for k in 0..3u64 {
        let eval = json!({
            "id": id_base + 100 + k,
            "method": "Runtime.evaluate",
            "sessionId": sid,
            "params": {
                "expression": "new Promise(r => setTimeout(() => r(1 + 1), 40))",
                "awaitPromise": true,
                "returnByValue": true,
            },
        });
        ws.send(Message::Text(eval.to_string().into()))
            .await
            .map_err(|e| e.to_string())?;

        let d = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if tokio::time::Instant::now() >= d {
                return Err(format!("timeout waiting for evaluate {}", k));
            }
            let remaining = d - tokio::time::Instant::now();
            let msg = tokio::time::timeout(remaining, ws.next())
                .await
                .map_err(|_| "timeout".to_string())?
                .ok_or("ws closed mid-evaluate")?
                .map_err(|e| e.to_string())?;
            let text = match msg {
                Message::Text(t) => t.to_string(),
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            if v.get("id").and_then(|x| x.as_u64()) == Some(id_base + 100 + k) {
                break;
            }
        }
    }

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_connections_heavy_page_do_not_abort_v8() {
    let ws_port = pick_port().await;
    // Bind the fixture listener up front so we can hand its port to the clients.
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_port = fixture.local_addr().unwrap().port();
    let page_url = format!("http://127.0.0.1:{}/", fixture_port);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Fixture HTTP server.
            let served = Arc::new(AtomicUsize::new(0));
            tokio::task::spawn_local(serve_heavy_fixture(fixture, served.clone()));

            // CDP server. allow_private_network so it may fetch 127.0.0.1.
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start_with_full_serve_options(
                    ws_port,
                    "127.0.0.1",
                    None,
                    false,
                    None,
                    false,
                    None,
                    true,
                )
                .await;
            });
            tokio::time::sleep(Duration::from_millis(200)).await;

            let mut handles = Vec::new();
            for i in 0..CLIENTS {
                let url = page_url.clone();
                let id_base = (i + 1) * 1000;
                handles.push(tokio::task::spawn_local(async move {
                    one_client(ws_port, url, id_base).await
                }));
            }

            let mut ok = 0usize;
            let mut errors = Vec::new();
            for (i, h) in handles.into_iter().enumerate() {
                match h.await {
                    Ok(Ok(())) => ok += 1,
                    Ok(Err(e)) => errors.push(format!("client {}: {}", i, e)),
                    Err(e) => errors.push(format!("client {} join: {}", i, e)),
                }
            }
            assert!(
                errors.is_empty(),
                "clients failed (a V8 abort would instead kill the process): {:#?}",
                errors
            );
            assert_eq!(ok as u64, CLIENTS, "every concurrent client must complete");

            // The fixture must actually have served each client the page and
            // its slow script -- that is what keeps the settle loop pumping and
            // makes this a #430 repro at all. Without this assertion, pointing
            // `page_url` at a dead port still passes (verified), and the test
            // silently degrades into the `data:`-URL one it was written to
            // replace. Two per client, not three: the engine does not fetch the
            // `<img>`, so only the document and /slow.js are requested.
            let hits = served.load(Ordering::Relaxed);
            assert!(
                hits >= CLIENTS as usize * 2,
                "fixture served {} requests, expected at least {} ({} clients x document \
                 + /slow.js): the heavy page never loaded, so this run proves nothing",
                hits,
                CLIENTS as usize * 2,
                CLIENTS
            );
        })
        .await;
}
