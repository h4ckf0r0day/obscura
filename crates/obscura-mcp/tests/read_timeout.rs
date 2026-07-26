//! Regression test: the MCP HTTP server must not let a slow/stalled client hold
//! a connection open forever. Connections are served sequentially, so one
//! slowloris client would otherwise block every other MCP client. The request
//! read (line + headers + body) is bounded by a deadline
//! (`OBSCURA_MCP_READ_TIMEOUT_MS`).

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
async fn slow_client_is_disconnected_by_read_timeout() {
    std::env::set_var("OBSCURA_MCP_READ_TIMEOUT_MS", "300");
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

            // Partial request: request line + one header, then stall — never send
            // the terminating blank line. Pre-fix the server blocks on the header
            // read forever; with the deadline it gives up after ~300ms.
            stream
                .write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();

            let mut buf = [0u8; 1024];
            let n = timeout(Duration::from_secs(3), stream.read(&mut buf))
                .await
                .expect("server did not time out the slow client")
                .expect("read failed");

            server.abort();

            let resp = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(
                n == 0 || resp.starts_with("HTTP/1.1 408"),
                "expected a 408 or a closed connection, got ({n} bytes):\n{resp}"
            );
        })
        .await;
}
