//! SSRF regression tests for the stealth (`wreq`) HTTP client.
//!
//! The `--stealth` path used to skip the SSRF guard that PR#279 added to the
//! reqwest client — both on the initial URL and on redirect hops. These tests
//! pin the guard into `StealthHttpClient::fetch`.
//!
//! Run with: `cargo test -p obscura-net --features stealth --test stealth_ssrf`
#![cfg(feature = "stealth")]

use std::sync::Arc;

use obscura_net::{CookieJar, StealthHttpClient};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

fn client(allow_private_network: bool) -> StealthHttpClient {
    StealthHttpClient::with_proxy_and_network(Arc::new(CookieJar::new()), None, allow_private_network)
}

// The guard must reject internal targets on the INITIAL URL before any
// connection is attempted. No server is involved: validate_url runs first.
#[tokio::test]
async fn stealth_rejects_internal_initial_urls() {
    let c = client(false);
    for raw in [
        "http://127.0.0.1:9/",
        "http://169.254.169.254/", // cloud metadata endpoint
        "http://[::1]:9/",
        "http://10.0.0.1/",
        "http://192.168.0.1/",
        "http://localhost:9/",
    ] {
        let err = c
            .fetch(&Url::parse(raw).unwrap())
            .await
            .expect_err(&format!("{raw} must be rejected by the SSRF guard in the stealth path"));
        let msg = err.to_string();
        assert!(
            msg.contains("internal") || msg.contains("localhost"),
            "{raw} rejected, but not by the guard — got: {msg}"
        );
    }
}

// The opt-in must still work in the stealth path, matching the reqwest client.
#[tokio::test]
async fn stealth_allows_internal_with_opt_in() {
    // allow_private_network = true => validate_url passes 127.0.0.1. We don't
    // run a server here; we only assert the guard does not pre-emptively reject,
    // i.e. the failure (if any) is a connection error, not a guard error.
    let c = client(true);
    let err = c.fetch(&Url::parse("http://127.0.0.1:9/").unwrap()).await.err();
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            !(msg.contains("private/internal") || msg.contains("localhost domain")),
            "opt-in should bypass the guard, but got a guard rejection: {msg}"
        );
    }
}

// The guard must run on every REDIRECT hop, not just the initial URL. We stand
// up a reachable loopback server (allow_private_network = true so we can connect
// to it) that 302-redirects to a guard-rejected target. The fetch must fail with
// the guard's message — proving validate_url ran on the redirect target before
// any further connection. We use a non-http scheme as the redirect target
// because the scheme check is enforced even with the private-network opt-in,
// whereas an internal *IP* target can't be tested this way (reaching the local
// 302 server already requires the opt-in, which would also permit that IP).
#[tokio::test]
async fn stealth_validates_redirect_target() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    // One-shot 302 server.
    let server = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            // Drain the request head so the client isn't left writing.
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let resp = "HTTP/1.1 302 Found\r\n\
                        Location: ftp://internal.invalid/secret\r\n\
                        Content-Length: 0\r\n\
                        Connection: close\r\n\r\n";
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });

    let c = client(true); // opt-in needed only to reach the loopback 302 server
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    let err = c
        .fetch(&url)
        .await
        .expect_err("redirect to a forbidden scheme must be rejected by the guard on the hop");
    let msg = err.to_string();
    assert!(
        msg.contains("Forbidden URL scheme") && msg.contains("ftp"),
        "redirect target should be rejected by validate_url on the hop — got: {msg}"
    );

    let _ = server.await;
}
