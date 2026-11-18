//! `Network.emulateNetworkConditions`: offline, latency and throughput apply to
//! the page that set them, for navigations and for page-script `fetch()`.

use std::time::{Duration, Instant};

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::{CdpRequest, CdpResponse};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// `/` is a small HTML page and `/big` is 20_000 bytes of text.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let (content_type, body) = if request.starts_with("GET /big ") {
                    ("text/plain", "x".repeat(20_000))
                } else {
                    ("text/html", "<html><body><h1>Main heading</h1></body></html>".to_string())
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

async fn raw(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> CdpResponse {
    dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session.map(str::to_string),
        },
        ctx,
    )
    .await
}

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Value {
    let resp = raw(ctx, id, method, params, session).await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

/// Opens a page the way Puppeteer and Playwright do, rather than inserting a
/// session for a hand-made page, and returns `(targetId, sessionId)`.
async fn created_and_attached(ctx: &mut CdpContext) -> (String, String) {
    let created = cdp(ctx, 900, "Target.createTarget", json!({"url": "about:blank"}), None).await;
    let target_id = created["targetId"].as_str().expect("no targetId").to_string();
    let attached = cdp(
        ctx,
        901,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )
    .await;
    let session = attached["sessionId"].as_str().expect("no sessionId").to_string();
    (target_id, session)
}

/// A navigation that was refused: either a protocol error or an `errorText`.
fn navigation_failure(resp: &CdpResponse) -> Option<String> {
    if let Some(error) = &resp.error {
        return Some(format!("{error:?}"));
    }
    resp.result
        .as_ref()
        .and_then(|result| result["errorText"].as_str())
        .map(str::to_string)
}

async fn navigate_ok(ctx: &mut CdpContext, id: u64, url: &str, session: &str) {
    let resp = raw(ctx, id, "Page.navigate", json!({"url": url}), Some(session)).await;
    assert_eq!(navigation_failure(&resp), None, "navigation to {url} failed");
}

async fn emulate(ctx: &mut CdpContext, id: u64, params: Value, session: &str) {
    cdp(ctx, id, "Network.emulateNetworkConditions", params, Some(session)).await;
}

async fn evaluate_promise(ctx: &mut CdpContext, id: u64, expression: &str, session: &str) -> Value {
    let result = cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
        Some(session),
    )
    .await;
    result["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn offline_fails_navigation_and_script_fetch() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    navigate_ok(&mut ctx, 1, &url, &session).await;

    emulate(
        &mut ctx,
        2,
        json!({"offline": true, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1}),
        &session,
    )
    .await;

    let resp = raw(&mut ctx, 3, "Page.navigate", json!({"url": url}), Some(&session)).await;
    let failure = navigation_failure(&resp).expect("offline navigation succeeded");
    assert!(failure.contains("ERR_INTERNET_DISCONNECTED"), "{failure}");

    let value = evaluate_promise(
        &mut ctx,
        4,
        "fetch('/big').then(() => 'ok', e => 'failed')",
        &session,
    )
    .await;
    assert_eq!(value, "failed");
}

#[tokio::test(flavor = "current_thread")]
async fn latency_delays_each_request() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    emulate(
        &mut ctx,
        1,
        json!({"offline": false, "latency": 300, "downloadThroughput": -1, "uploadThroughput": -1}),
        &session,
    )
    .await;

    let started = Instant::now();
    navigate_ok(&mut ctx, 2, &url, &session).await;
    assert!(started.elapsed() >= Duration::from_millis(300), "{:?}", started.elapsed());
}

#[tokio::test(flavor = "current_thread")]
async fn download_throughput_limits_body_reads() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    navigate_ok(&mut ctx, 1, &url, &session).await;
    emulate(
        &mut ctx,
        2,
        json!({"offline": false, "latency": 0, "downloadThroughput": 40_000, "uploadThroughput": -1}),
        &session,
    )
    .await;

    let started = Instant::now();
    let value = evaluate_promise(
        &mut ctx,
        3,
        "fetch('/big').then(r => r.text()).then(t => t.length)",
        &session,
    )
    .await;
    assert_eq!(value.as_f64(), Some(20_000.0));
    assert!(started.elapsed() >= Duration::from_millis(450), "{:?}", started.elapsed());
}

#[tokio::test(flavor = "current_thread")]
async fn chrome_mcp_reset_restores_normal_requests() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    emulate(
        &mut ctx,
        1,
        json!({"offline": true, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1}),
        &session,
    )
    .await;
    let resp = raw(&mut ctx, 2, "Page.navigate", json!({"url": url}), Some(&session)).await;
    assert!(navigation_failure(&resp).is_some(), "offline navigation succeeded");

    // The exact payload better-chrome-mcp sends to clear emulation.
    emulate(
        &mut ctx,
        3,
        json!({"connectionType":"none","downloadThroughput":-1,"latency":0,"offline":false,"uploadThroughput":-1}),
        &session,
    )
    .await;
    navigate_ok(&mut ctx, 4, &url, &session).await;
}

#[tokio::test(flavor = "current_thread")]
async fn conditions_are_per_page() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let (_, page_a) = created_and_attached(&mut ctx).await;
    let (_, page_b) = created_and_attached(&mut ctx).await;
    assert_ne!(page_a, page_b);

    emulate(
        &mut ctx,
        1,
        json!({"offline": true, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1}),
        &page_a,
    )
    .await;

    navigate_ok(&mut ctx, 2, &url, &page_b).await;
    let resp = raw(&mut ctx, 3, "Page.navigate", json!({"url": url}), Some(&page_a)).await;
    assert!(navigation_failure(&resp).is_some(), "page A should stay offline");
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_params_are_rejected() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;

    for (id, params) in [
        (1, json!({"offline": true})),
        (2, json!({"offline": false, "latency": -1})),
        (3, json!({"latency": 0})),
    ] {
        let resp = raw(&mut ctx, id, "Network.emulateNetworkConditions", params, Some(&session)).await;
        let error = resp.error.unwrap_or_else(|| panic!("params {id} were accepted"));
        assert!(
            format!("{error:?}").contains("Network.emulateNetworkConditions requires"),
            "{error:?}"
        );
    }
}

/// The wreq transport is a separate code path from reqwest, so offline gets its
/// own check there.
#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn stealth_offline_fails_navigation() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new_with_options(None, true);
    let (_, session) = created_and_attached(&mut ctx).await;
    navigate_ok(&mut ctx, 1, &url, &session).await;

    emulate(
        &mut ctx,
        2,
        json!({"offline": true, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1}),
        &session,
    )
    .await;

    let resp = raw(&mut ctx, 3, "Page.navigate", json!({"url": url}), Some(&session)).await;
    let failure = navigation_failure(&resp).expect("offline stealth navigation succeeded");
    assert!(failure.contains("ERR_INTERNET_DISCONNECTED"), "{failure}");

    let value = evaluate_promise(
        &mut ctx,
        4,
        "fetch('/big').then(() => 'ok', e => 'failed')",
        &session,
    )
    .await;
    assert_eq!(value, "failed");
}
