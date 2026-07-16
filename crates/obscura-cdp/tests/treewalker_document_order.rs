//! `TreeWalker.nextNode()` (and `createNodeIterator`, which aliases it) must
//! walk the whole subtree in document order, not stop at the first leaf (issue
//! #432). The old impl seeded traversal from `currentNode.firstChild` and only
//! walked that subtree, so once `currentNode` was a leaf it returned null and
//! iteration died. These mirror the structured_clone_crypto_parity helpers.

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
async fn tree_walker_next_node_walks_full_subtree_in_document_order() {
    let (mut ctx, sid) = setup().await;
    // A tree whose second node (SPAN) is an empty leaf: the old impl returned
    // null right after it, so P/SECTION/A/B were never visited.
    let v = eval(
        &mut ctx,
        2,
        r#"(() => {
            document.body.innerHTML =
              '<div id="r"><span></span><p>hi</p><section><a></a><b></b></section></div>';
            const r = document.getElementById('r');
            const w = document.createTreeWalker(r, NodeFilter.SHOW_ELEMENT);
            const seen = [];
            let n;
            while ((n = w.nextNode())) seen.push(n.tagName);
            return JSON.stringify(seen);
        })()"#,
        &sid,
    )
    .await;
    let seen: Vec<String> = serde_json::from_str(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        seen,
        vec!["SPAN", "P", "SECTION", "A", "B"],
        "nextNode must visit every element in document order, not stop at the first leaf"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tree_walker_honours_a_filter_function_across_leaves() {
    let (mut ctx, sid) = setup().await;
    // SHOW_TEXT plus a filter that rejects whitespace-only text. The text nodes
    // live under sibling leaf elements, so reaching the second one requires
    // advancing past a leaf.
    let v = eval(
        &mut ctx,
        2,
        r#"(() => {
            document.body.innerHTML =
              '<div id="r"><p>one</p><p>two</p><p>three</p></div>';
            const r = document.getElementById('r');
            // Numeric FILTER_ACCEPT(1)/FILTER_REJECT(2): this test targets
            // nextNode traversal, not the NodeFilter.* constants.
            const w = document.createTreeWalker(r, NodeFilter.SHOW_TEXT, {
                acceptNode(node) {
                    return node.data.trim() ? 1 : 2;
                }
            });
            const seen = [];
            let n;
            while ((n = w.nextNode())) seen.push(n.data);
            return JSON.stringify(seen);
        })()"#,
        &sid,
    )
    .await;
    let seen: Vec<String> = serde_json::from_str(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        seen,
        vec!["one", "two", "three"],
        "the text of every paragraph must be collected across sibling leaves"
    );
}
