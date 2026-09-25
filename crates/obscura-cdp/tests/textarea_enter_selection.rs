use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some("textarea".to_string()),
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
    ctx.sessions.insert("textarea".to_string(), page_id);
    cdp(
        &mut ctx,
        "Page.navigate",
        json!({"url": "data:text/html,<textarea id='field'></textarea>", "waitUntil": "load"}),
    )
    .await;
    evaluate(&mut ctx, "field.focus(); globalThis.changes = []; for (const type of ['keydown', 'keypress', 'input']) field.addEventListener(type, event => changes.push({type, value: field.value, start: field.selectionStart, end: field.selectionEnd, trusted: event.isTrusted}));").await;
    ctx
}

async fn enter(ctx: &mut CdpContext, event_type: &str) {
    cdp(
        ctx,
        "Input.dispatchKeyEvent",
        json!({"type": event_type, "key": "Enter", "code": "Enter", "text": "\r", "windowsVirtualKeyCode": 13}),
    ).await;
}

async fn state(ctx: &mut CdpContext) -> Value {
    let serialized = evaluate(ctx, "JSON.stringify({value: field.value, start: field.selectionStart, end: field.selectionEnd, changes})").await;
    serde_json::from_str(serialized.as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn enter_splices_at_the_caret_or_replaces_the_selected_range() {
    let mut ctx = setup().await;
    for (value, start, end, expected, caret, event_type) in [
        ("ab", 1, 1, "a\nb", 2, "keyDown"),
        ("abcd", 1, 3, "a\nd", 2, "keyDown"),
        ("ab", 0, 0, "\nab", 1, "rawKeyDown"),
        ("ab", 2, 2, "ab\n", 3, "keyDown"),
        ("a😀b", 1, 3, "a\nb", 2, "keyDown"),
        ("", 0, 0, "\n", 1, "keyDown"),
    ] {
        evaluate(
            &mut ctx,
            &format!(
                "field.value = {}; field.setSelectionRange({start}, {end}); changes.length = 0;",
                json!(value)
            ),
        )
        .await;
        enter(&mut ctx, event_type).await;
        let result = state(&mut ctx).await;
        assert_eq!(
            result["value"], expected,
            "selection {start}..{end} in {value:?}"
        );
        assert_eq!(result["start"], caret);
        assert_eq!(result["end"], caret);
        let changes = result["changes"].as_array().unwrap();
        assert_eq!(
            changes
                .iter()
                .map(|change| change["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["keydown", "keypress", "input"]
        );
        assert_eq!(changes[2]["value"], expected);
        assert_eq!(changes[2]["start"], caret);
        assert_eq!(changes[2]["end"], caret);
        assert_eq!(changes[2]["trusted"], true);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn typing_continues_after_the_inserted_newline() {
    let mut ctx = setup().await;
    cdp(&mut ctx, "Input.insertText", json!({"text": "a"})).await;
    enter(&mut ctx, "keyDown").await;
    cdp(&mut ctx, "Input.insertText", json!({"text": "b"})).await;
    let result = state(&mut ctx).await;
    assert_eq!(result["value"], "a\nb");
    assert_eq!(result["start"], 3);
    assert_eq!(result["end"], 3);
}
