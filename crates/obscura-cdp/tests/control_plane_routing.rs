//! The control plane routed on a substring of the whole request
//! head.
//!
//! `accept_dispatch` used `head.contains("/json/list")` and friends, which
//! matches anywhere — including inside a *header value*. So a WebSocket upgrade
//! for `/devtools/page/page-1` that happened to carry a header containing
//! `/json/list` was served as the JSON target list instead of reaching the
//! WebSocket path.
//!
//! Nothing exploitable was found: the JSON endpoints expose no more than they
//! already do to whoever can reach the port. The defect is the shape — routing
//! decisions taken on attacker-influenced bytes from anywhere in the head — and
//! it is a two-line fix.
//!
//! Run with `cargo test -p obscura-cdp --test control_plane_routing`.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

async fn spawn_server(port: u16) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let _ = obscura_cdp::start_with_host(port, "127.0.0.1", None, false, None, None).await;
        });
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("CDP server on port {port} never came up");
}

async fn exchange(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut buf = vec![0u8; 8192];
    let n = timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("read timed out")
        .expect("read failed");
    String::from_utf8_lossy(&buf[..n]).to_string()
}

/// The defect: a header value must not decide the route.
#[tokio::test]
async fn a_header_value_cannot_choose_the_route() {
    let port = pick_port().await;
    spawn_server(port).await;

    let response = exchange(
        port,
        &format!(
            "GET /devtools/page/page-1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             X-Thing: /json/list\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ),
    )
    .await;

    assert!(
        !response.contains("webSocketDebuggerUrl"),
        "a header value routed a WebSocket upgrade to the JSON endpoint:\n{response}"
    );
}

/// Regression: the real endpoints still route.
#[tokio::test]
async fn the_json_endpoints_still_route() {
    let port = pick_port().await;
    spawn_server(port).await;

    for (path, needle) in [
        ("/json/version", "webSocketDebuggerUrl"),
        ("/json/list", "webSocketDebuggerUrl"),
        ("/json", "webSocketDebuggerUrl"),
        ("/json/protocol", "version"),
    ] {
        let response = exchange(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        )
        .await;
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "{path} should be served, got:\n{response}"
        );
        assert!(response.contains(needle), "{path} body looks wrong:\n{response}");
    }
}

/// A query string and a trailing slash must not defeat the match.
#[tokio::test]
async fn query_strings_and_trailing_slashes_still_match() {
    let port = pick_port().await;
    spawn_server(port).await;

    for path in ["/json/version?x=1", "/json/list/"] {
        let response = exchange(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        )
        .await;
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "{path} should still route, got:\n{response}"
        );
    }
}

/// A near-miss must not match: prefix routing was part of the original problem.
#[tokio::test]
async fn a_near_miss_path_is_not_a_json_endpoint() {
    let port = pick_port().await;
    spawn_server(port).await;

    let response = exchange(
        port,
        &format!("GET /json/versionify HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    )
    .await;
    assert!(
        !response.contains("webSocketDebuggerUrl"),
        "/json/versionify is not /json/version:\n{response}"
    );
}
