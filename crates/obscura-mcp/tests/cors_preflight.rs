//! Browser callers are denied unless the operator explicitly allowlists their
//! Origin. Native MCP clients send no Origin and are unaffected.

use std::net::TcpListener as StdListener;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::LocalSet;
use tokio::time::{sleep, timeout};

fn pick_free_port() -> u16 {
    let l = StdListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

#[tokio::test(flavor = "current_thread")]
async fn browser_preflight_is_denied_without_an_origin_allowlist() {
    let port = pick_free_port();
    let local = LocalSet::new();

    // Spawn the MCP HTTP server. It loops forever; we abort the task at the
    // end of the test. `current_thread` + LocalSet is required because the
    // browser state is `!Send` (Page holds V8 handles).
    let server = local.spawn_local(async move {
        let _ = obscura_mcp::http::run("127.0.0.1".to_string(), port, None, None, false).await;
    });

    local.run_until(async {
        // Wait for the listener to bind.
        for _ in 0..40 {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }

        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("MCP server did not come up");
        let req = b"OPTIONS /mcp HTTP/1.1\r\n\
                    Host: 127.0.0.1\r\n\
                    Origin: https://dashboard.example.com\r\n\
                    Access-Control-Request-Method: POST\r\n\
                    Access-Control-Request-Headers: Content-Type, mcp-protocol-version, Authorization\r\n\
                    \r\n";
        stream.write_all(req).await.unwrap();
        stream.flush().await.unwrap();

        let mut buf = [0u8; 4096];
        let n = timeout(Duration::from_secs(2), stream.read(&mut buf))
            .await
            .expect("read timed out")
            .expect("read failed");
        let response = String::from_utf8_lossy(&buf[..n]).to_string();

        server.abort();

        assert!(response.starts_with("HTTP/1.1 403"), "expected 403, got:\n{response}");
        let lc = response.to_lowercase();
        assert!(!lc.contains("access-control-allow-origin: *"));
    })
    .await;
}

/// Regression test for the unbounded `Content-Length` allocation: a POST that
/// advertises a huge body must be rejected with 413 *before* the server tries
/// to allocate `vec![0u8; len]`, rather than committing gigabytes of RAM and
/// OOM-ing the process (unauthenticated DoS).
#[tokio::test(flavor = "current_thread")]
async fn oversized_content_length_is_rejected() {
    let port = pick_free_port();
    let local = LocalSet::new();

    let server = local.spawn_local(async move {
        let _ = obscura_mcp::http::run("127.0.0.1".to_string(), port, None, None, false).await;
    });

    local
        .run_until(async {
            for _ in 0..40 {
                if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }

            let mut stream = TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("MCP server did not come up");
            // 8 GiB advertised, zero body sent. Pre-fix the server allocates 8 GiB.
            let req = b"POST /mcp HTTP/1.1\r\n\
                        Host: 127.0.0.1\r\n\
                        Content-Type: application/json\r\n\
                        Content-Length: 8589934592\r\n\
                        \r\n";
            stream.write_all(req).await.unwrap();
            stream.flush().await.unwrap();

            let mut buf = [0u8; 1024];
            let n = timeout(Duration::from_secs(2), stream.read(&mut buf))
                .await
                .expect("read timed out")
                .expect("read failed");
            let response = String::from_utf8_lossy(&buf[..n]).to_string();

            server.abort();

            assert!(
                response.starts_with("HTTP/1.1 413"),
                "expected 413 Payload Too Large, got:\n{response}"
            );
        })
        .await;
}
