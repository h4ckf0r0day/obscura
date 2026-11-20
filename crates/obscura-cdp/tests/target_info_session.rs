//! `Target.getTargetInfo` without a targetId must describe the target of the
//! session it was sent on, as Chrome does, not always the browser target.

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

#[tokio::test(flavor = "current_thread")]
async fn page_session_gets_its_own_target_info() {
    let mut ctx = CdpContext::new();
    let (target_id, session) = created_and_attached(&mut ctx).await;
    let result = cdp(&mut ctx, 1, "Target.getTargetInfo", json!({}), Some(&session)).await;
    assert_eq!(result["targetInfo"]["targetId"], json!(target_id));
    assert_eq!(result["targetInfo"]["type"], json!("page"));
    assert_eq!(result["targetInfo"]["canAccessOpener"], json!(false));
}

#[tokio::test(flavor = "current_thread")]
async fn no_session_still_gets_browser_target_info() {
    let mut ctx = CdpContext::new();
    let result = cdp(&mut ctx, 1, "Target.getTargetInfo", json!({}), None).await;
    assert_eq!(result["targetInfo"]["type"], json!("browser"));
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_session_falls_back_to_browser_target_info() {
    let mut ctx = CdpContext::new();
    let result = cdp(&mut ctx, 1, "Target.getTargetInfo", json!({}), Some("nope")).await;
    assert_eq!(result["targetInfo"]["type"], json!("browser"));
}
