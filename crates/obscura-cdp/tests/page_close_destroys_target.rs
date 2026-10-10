//! `Page.close` closed nothing: obscura fell through to its
//! `Unknown Page method` catch-all, so a go-rod client that closed a tab got an
//! error back and - worse - no `Target.targetDestroyed` event. rod's `Page.Close`
//! sends `Page.close` and then sits in a `for msg := range messages` loop until
//! that event arrives (rod/page.go), so every crawled URL ended with
//! `Failed close page: Unknown Page method: close` and the page stayed in the
//! browser's page map.
//!
//! The teardown is the one `Target.closeTarget` already performs: detach each
//! attached session, announce the destruction, drop the page. Chromium answers
//! `{}` to `Page.close` too, so both methods share that helper.

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

fn emitted(ctx: &CdpContext, event: &str) -> Vec<Value> {
    ctx.pending_events
        .iter()
        .filter(|e| e.method == event)
        .map(|e| e.params.clone())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn page_close_destroys_the_target_and_emits_target_destroyed() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(&mut ctx, 1, "Page.close", json!({}), session_id).await;

    assert!(
        ctx.pages.iter().all(|p| p.id != page_id),
        "Page.close must drop the page, pages left: {:?}",
        ctx.pages.iter().map(|p| &p.id).collect::<Vec<_>>()
    );

    let destroyed = emitted(&ctx, "Target.targetDestroyed");
    assert!(
        destroyed
            .iter()
            .any(|p| p.get("targetId").and_then(|v| v.as_str()) == Some(page_id.as_str())),
        "rod waits for Target.targetDestroyed before Close returns; events: {destroyed:#?}"
    );

    let detached = emitted(&ctx, "Target.detachedFromTarget");
    assert!(
        detached
            .iter()
            .any(|p| p.get("sessionId").and_then(|v| v.as_str()) == Some(session_id)),
        "the attached session must be detached too; events: {detached:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stop_loading_answers_success_like_chromium() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    let result = cdp(&mut ctx, 1, "Page.stopLoading", json!({}), session_id).await;

    assert_eq!(result, json!({}), "go-rod calls this before every navigate");
}
