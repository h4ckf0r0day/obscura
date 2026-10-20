//! `DOM.getOuterHTML` is the one DOM call a CDP client makes while holding a
//! JavaScript element handle rather than a node id: go-rod's `element.HTML()`
//! sends `DOM.getOuterHTML {objectId}` (rod/element.go), because it never asked
//! for a node id in the first place.
//!
//! Chromium accepts any of `nodeId`, `backendNodeId` or `objectId` here.
//! obscura accepted only the first two, so every rod `page.HTML()` - the call
//! behind the crawler's static HTML collection - came back with
//! `{-32601 nodeId required}` and the page was recorded as a failure even
//! though it had loaded fine.
//!
//! The objectId lookup is shared with `DOM.describeNode` / `DOM.resolveNode`
//! via `resolve_node_id`, so a handle that cannot be mapped to a node fails the
//! same way in all three rather than silently returning node 0.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

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
async fn outer_html_is_available_for_a_node_only_object_id() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": "data:text/html,<div id=a><b>hi</b></div>", "waitUntil": "load"}),
        session_id,
    )
    .await;

    let handle = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": "document.getElementById('a')"}),
        session_id,
    )
    .await;
    let object_id = handle
        .pointer("/result/objectId")
        .and_then(|v| v.as_str())
        .expect("an element handle must come back as an objectId")
        .to_string();

    // The regression itself: this used to answer `nodeId required`.
    let by_object_id = cdp(
        &mut ctx,
        3,
        "DOM.getOuterHTML",
        json!({"objectId": object_id}),
        session_id,
    )
    .await;

    assert_eq!(
        by_object_id
            .get("outerHTML")
            .and_then(|v| v.as_str()),
        Some("<div id=\"a\"><b>hi</b></div>"),
        "the HTML must come from the element the handle points at; got {by_object_id:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn outer_html_still_accepts_a_plain_node_id() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": "data:text/html,<p id=a>text</p>", "waitUntil": "load"}),
        session_id,
    )
    .await;

    let found = cdp(
        &mut ctx,
        2,
        "DOM.querySelector",
        json!({"nodeId": 0, "selector": "#a"}),
        session_id,
    )
    .await;
    let node_id = found
        .get("nodeId")
        .and_then(|v| v.as_u64())
        .expect("querySelector must hand back a nodeId");

    let html = cdp(
        &mut ctx,
        3,
        "DOM.getOuterHTML",
        json!({"nodeId": node_id}),
        session_id,
    )
    .await;

    assert_eq!(
        html.get("outerHTML").and_then(|v| v.as_str()),
        Some("<p id=\"a\">text</p>"),
        "the nodeId path must keep working; got {html:#?}"
    );
}
