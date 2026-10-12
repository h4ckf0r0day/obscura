// Regression for issue #1244: a `<link rel="preload" as>` inserted via JS must
// fetch and fire `load` (or `error` when the fetch fails) without applying the
// resource. relList.supports('preload') reports true, so loaders that wait on
// the preload before inserting the real stylesheet (React's stylesheet
// resources) hung forever when the link fired neither event.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Serves a page that appends two preload links, one for a stylesheet that
// exists and one for a stylesheet that 404s, and records which event each fires.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..8 {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..]);
                let (status, ct, body) = if req.starts_with("GET /style.css") {
                    ("200 OK", "text/css", "body { color: red; }")
                } else if req.starts_with("GET /missing.css") {
                    ("404 Not Found", "text/plain", "missing")
                } else {
                    (
                        "200 OK",
                        "text/html",
                        r#"<html><head></head><body>
<script>
function preload(href) {
  return new Promise(function (resolve) {
    var l = document.createElement("link");
    l.rel = "preload";
    l.as = "style";
    l.href = href;
    l.onload = function () { resolve("load"); };
    l.onerror = function () { resolve("error"); };
    document.head.appendChild(l);
  });
}
window.__preloads = Promise.all([preload("/style.css"), preload("/missing.css")]);
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
async fn dynamic_preload_link_fires_load_and_error() {
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

    // On main neither link settles, so this resolves to "timeout".
    let v = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": "(async () => JSON.stringify(await Promise.race([window.__preloads, new Promise(r => setTimeout(() => r('timeout'), 5000))])))()",
            "awaitPromise": true,
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    let value = v["result"]["value"].as_str().unwrap_or("");
    assert_eq!(
        value, r#"["load","error"]"#,
        "a preload link must fire load on success and error on a 404 (got {:?})",
        value
    );

    // A preload only fetches; it must not apply the stylesheet.
    let sheets = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({"expression": "String(document.styleSheets.length)", "returnByValue": true}),
        session_id,
    )
    .await;
    let count = sheets["result"]["value"].as_str().unwrap_or("");
    assert_eq!(
        count, "0",
        "a preload link must not add a stylesheet to the document (got {:?})",
        count
    );
}
