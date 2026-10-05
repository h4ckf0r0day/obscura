//! #872 / #873 — concurrent pages over ONE CDP connection must each keep their
//! own live JS heap.
//!
//! Playwright's `connectOverCDP` multiplexes N contexts/pages over a single
//! connection. It installs a per-page utility-script object, hands back an
//! `objectId`, and later drives the page through `callFunctionOn { objectId }`.
//!
//! The server used to allow only ONE live V8 isolate per connection: routing a
//! command to another page `suspend_js`'d whichever page was live (tearing its
//! isolate down) and `resume_js`'d the target (rebuilding a fresh one from the
//! snapshot). So a command for page-B destroyed page-A's isolate, and page-A's
//! installed object — and its `objectId` binding — vanished. Playwright then
//! saw `Cannot read properties of undefined (reading 'evaluate')` (issue #872),
//! and the constant snapshot re-deserialization retained ~130 MB per rebuild
//! (issue #873).
//!
//! The runtime layer has supported many concurrently-live isolates on one
//! thread since #756 (each entered only transiently per op, strictly nested),
//! so a page's heap must now survive a command routed to a different page.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: Option<&str>,
) -> obscura_cdp::types::CdpResponse {
    dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.map(str::to_string),
        },
        ctx,
    )
    .await
}

async fn create_and_attach(ctx: &mut CdpContext, url: &str, id: u64) -> String {
    let created = cdp(ctx, id, "Target.createTarget", json!({ "url": url }), None).await;
    assert!(created.error.is_none(), "createTarget failed: {:?}", created.error);
    let target = created.result.unwrap()["targetId"].as_str().unwrap().to_string();
    let attached = cdp(
        ctx,
        id + 1,
        "Target.attachToTarget",
        json!({ "targetId": target, "flatten": true }),
        None,
    )
    .await;
    attached.result.unwrap()["sessionId"].as_str().unwrap().to_string()
}

/// State established in one page's JS heap must still be there after an
/// unrelated command runs against a *different* page on the same connection.
#[tokio::test(flavor = "current_thread")]
async fn concurrent_pages_keep_their_own_js_heap() {
    let mut ctx = CdpContext::new();
    let first = create_and_attach(&mut ctx, "about:blank", 1).await;
    let second = create_and_attach(&mut ctx, "about:blank", 10).await;

    // Give each page a distinct value in its own default context.
    let set_a = cdp(
        &mut ctx,
        20,
        "Runtime.evaluate",
        json!({ "expression": "globalThis.__probe = 'first-state'; '__probe set'" }),
        Some(&first),
    )
    .await;
    assert!(set_a.error.is_none(), "set on first failed: {:?}", set_a.error);

    let set_b = cdp(
        &mut ctx,
        21,
        "Runtime.evaluate",
        json!({ "expression": "globalThis.__probe = 'second-state'; '__probe set'" }),
        Some(&second),
    )
    .await;
    assert!(set_b.error.is_none(), "set on second failed: {:?}", set_b.error);

    // Read each page's value back. Under the single-live-isolate model, setting
    // `second` tore down `first`'s isolate, so `first.__probe` is gone.
    let read_a = cdp(
        &mut ctx,
        22,
        "Runtime.evaluate",
        json!({ "expression": "globalThis.__probe", "returnByValue": true }),
        Some(&first),
    )
    .await;
    assert!(read_a.error.is_none(), "read on first errored: {:?}", read_a.error);
    assert_eq!(
        read_a.result.unwrap()["result"]["value"],
        json!("first-state"),
        "first page's JS heap was destroyed by a command routed to the second page"
    );

    let read_b = cdp(
        &mut ctx,
        23,
        "Runtime.evaluate",
        json!({ "expression": "globalThis.__probe", "returnByValue": true }),
        Some(&second),
    )
    .await;
    assert!(read_b.error.is_none(), "read on second errored: {:?}", read_b.error);
    assert_eq!(
        read_b.result.unwrap()["result"]["value"],
        json!("second-state"),
        "second page's JS heap was destroyed by a command routed to the first page"
    );
}

/// The exact Playwright pattern: an `objectId` handed out for one page is
/// driven through `callFunctionOn`. The handle must keep pointing at the *same*
/// live object across a command routed to another page — mutations made through
/// it must persist. Tearing the page's isolate down and rebuilding it from a
/// stored recipe silently resets that object's state, which is what breaks
/// Playwright's utility script.
#[tokio::test(flavor = "current_thread")]
async fn an_objectid_keeps_its_live_object_across_a_command_on_another_page() {
    let mut ctx = CdpContext::new();
    let first = create_and_attach(&mut ctx, "about:blank", 1).await;
    let second = create_and_attach(&mut ctx, "about:blank", 10).await;

    // Mint a stateful object handle in `first` (stands in for Playwright's
    // utility script object, which accumulates state as it drives the page).
    let handle = cdp(
        &mut ctx,
        20,
        "Runtime.evaluate",
        json!({ "expression": "globalThis.__util = { n: 0 }; globalThis.__util" }),
        Some(&first),
    )
    .await;
    assert!(handle.error.is_none(), "evaluate on first failed: {:?}", handle.error);
    let object_id = handle.result.unwrap()["result"]["objectId"]
        .as_str()
        .expect("evaluate should return an objectId for an object")
        .to_string();

    // First call through the handle: n -> 1.
    let first_call = cdp(
        &mut ctx,
        21,
        "Runtime.callFunctionOn",
        json!({
            "objectId": object_id,
            "functionDeclaration": "function () { return ++this.n; }",
            "returnByValue": true,
        }),
        Some(&first),
    )
    .await;
    assert!(first_call.error.is_none(), "first callFunctionOn failed: {:?}", first_call.error);
    assert_eq!(first_call.result.unwrap()["result"]["value"], json!(1.0));

    // Touch the other page — this is what tore `first`'s isolate down before.
    let touch = cdp(
        &mut ctx,
        22,
        "Runtime.evaluate",
        json!({ "expression": "1 + 1", "returnByValue": true }),
        Some(&second),
    )
    .await;
    assert!(touch.error.is_none(), "evaluate on second failed: {:?}", touch.error);

    // Second call through the SAME handle must continue from n == 1 -> 2. If
    // the isolate was rebuilt, the handle points at a freshly reconstructed
    // `{ n: 0 }` and this wrongly returns 1 again.
    let second_call = cdp(
        &mut ctx,
        23,
        "Runtime.callFunctionOn",
        json!({
            "objectId": object_id,
            "functionDeclaration": "function () { return ++this.n; }",
            "returnByValue": true,
        }),
        Some(&first),
    )
    .await;
    assert!(
        second_call.error.is_none(),
        "second callFunctionOn (after touching the other page) failed: {:?}",
        second_call.error
    );
    assert_eq!(
        second_call.result.unwrap()["result"]["value"],
        json!(2.0),
        "the first page's object handle lost its state when the second page ran a command"
    );
}
