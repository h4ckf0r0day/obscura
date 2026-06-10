//! Regression test for issue #175: the MCP HTTP server's OPTIONS preflight
//! response must list every header a browser MCP client may send, including
//! `mcp-protocol-version` (from the MCP spec) and `Authorization` /
//! `X-API-Key` (common in hosted deployments). Otherwise the browser blocks
//! the actual request with a CORS error.

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
async fn options_preflight_lists_required_browser_headers() {
    let port = pick_free_port();
    let local = LocalSet::new();

    // Spawn the MCP HTTP server. It loops forever; we abort the task at the
    // end of the test. `current_thread` + LocalSet is required because the
    // browser state is `!Send` (Page holds V8 handles).
    let server = local.spawn_local(async move {
        let _ = obscura_mcp::http::run("127.0.0.1".to_string(), port, None, None, false, false).await;
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

        assert!(
            response.starts_with("HTTP/1.1 204"),
            "expected 204 No Content preflight, got:\n{response}"
        );
        let lc = response.to_lowercase();
        assert!(
            lc.contains("access-control-allow-headers:"),
            "preflight must include Access-Control-Allow-Headers; got:\n{response}"
        );
        assert!(
            lc.contains("mcp-protocol-version"),
            "ACAH must list mcp-protocol-version (per MCP spec); got:\n{response}"
        );
        assert!(
            lc.contains("authorization"),
            "ACAH must list Authorization for hosted deployments; got:\n{response}"
        );
        assert!(
            lc.contains("x-api-key"),
            "ACAH must list X-API-Key for hosted deployments; got:\n{response}"
        );
    })
    .await;
}

// MCP-02 / MCP-03: the HTTP transport must reject a cross-origin browser POST,
// an oversized Content-Length, and a DNS-rebinding Host header.
#[tokio::test(flavor = "current_thread")]
async fn http_transport_rejects_cross_origin_oversized_and_rebinding() {
    let port = pick_free_port();
    let local = LocalSet::new();
    let server = local.spawn_local(async move {
        let _ = obscura_mcp::http::run("127.0.0.1".to_string(), port, None, None, false, false).await;
    });

    local
        .run_until(async {
            for _ in 0..40 {
                if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }

            async fn send(port: u16, req: &[u8]) -> String {
                let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                s.write_all(req).await.unwrap();
                s.flush().await.unwrap();
                let mut buf = [0u8; 1024];
                let n = timeout(Duration::from_secs(2), s.read(&mut buf))
                    .await
                    .expect("read timed out")
                    .expect("read failed");
                String::from_utf8_lossy(&buf[..n]).to_string()
            }

            // 1. Cross-origin POST (browser page) -> 403.
            let r = send(
                port,
                b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://evil.example\r\n\
                  Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            )
            .await;
            assert!(r.starts_with("HTTP/1.1 403"), "cross-origin POST must be 403; got:\n{r}");

            // 2. Oversized Content-Length -> 413 (no body actually sent).
            let huge = format!(
                "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
                17 * 1024 * 1024
            );
            let r = send(port, huge.as_bytes()).await;
            assert!(r.starts_with("HTTP/1.1 413"), "oversized body must be 413; got:\n{r}");

            // 3. DNS-rebinding Host (attacker domain) -> 403.
            let r = send(
                port,
                b"POST /mcp HTTP/1.1\r\nHost: attacker.example\r\n\
                  Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            )
            .await;
            assert!(r.starts_with("HTTP/1.1 403"), "rebinding Host must be 403; got:\n{r}");

            server.abort();
        })
        .await;
}
