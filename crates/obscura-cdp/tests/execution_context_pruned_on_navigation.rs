// Regression for issue #407: navigation must retire a page's old execution
// context ids without deleting live contexts owned by another page.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: &str,
) -> obscura_cdp::types::CdpResponse {
    dispatch(&CdpRequest {
        id,
        method: method.to_string(),
        params,
        session_id: Some(session_id.to_string()),
    }, ctx).await
}

#[tokio::test(flavor = "current_thread")]
async fn navigation_prunes_only_the_navigated_pages_stale_context_ids() {
    let mut ctx = CdpContext::new();
    let first_page = ctx.create_page();
    let second_page = ctx.create_page();
    ctx.sessions.insert("first".to_string(), first_page);
    ctx.sessions.insert("second".to_string(), second_page);

    let stale = cdp(
        &mut ctx, 1, "Page.createIsolatedWorld",
        json!({"worldName": "first-world"}), "first",
    ).await.result.unwrap()["executionContextId"].as_i64().unwrap();
    let other_page = cdp(
        &mut ctx, 2, "Page.createIsolatedWorld",
        json!({"worldName": "second-world"}), "second",
    ).await.result.unwrap()["executionContextId"].as_i64().unwrap();

    cdp(
        &mut ctx, 3, "Page.navigate",
        json!({"url": "data:text/html,<p>replacement</p>", "waitUntil": "load"}),
        "first",
    ).await;

    let stale_result = cdp(
        &mut ctx, 4, "Runtime.evaluate",
        json!({"expression": "1", "contextId": stale}), "first",
    ).await;
    assert!(stale_result.error.unwrap().message.contains("Cannot find context"));
    let other_result = cdp(
        &mut ctx, 5, "Runtime.evaluate",
        json!({"expression": "1", "contextId": other_page}), "second",
    ).await;
    assert!(other_result.error.is_none(), "other page context was pruned");
}
