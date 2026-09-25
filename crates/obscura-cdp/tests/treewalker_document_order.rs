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
        let mut buf = [0u8; 2048];
        let _ = socket.read(&mut buf).await.unwrap();
        let body = "<html><body></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
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
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        },
        ctx,
    )
    .await;
    assert!(
        response.error.is_none(),
        "CDP {method} failed: {:?}",
        response.error
    );
    response.result.unwrap_or_else(|| json!({}))
}

async fn setup() -> (CdpContext, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "treewalker-session";
    ctx.sessions.insert(session_id.to_string(), page_id);
    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": serve_once().await, "waitUntil": "load"}),
        session_id,
    )
    .await;
    (ctx, session_id.to_string())
}

async fn eval(ctx: &mut CdpContext, session_id: &str, expression: &str) -> Value {
    cdp(
        ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": expression, "returnByValue": true}),
        session_id,
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn next_node_walks_the_whole_subtree_in_document_order() {
    let (mut ctx, session_id) = setup().await;
    let value = eval(
        &mut ctx,
        &session_id,
        r#"(() => {
            document.body.innerHTML =
              '<div id="r"><span></span><p>hi</p><section><a></a><b></b></section></div>';
            const walker = document.createTreeWalker(
              document.getElementById('r'), NodeFilter.SHOW_ELEMENT);
            const seen = [];
            let node;
            while ((node = walker.nextNode())) seen.push(node.tagName);
            return JSON.stringify(seen);
        })()"#,
    )
    .await;
    let seen: Vec<String> =
        serde_json::from_str(value["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(seen, ["SPAN", "P", "SECTION", "A", "B"]);
}

#[tokio::test(flavor = "current_thread")]
async fn next_node_keeps_searching_after_filtered_leaf_nodes() {
    let (mut ctx, session_id) = setup().await;
    let value = eval(
        &mut ctx,
        &session_id,
        r#"(() => {
            document.body.innerHTML =
              '<div id="r"><p>one</p><p>two</p><p>three</p></div>';
            const walker = document.createTreeWalker(
              document.getElementById('r'), NodeFilter.SHOW_TEXT, {
                acceptNode(node) {
                  return node.data === 'two'
                    ? NodeFilter.FILTER_REJECT
                    : NodeFilter.FILTER_ACCEPT;
                }
              });
            const seen = [];
            let node;
            while ((node = walker.nextNode())) seen.push(node.data);
            return JSON.stringify(seen);
        })()"#,
    )
    .await;
    let seen: Vec<String> =
        serde_json::from_str(value["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(seen, ["one", "three"]);
}

#[tokio::test(flavor = "current_thread")]
async fn next_node_handles_a_deep_accepted_child_fast_path() {
    let (mut ctx, session_id) = setup().await;
    let value = eval(
        &mut ctx,
        &session_id,
        r#"(() => {
            const root = document.createElement('div');
            let parent = root;
            for (let i = 0; i < 5000; i++) {
              const child = document.createElement('span');
              parent.appendChild(child);
              parent = child;
            }
            document.body.appendChild(root);
            const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
            let count = 0;
            while (walker.nextNode()) count++;
            return count;
        })()"#,
    )
    .await;
    assert_eq!(value["result"]["value"].as_f64(), Some(5000.0));
}
