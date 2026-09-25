//! `Runtime.bindingCalled` was addressed to one arbitrary session of the page,
//! picked by HashMap ordering. A client reaching a page the ordinary way holds
//! two sessions — `Target.createTarget` opens one and the `Target.attachToTarget`
//! after it opens another — and discards any event whose sessionId is not the
//! one it attached with, so `page.exposeFunction()` never fired its callback.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session.map(str::to_string),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

/// Opens a page the way Puppeteer and Playwright do, rather than inserting a
/// session for a hand-made page, and returns `(targetId, sessionId)`.
async fn created_and_attached(ctx: &mut CdpContext) -> (String, String) {
    let created = cdp(ctx, 900, "Target.createTarget", json!({"url": "about:blank"}), None).await;
    let target_id = created["targetId"].as_str().expect("no targetId").to_string();
    let session = attach_to(ctx, &target_id, 901).await;
    (target_id, session)
}

async fn attach_to(ctx: &mut CdpContext, target_id: &str, id: u64) -> String {
    let attached = cdp(
        ctx,
        id,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )
    .await;
    attached["sessionId"]
        .as_str()
        .expect("no sessionId")
        .to_string()
}

fn binding_calls<'a>(ctx: &'a CdpContext) -> Vec<(&'a str, &'a Value)> {
    ctx.pending_events
        .iter()
        .filter(|e| e.method == "Runtime.bindingCalled")
        .map(|e| (e.session_id.as_deref().unwrap_or(""), &e.params))
        .collect()
}

/// The deterministic guard. Two sessions each subscribe, so the correct answer
/// is two events. Delivering to one session of the page — whichever a HashMap
/// yields — can only ever produce one, so this fails on the old behaviour no
/// matter which session that ordering happens to pick.
#[tokio::test(flavor = "current_thread")]
async fn every_session_that_added_the_binding_is_called() {
    let mut ctx = CdpContext::new();
    let (target_id, first) = created_and_attached(&mut ctx).await;
    let second = attach_to(&mut ctx, &target_id, 902).await;
    assert_ne!(first, second, "the second attach reused the first session");

    for (id, session) in [(1, &first), (2, &second)] {
        cdp(&mut ctx, id, "Runtime.enable", json!({}), Some(session)).await;
        cdp(
            &mut ctx,
            id + 10,
            "Runtime.addBinding",
            json!({"name": "obscuraProbe"}),
            Some(session),
        )
        .await;
    }

    cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({"expression": "obscuraProbe('HELLO')"}),
        Some(&first),
    )
    .await;

    let called = binding_calls(&ctx);
    let mut got: Vec<&str> = called.iter().map(|(session, _)| *session).collect();
    got.sort_unstable();
    let mut want = vec![first.as_str(), second.as_str()];
    want.sort_unstable();
    assert_eq!(got, want, "both subscribers should have been called: {called:?}");
    for (_, params) in &called {
        assert_eq!(params["name"], "obscuraProbe");
        assert_eq!(params["payload"], "HELLO");
    }
}

/// A session that never asked for the binding is not told about it, so a client
/// does not see a call it has no handler for.
#[tokio::test(flavor = "current_thread")]
async fn a_session_that_did_not_subscribe_is_not_called() {
    let mut ctx = CdpContext::new();
    let (target_id, subscriber) = created_and_attached(&mut ctx).await;
    let bystander = attach_to(&mut ctx, &target_id, 902).await;

    cdp(&mut ctx, 1, "Runtime.enable", json!({}), Some(&subscriber)).await;
    cdp(&mut ctx, 2, "Runtime.enable", json!({}), Some(&bystander)).await;
    cdp(
        &mut ctx,
        3,
        "Runtime.addBinding",
        json!({"name": "obscuraProbe"}),
        Some(&subscriber),
    )
    .await;
    cdp(
        &mut ctx,
        4,
        "Runtime.evaluate",
        json!({"expression": "obscuraProbe('HELLO')"}),
        Some(&subscriber),
    )
    .await;

    let called = binding_calls(&ctx);
    assert_eq!(
        called.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        vec![subscriber.as_str()],
        "only the subscribing session should be called: {called:?}"
    );
}

/// Removing the binding drops both the shim and the subscription, so a later
/// call cannot be delivered to a client that has stopped listening.
#[tokio::test(flavor = "current_thread")]
async fn a_removed_binding_is_not_called() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;

    cdp(&mut ctx, 1, "Runtime.enable", json!({}), Some(&session)).await;
    cdp(
        &mut ctx,
        2,
        "Runtime.addBinding",
        json!({"name": "obscuraProbe"}),
        Some(&session),
    )
    .await;
    cdp(
        &mut ctx,
        3,
        "Runtime.removeBinding",
        json!({"name": "obscuraProbe"}),
        Some(&session),
    )
    .await;

    let gone = cdp(
        &mut ctx,
        4,
        "Runtime.evaluate",
        json!({"expression": "typeof globalThis.obscuraProbe", "returnByValue": true}),
        Some(&session),
    )
    .await;
    assert_eq!(gone["result"]["value"], "undefined");
    assert!(
        binding_calls(&ctx).is_empty(),
        "a removed binding still delivered a call"
    );
}
