//! The CDP endpoint must accept the handshakes that standard WebSocket
//! clients actually send (issue #1495).
//!
//! RFC 6455 browsers attach an `Origin` header to every cross-site
//! WebSocket connect, and the mainstream CDP clients (Puppeteer, Playwright's
//! `connect_over_cdp`, `chrome-remote-interface`, Python `websocket-client`)
//! send a normal loopback `Origin` on every connect. Before the fix any
//! request with an `Origin` header was refused with
//! `403 {"error":"request refused"}`, so every one of those clients failed
//! while only clients that suppress the header could connect.
//!
//! The policy now matches Chrome's DevTools handler: loopback origins and the
//! server's own address are accepted, native clients without `Origin` are
//! accepted, and a non-loopback origin is refused with an actionable
//! `OBSCURA_CDP_ALLOWED_ORIGINS` hint unless the allowlist admits it.
//!
//! Run with `cargo nextest run -p obscura-cdp -E 'test(cdp_ws_origin_policy)'`.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Send one handshake request with a caller-chosen `Origin` and return the
/// raw response head. The status line and any JSON body come back together
/// for refusals; a 101 answer has no body.
async fn raw_handshake(port: u16, origin: Option<&str>, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("control connection must open");
    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Upgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n"
    );
    if let Some(origin) = origin {
        request.push_str(&format!("Origin: {origin}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.expect("handshake write");

    // The refusal carries `Connection: close`, the 101 keeps the socket open,
    // so read until the end of the response head rather than until EOF.
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.expect("handshake read");
            assert!(n > 0, "connection closed before a response head arrived");
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
    })
    .await
    .expect("handshake response timed out");
    String::from_utf8_lossy(&buf).to_string()
}

fn start_server(port: u16) {
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
            128,
        )
        .await;
    });
}

#[test]
fn standard_client_origins_complete_the_websocket_handshake() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let port = pick_port().await;
        start_server(port);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // What the mainstream CDP clients send: an Origin derived from the
        // ws://127.0.0.1:<port> endpoint, or from a localhost variant of it.
        // Before the fix each of these was a 403.
        for origin in [
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
            "http://127.0.0.1:5173".to_string(),
        ] {
            let response = tokio::time::timeout(
                Duration::from_secs(5),
                raw_handshake(port, Some(&origin), "/devtools/browser"),
            )
            .await
            .expect("handshake timed out");
            assert!(
                response.starts_with("HTTP/1.1 101"),
                "origin {origin} must complete the upgrade, got: {:?}",
                response.lines().next()
            );
        }

        // Native clients never send Origin and were never affected.
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            raw_handshake(port, None, "/devtools/browser"),
        )
        .await
        .expect("handshake timed out");
        assert!(response.starts_with("HTTP/1.1 101"), "no-Origin connect: {response}");

        // The /json discovery endpoints share the accept thread and the same
        // origin policy; a browser tab probing them sends an Origin too.
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            raw_handshake(port, Some("http://localhost:5173"), "/json/version"),
        )
        .await
        .expect("/json/version timed out");
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "/json/version with loopback Origin must answer, got: {:?}",
            response.lines().next()
        );
    });
}

#[test]
fn cross_origin_handshake_is_refused_with_an_actionable_message() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let port = pick_port().await;
        start_server(port);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            raw_handshake(port, Some("http://evil.example:8080"), "/devtools/browser"),
        )
        .await
        .expect("handshake timed out");
        assert!(
            response.starts_with("HTTP/1.1 403"),
            "cross-origin connect must be refused, got: {:?}",
            response.lines().next()
        );
        assert!(response.contains("\"error\":\"request refused\""), "body: {response}");
        // The refusal names the knob, like Chrome names
        // --remote-allow-origins in its 403 body.
        assert!(
            response.contains("OBSCURA_CDP_ALLOWED_ORIGINS=http://evil.example:8080"),
            "refusal must be actionable, body: {response}"
        );
    });
}

#[test]
fn allowed_origins_env_var_admits_listed_and_wildcard_origins() {
    // SAFETY: nextest runs every test in its own process, and each server
    // reads the variable once, when its accept thread starts.
    unsafe {
        std::env::set_var("OBSCURA_CDP_ALLOWED_ORIGINS", "*");
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        // Wildcard server: even a non-loopback origin now upgrades.
        let wildcard_port = pick_port().await;
        start_server(wildcard_port);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            raw_handshake(wildcard_port, Some("http://evil.example:8080"), "/devtools/browser"),
        )
        .await
        .expect("handshake timed out");
        assert!(
            response.starts_with("HTTP/1.1 101"),
            "wildcard must admit any origin, got: {:?}",
            response.lines().next()
        );

        // Explicit-list server on a second port: only listed origins pass.
        unsafe {
            std::env::set_var("OBSCURA_CDP_ALLOWED_ORIGINS", "http://tunnel.example:4242");
        }
        let port = pick_port().await;
        start_server(port);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            raw_handshake(port, Some("http://tunnel.example:4242"), "/devtools/browser"),
        )
        .await
        .expect("handshake timed out");
        assert!(
            response.starts_with("HTTP/1.1 101"),
            "allowlisted origin must complete the upgrade, got: {:?}",
            response.lines().next()
        );

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            raw_handshake(port, Some("http://unlisted.example:1"), "/devtools/browser"),
        )
        .await
        .expect("handshake timed out");
        assert!(response.starts_with("HTTP/1.1 403"), "unlisted origin: {response}");
    });
    // SAFETY: as above; restored so a same-process cargo test run is clean.
    unsafe {
        std::env::remove_var("OBSCURA_CDP_ALLOWED_ORIGINS");
    }
}
