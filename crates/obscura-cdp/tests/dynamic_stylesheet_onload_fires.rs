// Regression for issue #409: a `<link rel="stylesheet" href>` inserted via JS
// (createElement + appendChild) must fetch and fire `load`, so frameworks that
// await the link's onload (Promise.all of lazy CSS + JS, antd/bootstrap
// loaders) resolve instead of hanging forever. On main the link neither fires
// load nor error, so the page stays on stage1.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Serves a page that appends a stylesheet link and resolves a promise on load.
// Also serves the CSS body so the fetch succeeds.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..4 {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..]);
                let (status, ct, body) = if req.starts_with("GET /style.css") {
                    ("200 OK", "text/css", "body { color: red; }")
                } else {
                    (
                        "200 OK",
                        "text/html",
                        r#"<html><head></head><body>
<div id="r">stage1</div>
<script>
window.__loaded = new Promise(function (resolve, reject) {
  var l = document.createElement("link");
  l.rel = "stylesheet";
  l.href = "/style.css";
  l.onload = function () { document.getElementById("r").textContent = "stage2"; resolve("ok"); };
  l.onerror = function () { reject("err"); };
  document.head.appendChild(l);
});
</script>
</body></html>"#,
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

#[tokio::test(flavor = "current_thread")]
async fn dynamic_stylesheet_fires_load() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": url, "waitUntil": "load"}),
        session_id,
    )
    .await;

    // Race the link's onload promise against a 5s timeout. On main the link
    // never settles, so this rejects with "timeout" and the assertion fails.
    let v = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": "(async () => { try { return await Promise.race([window.__loaded, new Promise((_, r) => setTimeout(() => r('timeout'), 5000))]); } catch (e) { return 'rejected:' + e; } })()",
            "awaitPromise": true,
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    let value = v["result"]["value"].as_str().unwrap_or("");
    assert_eq!(value, "ok", "dynamic <link rel=stylesheet> must fire load (got {:?})", value);

    let text = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({"expression": "document.getElementById('r').textContent", "returnByValue": true}),
        session_id,
    )
    .await;
    assert_eq!(
        text["result"]["value"], "stage2",
        "the page must advance to stage2 once the stylesheet link loads"
    );
}
