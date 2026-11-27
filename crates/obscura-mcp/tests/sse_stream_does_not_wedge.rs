//! Regression test: an open SSE stream (`GET /mcp` with
//! `Accept: text/event-stream`) must not wedge the whole MCP HTTP server.
//!
//! The server serves connections sequentially on the single current-thread
//! runtime (the browser session is `!Send`). The SSE keep-alive is an infinite
//! ping loop, so if it is held inline it never returns and the `accept()` loop
//! never runs again — every later request hangs forever. The keep-alive carries
//! no browser state, so it must be detached and the connection handler must
//! return, leaving the accept loop free.

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
async fn open_sse_stream_does_not_block_other_requests() {
    let port = pick_free_port();
    let local = LocalSet::new();

    let server = local.spawn_local(async move {
        let _ = obscura_mcp::http::run("127.0.0.1".to_string(), port, None, None, false).await;
    });

    local
        .run_until(async {
            // Wait for the listener to bind.
            for _ in 0..40 {
                if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }

            // Connection A: open the SSE stream and read its headers so the
            // server-side handler has entered its keep-alive path. Keep it open.
            let mut sse = TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("MCP server did not come up");
            let get = b"GET /mcp HTTP/1.1\r\n\
                        Host: 127.0.0.1\r\n\
                        Accept: text/event-stream\r\n\
                        \r\n";
            sse.write_all(get).await.unwrap();
            sse.flush().await.unwrap();

            let mut hbuf = [0u8; 512];
            let hn = timeout(Duration::from_secs(2), sse.read(&mut hbuf))
                .await
                .expect("SSE headers read timed out")
                .expect("SSE headers read failed");
            let sse_head = String::from_utf8_lossy(&hbuf[..hn]).to_lowercase();
            assert!(
                sse_head.contains("text/event-stream"),
                "expected an SSE response on the GET stream, got:\n{sse_head}"
            );

            // Connection B: while A's SSE stream is still open, a second request
            // must still be accepted and answered promptly. Pre-fix this hangs
            // because the SSE loop never yields the accept loop.
            let mut stream = TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("second connection refused");
            let req = b"OPTIONS /mcp HTTP/1.1\r\n\
                        Host: 127.0.0.1\r\n\
                        Access-Control-Request-Method: POST\r\n\
                        \r\n";
            stream.write_all(req).await.unwrap();
            stream.flush().await.unwrap();

            let mut buf = [0u8; 1024];
            let n = timeout(Duration::from_secs(3), stream.read(&mut buf))
                .await
                .expect("second request timed out — the SSE stream wedged the server")
                .expect("read failed");
            let response = String::from_utf8_lossy(&buf[..n]).to_string();

            server.abort();
            drop(sse);

            assert!(
                response.starts_with("HTTP/1.1 204"),
                "expected the second request to be served (204), got:\n{response}"
            );
        })
        .await;
}
