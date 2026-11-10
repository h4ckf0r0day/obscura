// Regression test for iframes whose src is a blob: URL. The iframe loader sent
// every src to the native fetch op, which cannot see the realm's object URL
// store, so the frame stayed on an empty document. Fails on main, passes after
// the fix.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let body = "<html><body></body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: &str,
) -> Value {
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
async fn iframe_loads_a_blob_url_document() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let sid = "session-1";
    ctx.sessions.insert(sid.to_string(), page_id.clone());
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), sid).await;

    let v = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": r#"new Promise((resolve) => {
                const blob = new Blob(['<!doctype html><title>x</title><p id=a>Hello<b>bold</b>'], { type: 'text/html' });
                const src = URL.createObjectURL(blob);
                const iframe = document.createElement('iframe');
                iframe.onload = () => {
                    const doc = iframe.contentDocument;
                    resolve(JSON.stringify({
                        sameUrl: iframe.contentWindow.location.href === src,
                        title: doc.title,
                        body: doc.body.innerHTML,
                    }));
                };
                iframe.src = src;
                document.body.appendChild(iframe);
            })"#,
            "returnByValue": true,
            "awaitPromise": true,
        }),
        sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["sameUrl"], true, "the frame is at the blob URL, not about:blank");
    assert_eq!(val["title"], "x");
    assert_eq!(val["body"], r#"<p id="a">Hello<b>bold</b></p>"#);
}
