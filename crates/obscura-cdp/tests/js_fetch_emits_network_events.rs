// Regression for issue #406: requests initiated by page JS (fetch/XHR/dynamic
// resource) must emit Network.requestWillBeSent / responseReceived so
// Puppeteer/Playwright `page.on('request'|'response')` observe them. On main
// only the static navigation subresources surfaced; a `fetch()` fired from the
// page produced no CDP Network event, so clients captured zero XHR/JSON
// responses (this is also the root cause of the Aviasales half of #394).

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Serves an HTML page that, on load, fetches /api/data.json, plus that JSON.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..6 {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..]);
                let (ct, body) = if req.starts_with("GET /api/data.json") {
                    ("application/json", "{\"value\":42}")
                } else {
                    (
                        "text/html",
                        r#"<html><head></head><body>
<div id="r">stage1</div>
<script>
window.__done = new Promise(function (resolve) {
  fetch("/api/data.json")
    .then(function (r) { return r.json(); })
    .then(function (d) { document.getElementById("r").textContent = "got:" + d.value; resolve("ok"); })
    .catch(function (e) { resolve("err:" + e); });
});
</script>
</body></html>"#,
                    )
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session_id: &str) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

// Collect the request URLs from every Network.requestWillBeSent currently
// queued in ctx.pending_events, then clear the queue.
fn drain_request_urls(ctx: &mut CdpContext) -> Vec<String> {
    let urls = ctx
        .pending_events
        .iter()
        .filter(|e| e.method == "Network.requestWillBeSent")
        .filter_map(|e| e.params.get("request").and_then(|r| r.get("url")).and_then(|u| u.as_str()).map(str::to_string))
        .collect();
    ctx.pending_events.clear();
    urls
}

// The requestId that Network.responseReceived reported for the given URL.
fn response_request_id(ctx: &CdpContext, url_needle: &str) -> Option<String> {
    ctx.pending_events
        .iter()
        .find(|e| {
            e.method == "Network.responseReceived"
                && e.params
                    .get("response")
                    .and_then(|r| r.get("url"))
                    .and_then(|u| u.as_str())
                    .map(|u| u.contains(url_needle))
                    .unwrap_or(false)
        })
        .and_then(|e| e.params.get("requestId").and_then(|v| v.as_str()).map(str::to_string))
}

#[tokio::test(flavor = "current_thread")]
async fn js_fetch_emits_network_request_and_response() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = serve().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    // Navigate with waitUntil:load so the after-load fetch() runs and settles
    // before the navigation emits its Network events.
    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": base, "waitUntil": "load"}),
        session_id,
    )
    .await;

    // The fetched JSON URL must appear as a requestWillBeSent event.
    let request_urls = ctx
        .pending_events
        .iter()
        .filter(|e| e.method == "Network.requestWillBeSent")
        .filter_map(|e| e.params.get("request").and_then(|r| r.get("url")).and_then(|u| u.as_str()).map(str::to_string))
        .collect::<Vec<_>>();
    assert!(
        request_urls.iter().any(|u| u.contains("/api/data.json")),
        "script-initiated fetch must emit Network.requestWillBeSent; saw {request_urls:?}"
    );

    // And its response body must be resolvable via the same requestId, so a
    // client can read the captured JSON.
    let request_id = response_request_id(&ctx, "/api/data.json")
        .expect("fetch must emit Network.responseReceived with a requestId");
    let body = cdp(
        &mut ctx,
        2,
        "Network.getResponseBody",
        json!({"requestId": request_id}),
        session_id,
    )
    .await;
    assert_eq!(
        body.get("body").and_then(|b| b.as_str()),
        Some("{\"value\":42}"),
        "Network.getResponseBody must return the script-fetched JSON"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn navigation_without_script_fetch_is_unaffected() {
    // A page that issues no script fetch must still emit exactly its document
    // request, proving the #406 change adds nothing spurious.
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await.unwrap();
        let body = "<html><body>plain</body></html>";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(resp.as_bytes()).await;
    });
    let base = format!("http://{addr}/");

    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(&mut ctx, 1, "Page.navigate", json!({"url": base, "waitUntil": "load"}), session_id).await;

    let urls = drain_request_urls(&mut ctx);
    assert!(
        urls.iter().any(|u| u == &base || u.starts_with(&base)),
        "the document request must still be emitted; saw {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u.contains("/api/")),
        "no spurious script-fetch events for a page that makes none; saw {urls:?}"
    );
}
