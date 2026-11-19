use std::sync::Arc;
use std::time::Duration;

use obscura_net::{CookieJar, ObscuraHttpClient};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

// nextest runs each test in a separate process. Restore the environment before
// starting I/O so the client must retain the setting read at construction.
fn with_http_timeout<T>(value: &str, build: impl FnOnce() -> T) -> T {
    let previous = std::env::var_os("OBSCURA_HTTP_TIMEOUT_MS");
    std::env::set_var("OBSCURA_HTTP_TIMEOUT_MS", value);
    let client = build();
    match previous {
        Some(value) => std::env::set_var("OBSCURA_HTTP_TIMEOUT_MS", value),
        None => std::env::remove_var("OBSCURA_HTTP_TIMEOUT_MS"),
    }
    client
}

async fn slow_response(delay_body: bool) -> (Url, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).await.unwrap();
            if count == 0 { return; }
            request.extend_from_slice(&buffer[..count]);
        }
        let headers = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n";
        if delay_body && stream.write_all(headers).await.is_err() { return; }
        tokio::time::sleep(Duration::from_millis(150)).await;
        if !delay_body && stream.write_all(headers).await.is_err() { return; }
        let _ = stream.write_all(b"ok").await;
    });
    (url, server)
}

fn assert_response(
    result: Result<obscura_net::Response, obscura_net::ObscuraNetError>,
    value: &str,
) {
    if value == "25" {
        let error = result.unwrap_err().to_string();
        assert!(error.contains("timed out"), "unexpected error: {error}");
    } else {
        let response = result.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ok");
    }
}

#[tokio::test]
async fn regular_http_timeout_uses_environment_for_headers_and_body() {
    for value in ["25", "1000", "0"] {
        for delay_body in [false, true] {
            let client = with_http_timeout(value, || {
                ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true)
            });
            let (url, server) = slow_response(delay_body).await;
            let result = tokio::time::timeout(Duration::from_secs(5), client.fetch(&url))
                .await.expect("HTTP client ignored its configured timeout");
            server.abort();
            let _ = server.await;
            assert_response(result, value);
            assert_eq!(client.active_requests(), 0);
        }
    }
}

#[cfg(feature = "stealth")]
#[tokio::test]
async fn stealth_http_timeout_uses_environment_for_headers_and_body() {
    for value in ["25", "1000", "0"] {
        for delay_body in [false, true] {
            let client = with_http_timeout(value, || {
                obscura_net::StealthHttpClient::with_proxy(Arc::new(CookieJar::new()), None, true)
            });
            let (url, server) = slow_response(delay_body).await;
            let result = tokio::time::timeout(Duration::from_secs(5), client.fetch(&url))
                .await.expect("stealth HTTP client ignored its configured timeout");
            server.abort();
            let _ = server.await;
            assert_response(result, value);
            assert_eq!(client.active_requests(), 0);
        }
    }
}
