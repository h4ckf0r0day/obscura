//! `--max-connections` bounds the thread-per-connection server.
//!
//! Each CDP connection owns an OS thread and its pages' V8 isolates, so without
//! a cap a client can grow the server's thread count and memory without limit.
//! The cap must do three things, and this test pins all three:
//!
//! 1. connections up to the limit are accepted and usable;
//! 2. the one past the limit is refused with an explicit `503` carrying
//!    `X-Obscura-Reason: max-connections`, not dropped with a bare reset;
//! 3. closing a connection frees its slot, so the server recovers rather than
//!    wedging shut once it has ever been full.
//!
//! (3) is the one that actually bites: a slot leaked on any connection-teardown
//! path would only show up as a server that stops accepting after N lifetime
//! connections, which no other test would catch.
//!
//! Run with `cargo nextest run -p obscura-cdp -E 'test(max_connections_cap)'`.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const LIMIT: usize = 2;

async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// A CDP connection that is actually driven, not just opened: `Target.getTargets`
/// round-trips through the connection's own processor, proving the connection
/// holds a live slot rather than a socket the server has forgotten about.
async fn open_and_use(
    ws_port: u16,
    id: u64,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, String>
{
    let url = format!("ws://127.0.0.1:{}/devtools/browser", ws_port);
    let (mut ws, _) = connect_async(&url).await.map_err(|e| e.to_string())?;
    ws.send(Message::Text(
        json!({"id": id, "method": "Target.getTargets"}).to_string().into(),
    ))
    .await
    .map_err(|e| e.to_string())?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("timeout waiting for Target.getTargets reply".into());
        }
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .map_err(|_| "timeout".to_string())?
            .ok_or("ws closed")?
            .map_err(|e| e.to_string())?;
        if let Message::Text(t) = msg {
            let v: serde_json::Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return Ok(ws);
            }
        }
    }
}

/// Raw handshake, returning the server's status line. Deliberately not
/// `connect_async`: a refusal is an HTTP response, and we want to read it
/// rather than have the websocket client turn it into an opaque error.
async fn raw_handshake_status(ws_port: u16) -> String {
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", ws_port))
        .await
        .expect("connect");
    let req = format!(
        "GET /devtools/browser HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
         Upgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        ws_port
    );
    sock.write_all(req.as_bytes()).await.expect("write");
    let mut buf = vec![0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf))
        .await
        .expect("read timed out")
        .expect("read");
    String::from_utf8_lossy(&buf[..n]).to_string()
}

#[test]
fn max_connections_refuses_then_recovers() {
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
                LIMIT,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(300)).await;

        // 1. Fill the server to its limit.
        let mut held = Vec::new();
        for i in 0..LIMIT {
            held.push(
                open_and_use(ws_port, 100 + i as u64)
                    .await
                    .unwrap_or_else(|e| panic!("connection {} within the limit must be accepted: {}", i, e)),
            );
        }

        // 2. One past the limit is refused, and says why.
        let refused = raw_handshake_status(ws_port).await;
        assert!(
            refused.starts_with("HTTP/1.1 503"),
            "over-limit connection must be refused with 503, got: {:?}",
            refused.lines().next()
        );
        assert!(
            refused.contains("X-Obscura-Reason: max-connections"),
            "refusal must name the reason so a client can tell it apart from a crash: {:?}",
            refused
        );

        // 3. Free one slot and confirm the server accepts again. Closing is
        //    asynchronous (the connection thread unwinds and drops its guard),
        //    so poll rather than assume an instant handover.
        let mut freed = held.pop().unwrap();
        let _ = freed.close(None).await;
        drop(freed);

        let mut reconnected = None;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Ok(ws) = open_and_use(ws_port, 900).await {
                reconnected = Some(ws);
                break;
            }
        }
        assert!(
            reconnected.is_some(),
            "a closed connection must release its slot; the server is wedged at the cap"
        );
    });
}
