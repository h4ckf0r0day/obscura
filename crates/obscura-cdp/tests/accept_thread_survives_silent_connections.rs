//! The accept thread must survive connections that never send a request head.
//!
//! The accept thread is one OS thread shared by every client. Before issue
//! #715 it classified each fresh connection with a *blocking* peek, so a
//! single connection that was accepted but never sent anything (speculative
//! browser preconnect, port probe, stalled client) parked the thread forever.
//! Every later connection then sat in the kernel backlog with no 101 and no
//! error until its own connect timeout: Playwright's `connectOverCDP` hangs
//! exactly like that while a raw WebSocket client appears to work — it only
//! works as long as it connects while no silent connection is parked.
//!
//! This test parks several silent connections and then requires that
//! 1. a real CDP WebSocket handshake plus a `Target.getTargets` round-trip
//!    still completes promptly, and
//! 2. the `/json/version` control plane — served by the same thread — stays
//!    reachable too.
//!
//! Run with `cargo nextest run -p obscura-cdp -E 'test(accept_thread_survives)'`.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const SILENT_CONNECTIONS: usize = 4;

async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

#[test]
fn accept_thread_survives_silent_connections() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let ws_port = pick_port().await;
        tokio::task::spawn_local(async move {
            let _ = obscura_cdp::start_with_serve_options_and_limit(
                ws_port,
                "127.0.0.1",
                None,
                false,
                None,
                false,
                None,
                true,
                128,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Park connections that connect and then never send a byte. They must
        // be held by the server without occupying its attention.
        let mut silent = Vec::new();
        for _ in 0..SILENT_CONNECTIONS {
            silent.push(
                tokio::net::TcpStream::connect(("127.0.0.1", ws_port))
                    .await
                    .expect("silent connection must open"),
            );
        }
        // Give the accept thread a moment to pick them up before the real
        // clients arrive, so the test cannot pass by racing the park.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 1. A real WebSocket client still gets its handshake and a CDP
        //    round-trip. Before the fix this hung until the timeout because
        //    the accept thread was parked inside a blocking peek.
        let url = format!("ws://127.0.0.1:{}/devtools/browser", ws_port);
        let cdp = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut ws, _) = connect_async(&url).await.expect("handshake");
            ws.send(Message::Text(
                json!({"id": 1, "method": "Target.getTargets"}).to_string().into(),
            ))
            .await
            .expect("send");
            loop {
                let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                    .await
                    .expect("per-message timeout")
                    .expect("ws closed")
                    .expect("ws error");
                if let Message::Text(t) = msg {
                    let v: serde_json::Value = serde_json::from_str(&t).expect("cdp json");
                    if v.get("id").and_then(|i| i.as_u64()) == Some(1) {
                        assert!(v["result"]["targetInfos"].is_array(), "getTargets reply: {v}");
                        return;
                    }
                }
            }
        })
        .await;
        assert!(cdp.is_ok(), "CDP handshake wedged behind silent connections");

        // 2. The /json control plane shares the accept thread and must stay
        //    reachable too.
        let mut http = tokio::net::TcpStream::connect(("127.0.0.1", ws_port))
            .await
            .expect("http connect");
        let req = format!(
            "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            ws_port
        );
        http.write_all(req.as_bytes()).await.expect("http write");
        let mut buf = vec![0u8; 2048];
        let body = tokio::time::timeout(Duration::from_secs(5), http.read(&mut buf))
            .await
            .expect("/json/version timed out behind silent connections")
            .expect("/json/version read");
        let body = String::from_utf8_lossy(&buf[..body]).to_string();
        assert!(
            body.starts_with("HTTP/1.1 200"),
            "/json/version must answer, got: {:?}",
            body.lines().next()
        );

        drop(silent);
    });
}
