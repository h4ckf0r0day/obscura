//! #1005 — Backspace must delete a whole supplementary character (surrogate
//! pair), not a single UTF-16 code unit that would leave a lone surrogate.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some("ta".to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn evaluate(ctx: &mut CdpContext, expression: &str) -> Value {
    let result = cdp(
        ctx,
        "Runtime.evaluate",
        json!({"expression": expression, "returnByValue": true}),
    )
    .await;
    assert!(result.get("exceptionDetails").is_none(), "{result}");
    result["result"]["value"].clone()
}

async fn setup() -> CdpContext {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("ta".to_string(), page_id);
    cdp(
        &mut ctx,
        "Page.navigate",
        json!({"url": "data:text/html,<textarea id='field'></textarea>", "waitUntil": "load"}),
    )
    .await;
    evaluate(&mut ctx, "field.focus();").await;
    ctx
}

#[tokio::test(flavor = "current_thread")]
async fn backspace_deletes_a_whole_surrogate_pair() {
    let mut ctx = setup().await;
    // "a<emoji>b" is JS length 4 (emoji = 2 code units); caret at 3 is between
    // the emoji and 'b'.
    evaluate(
        &mut ctx,
        "field.value = 'a\\u{1F600}b'; field.setSelectionRange(3, 3);",
    )
    .await;
    cdp(
        &mut ctx,
        "Input.dispatchKeyEvent",
        json!({"type": "keyDown", "key": "Backspace", "code": "Backspace"}),
    )
    .await;
    assert_eq!(
        evaluate(&mut ctx, "field.value").await,
        json!("ab"),
        "Backspace must remove the whole emoji, not split the surrogate pair"
    );
    assert_eq!(
        evaluate(&mut ctx, "field.selectionStart").await.as_f64(),
        Some(1.0),
        "caret must move to before the deleted character"
    );
}
