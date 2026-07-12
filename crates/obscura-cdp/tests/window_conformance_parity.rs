// Conformance parity: a handful of standard Web APIs that real Chrome
// exposes but bootstrap.js left undefined, so vendor JS that feature-detects
// them dies with "X is not a function". Each case fails on main, passes
// after the stubs land. These are spec surface gaps, not anti-bot work.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_once() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = "<html><body><div id=a></div></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
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

async fn eval(ctx: &mut CdpContext, id: u64, expr: &str, session_id: &str) -> Value {
    cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
        session_id,
    )
    .await
}

async fn setup() -> (CdpContext, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_once().await;
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
    (ctx, session_id.to_string())
}

#[tokio::test(flavor = "current_thread")]
async fn element_toggle_attribute_is_callable_and_toggles() {
    let (mut ctx, sid) = setup().await;
    // Element.prototype.toggleAttribute is on every real Chrome. Bootstrap
    // leaves it undefined, so any framework that calls el.toggleAttribute()
    // (Lit, Stencil, several ad SDKs) throws.
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            const el = document.getElementById('a');
            const t = typeof el.toggleAttribute;
            const first = el.toggleAttribute('hidden');
            const afterFirst = el.hasAttribute('hidden');
            const second = el.toggleAttribute('hidden');
            const afterSecond = el.hasAttribute('hidden');
            return JSON.stringify({ type: t, first, afterFirst, second, afterSecond });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["type"], "function");
    assert_eq!(val["first"], true, "first toggle should add the attribute");
    assert_eq!(val["afterFirst"], true);
    assert_eq!(val["second"], false, "second toggle should remove it");
    assert_eq!(val["afterSecond"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn document_adopt_node_moves_node() {
    let (mut ctx, sid) = setup().await;
    // document.adoptNode is standard DOM; bootstrap leaves it undefined.
    // Frameworks that move nodes between documents (iframes, portals) call it.
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            const t = typeof document.adoptNode;
            const span = document.createElement('span');
            span.id = 'moved';
            const adopted = document.adoptNode(span);
            return JSON.stringify({
                type: t,
                sameNode: adopted === span,
                ownerDoc: adopted.ownerDocument === document,
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["type"], "function");
    assert_eq!(val["sameNode"], true);
    assert_eq!(val["ownerDoc"], true);
}
