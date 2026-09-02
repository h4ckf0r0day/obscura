// Regression for issue #406: requests initiated by page JS (fetch/XHR/dynamic
// resource) must emit Network.requestWillBeSent / responseReceived so
// Puppeteer/Playwright `page.on('request'|'response')` observe them. On main
// only the static navigation subresources surfaced; a `fetch()` fired from the
// page produced no CDP Network event, so clients captured zero XHR/JSON
// responses (this is also the root cause of the Aviasales half of #394).

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
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

async fn serve_binary_fetch() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..16 {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let (content_type, body): (Option<&str>, Vec<u8>) = if request.starts_with("GET /bytes.bin") {
                    (Some("application/octet-stream"), (0u8..=255).collect())
                } else if request.starts_with("GET /ascii.bin") {
                    (Some("application/octet-stream"), b"ABC".to_vec())
                } else if request.starts_with("GET /invalid.txt") {
                    (Some("text/plain"), vec![0xff])
                } else if request.starts_with("GET /win1252.txt") {
                    (Some("text/plain; charset=windows-1252"), vec![0x80])
                } else if request.starts_with("GET /gbk.txt") {
                    (Some("text/plain; charset=gbk"), vec![0xd6, 0xd0, 0xce, 0xc4])
                } else if request.starts_with("GET /invalid.json") {
                    (Some("application/json"), vec![0xff])
                } else if request.starts_with("GET /legacy.js") {
                    (Some("application/x-javascript"), b"legacy()".to_vec())
                } else if request.starts_with("GET /legacy.css") {
                    (Some("text/css"), vec![0xff])
                } else if request.starts_with("GET /no-type") {
                    (None, vec![0x81, 0x8d])
                } else if request.starts_with("GET /empty.json") {
                    (Some("application/json"), Vec::new())
                } else if request.starts_with("GET /empty-no-type") {
                    (None, Vec::new())
                } else {
                    (
                        Some("text/html"),
                        br#"<script>Promise.all([
                          fetch('/bytes.bin').then(r => r.arrayBuffer()),
                          fetch('/ascii.bin').then(r => r.arrayBuffer()),
                          fetch('/invalid.txt').then(r => r.text()),
                          fetch('/win1252.txt').then(r => r.text()),
                          fetch('/gbk.txt').then(r => r.text()),
                          fetch('/invalid.json').then(r => r.arrayBuffer()),
                          fetch('/legacy.js').then(r => r.text()),
                          fetch('/legacy.css').then(r => r.text()),
                          fetch('/no-type').then(r => r.arrayBuffer()),
                          fetch('/empty.json').then(r => r.text()),
                          fetch('/empty-no-type').then(r => r.text())
                        ]).then(() => { globalThis.__done = true; });</script>"#.to_vec(),
                    )
                };
                let content_type = content_type
                    .map(|value| format!("Content-Type: {value}\r\n"))
                    .unwrap_or_default();
                let header = format!(
                    "HTTP/1.1 200 OK\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
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

    // An ordinary fetch() is not load-delaying in Chromium: `load` may fire
    // while its response is still pending. Ask for networkidle0 explicitly so
    // this output-level assertion observes the completed request without
    // turning every load navigation into an implicit global settle.
    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": base, "waitUntil": "networkidle0"}),
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
    assert_eq!(body.get("base64Encoded"), Some(&json!(false)));
}

#[tokio::test(flavor = "current_thread")]
async fn js_fetch_get_response_body_preserves_binary_bytes() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = serve_binary_fetch().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": base, "waitUntil": "networkidle0"}),
        session_id,
    )
    .await;

    let request_id = response_request_id(&ctx, "/bytes.bin")
        .expect("binary fetch must emit a response with a requestId");
    let body = cdp(
        &mut ctx,
        2,
        "Network.getResponseBody",
        json!({"requestId": request_id}),
        session_id,
    )
    .await;
    assert_eq!(body.get("base64Encoded"), Some(&json!(true)));
    let encoded = body.get("body").and_then(Value::as_str).expect("response body");
    assert_eq!(BASE64.decode(encoded).unwrap(), (0u8..=255).collect::<Vec<_>>());
}

#[tokio::test(flavor = "current_thread")]
async fn network_get_response_body_uses_chromium_mime_encoding_rules() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = serve_binary_fetch().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": base, "waitUntil": "networkidle0"}),
        session_id,
    )
    .await;

    let ascii_id = response_request_id(&ctx, "/ascii.bin").expect("ASCII binary response");
    let ascii = cdp(
        &mut ctx,
        2,
        "Network.getResponseBody",
        json!({"requestId": ascii_id}),
        session_id,
    )
    .await;
    assert_eq!(ascii, json!({"body": "QUJD", "base64Encoded": true}));

    let text_id = response_request_id(&ctx, "/invalid.txt").expect("invalid text response");
    let text = cdp(
        &mut ctx,
        3,
        "Network.getResponseBody",
        json!({"requestId": text_id}),
        session_id,
    )
    .await;
    assert_eq!(text, json!({"body": "ÿ", "base64Encoded": false}));

    let windows_id = response_request_id(&ctx, "/win1252.txt").expect("windows-1252 response");
    let windows = cdp(
        &mut ctx,
        4,
        "Network.getResponseBody",
        json!({"requestId": windows_id}),
        session_id,
    )
    .await;
    assert_eq!(windows, json!({"body": "€", "base64Encoded": false}));

    let invalid_json_id = response_request_id(&ctx, "/invalid.json").expect("invalid JSON");
    let invalid_json = cdp(
        &mut ctx,
        5,
        "Network.getResponseBody",
        json!({"requestId": invalid_json_id}),
        session_id,
    )
    .await;
    assert_eq!(invalid_json, json!({"body": "/w==", "base64Encoded": true}));

    let legacy_id = response_request_id(&ctx, "/legacy.js").expect("legacy JavaScript MIME");
    let legacy = cdp(
        &mut ctx,
        6,
        "Network.getResponseBody",
        json!({"requestId": legacy_id}),
        session_id,
    )
    .await;
    assert_eq!(legacy, json!({"body": "legacy()", "base64Encoded": false}));

    let missing_id = response_request_id(&ctx, "/no-type").expect("missing MIME response");
    let missing = cdp(
        &mut ctx,
        7,
        "Network.getResponseBody",
        json!({"requestId": missing_id}),
        session_id,
    )
    .await;
    assert_eq!(missing, json!({"body": "\u{81}\u{8d}", "base64Encoded": false}));

    let empty_id = response_request_id(&ctx, "/empty.json").expect("empty JSON response");
    let empty = cdp(
        &mut ctx,
        8,
        "Network.getResponseBody",
        json!({"requestId": empty_id}),
        session_id,
    )
    .await;
    assert_eq!(empty, json!({"body": "", "base64Encoded": false}));

    let empty_no_type_id = response_request_id(&ctx, "/empty-no-type")
        .expect("empty response without MIME type");
    let empty_no_type = cdp(
        &mut ctx,
        9,
        "Network.getResponseBody",
        json!({"requestId": empty_no_type_id}),
        session_id,
    )
    .await;
    assert_eq!(empty_no_type, json!({"body": "", "base64Encoded": false}));

    let legacy_css_id = response_request_id(&ctx, "/legacy.css").expect("legacy CSS text MIME");
    let legacy_css = cdp(
        &mut ctx,
        10,
        "Network.getResponseBody",
        json!({"requestId": legacy_css_id}),
        session_id,
    )
    .await;
    assert_eq!(legacy_css, json!({"body": "ÿ", "base64Encoded": false}));

    let gbk_id = response_request_id(&ctx, "/gbk.txt").expect("GBK text response");
    let gbk = cdp(
        &mut ctx,
        11,
        "Network.getResponseBody",
        json!({"requestId": gbk_id}),
        session_id,
    )
    .await;
    assert_eq!(gbk, json!({"body": "中文", "base64Encoded": false}));

    let navigated = cdp(
        &mut ctx,
        12,
        "Page.navigate",
        json!({"url": format!("{base}gbk.txt")}),
        session_id,
    )
    .await;
    let loader_id = navigated["loaderId"].as_str().unwrap();
    let document_gbk = cdp(
        &mut ctx,
        13,
        "Network.getResponseBody",
        json!({"requestId": loader_id}),
        session_id,
    )
    .await;
    assert_eq!(
        document_gbk,
        json!({"body": "中文", "base64Encoded": false}),
    );

    let navigated = cdp(
        &mut ctx,
        14,
        "Page.navigate",
        json!({"url": format!("{base}bytes.bin")}),
        session_id,
    )
    .await;
    let loader_id = navigated["loaderId"].as_str().unwrap();
    let document_binary = cdp(
        &mut ctx,
        15,
        "Network.getResponseBody",
        json!({"requestId": loader_id}),
        session_id,
    )
    .await;
    assert_eq!(document_binary["base64Encoded"], true);
    assert_eq!(
        BASE64.decode(document_binary["body"].as_str().unwrap()).unwrap(),
        (0u8..=255).collect::<Vec<_>>(),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn binary_fetch_response_stream_preserves_original_bytes() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = serve_binary_fetch().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": base, "waitUntil": "networkidle0"}),
        session_id,
    )
    .await;
    let request_id = response_request_id(&ctx, "/bytes.bin").expect("binary response");
    let stream = cdp(
        &mut ctx,
        2,
        "Fetch.takeResponseBodyAsStream",
        json!({"requestId": request_id}),
        session_id,
    )
    .await;
    let handle = stream.get("stream").and_then(Value::as_str).expect("stream handle");
    let read = cdp(
        &mut ctx,
        3,
        "IO.read",
        json!({"handle": handle, "size": 512}),
        session_id,
    )
    .await;
    assert_eq!(read.get("eof"), Some(&json!(true)));
    assert_eq!(read.get("base64Encoded"), Some(&json!(true)));
    let encoded = read.get("data").and_then(Value::as_str).expect("stream data");
    assert_eq!(BASE64.decode(encoded).unwrap(), (0u8..=255).collect::<Vec<_>>());
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
