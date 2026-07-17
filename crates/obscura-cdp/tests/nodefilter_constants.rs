//! `NodeFilter` must expose the standard filter constants (issue #439).
//! bootstrap.js defined NodeFilter twice — a partial live one (SHOW_ELEMENT /
//! SHOW_TEXT / SHOW_ALL) and a complete one behind a dead `typeof === undefined`
//! guard — so `NodeFilter.FILTER_ACCEPT` was undefined at runtime and the
//! canonical `acceptNode() { return NodeFilter.FILTER_ACCEPT; }` idiom made the
//! walker reject every node. These use the structured_clone_crypto_parity
//! helper pattern.

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
            let body = "<html><body><script>window.__boot = true;</script></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
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

async fn eval(ctx: &mut CdpContext, id: u64, expr: &str, session_id: &str) -> Value {
    cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": expr, "returnByValue": true}),
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
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), session_id).await;
    (ctx, session_id.to_string())
}

#[tokio::test(flavor = "current_thread")]
async fn node_filter_exposes_the_standard_constants() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"JSON.stringify({
            accept: NodeFilter.FILTER_ACCEPT,
            reject: NodeFilter.FILTER_REJECT,
            skip: NodeFilter.FILTER_SKIP,
            showAll: NodeFilter.SHOW_ALL,
            showElement: NodeFilter.SHOW_ELEMENT,
            showText: NodeFilter.SHOW_TEXT,
            showComment: NodeFilter.SHOW_COMMENT,
        })"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["accept"], 1, "FILTER_ACCEPT must be 1");
    assert_eq!(val["reject"], 2, "FILTER_REJECT must be 2");
    assert_eq!(val["skip"], 3, "FILTER_SKIP must be 3");
    assert_eq!(val["showAll"], 0xFFFFFFFFu32, "SHOW_ALL must be 0xFFFFFFFF");
    assert_eq!(val["showElement"], 1, "SHOW_ELEMENT must be 1");
    assert_eq!(val["showText"], 4, "SHOW_TEXT must be 4");
    assert_eq!(val["showComment"], 128, "SHOW_COMMENT must be 128");
}

#[tokio::test(flavor = "current_thread")]
async fn tree_walker_filter_using_filter_accept_constant_works() {
    let (mut ctx, sid) = setup().await;
    // The canonical MDN idiom: acceptNode returns NodeFilter.FILTER_ACCEPT.
    // Only the first accepted child is checked, so this does not depend on the
    // separate nextNode leaf-advance fix (#432).
    let v = eval(
        &mut ctx,
        2,
        r#"(() => {
            document.body.innerHTML = '<div id="r"><p id="target">keep</p></div>';
            const r = document.getElementById('r');
            const w = document.createTreeWalker(r, NodeFilter.SHOW_ELEMENT, {
                acceptNode() { return NodeFilter.FILTER_ACCEPT; }
            });
            const first = w.nextNode();
            return JSON.stringify({ id: first ? first.id : null });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        val["id"], "target",
        "a filter returning NodeFilter.FILTER_ACCEPT must accept the node, not reject it"
    );
}
