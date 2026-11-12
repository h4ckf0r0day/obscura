//! Endpoint-derived client origins are accepted without trusting other local
//! applications. Additional origins require an explicit allowlist.

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
/// response head and any Content-Length body; a 101 answer has no body.
async fn raw_handshake(port: u16, origin: Option<&str>, path: &str) -> String {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(stream) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("control connection failed: {error}"),
            }
        }
    }).await.expect("CDP listener must become ready");
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

    // An upgrade keeps the connection open, so do not wait for EOF.
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.expect("handshake read");
            assert!(n > 0, "connection closed before a response head arrived");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = std::str::from_utf8(&buf[..end]).expect("UTF-8 response head");
                let length = head.lines().filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().expect("valid content length"))
                    .unwrap_or(0);
                if buf.len() >= end + 4 + length {
                    break;
                }
            }
        }
    })
    .await
    .expect("handshake response timed out");
    String::from_utf8_lossy(&buf).to_string()
}

fn start_server(port: u16) {
    tokio::task::spawn_local(async move {
        obscura_cdp::start_with_serve_options_and_limit(
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
        .await.expect("CDP server must start");
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

        let origin = format!("http://127.0.0.1:{port}");
        let response = raw_handshake(port, Some(&origin), "/devtools/browser").await;
        assert!(response.starts_with("HTTP/1.1 101"), "endpoint Origin: {response}");

        // Clients that omit Origin remain accepted.
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
            raw_handshake(port, Some(&origin), "/json/version"),
        )
        .await
        .expect("/json/version timed out");
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "/json/version with endpoint Origin must answer, got: {:?}",
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

        for origin in [
            "http://evil.example:8080".to_string(),
            format!("http://127.0.0.1:{}", port.wrapping_add(1)),
            format!("http://localhost:{port}"),
            format!("https://127.0.0.1:{port}"),
            "null".to_string(),
        ] {
            let response = raw_handshake(port, Some(&origin), "/devtools/browser").await;
            assert!(response.starts_with("HTTP/1.1 403"), "origin {origin}: {response}");
            assert!(response.contains("\"error\":\"request refused\""), "body: {response}");
            assert!(response.contains(&format!("OBSCURA_CDP_ALLOWED_ORIGINS={origin}")),
                "refusal must be actionable, body: {response}");
        }
    });
}

#[test]
fn allowed_origins_env_var_admits_wildcard_origins() {
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
    });
}

#[test]
fn allowed_origins_env_var_admits_only_listed_origins() {
    // SAFETY: set before starting runtime or server threads; nextest isolates tests.
    unsafe {
        std::env::set_var("OBSCURA_CDP_ALLOWED_ORIGINS", "http://tunnel.example:4242");
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let port = pick_port().await;
        start_server(port);

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
}
