use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::{CdpRequest, CdpResponse};
use serde_json::{json, Value};

async fn request(ctx: &mut CdpContext, id: u64, method: &str, params: Value) -> CdpResponse {
    dispatch(&CdpRequest {
        id, method: method.to_string(), params,
        session_id: Some("touch-capability-session".to_string()),
    }, ctx).await
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_touch_sequences_fail_without_fabricating_page_events() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("touch-capability-session".to_string(), page_id);
    let setup = request(&mut ctx, 1, "Runtime.evaluate", json!({
        "expression": "globalThis.seen = []; for (const kind of ['touchstart','touchmove','touchend','touchcancel','pointerdown','pointerup','click']) document.addEventListener(kind, e => seen.push(e.type)); const field = document.createElement('textarea'); field.id = 'field'; document.body.appendChild(field); field.focus();"
    })).await;
    assert!(setup.error.is_none(), "{setup:?}");
    for (index, kind) in ["touchStart", "touchMove", "touchEnd", "touchCancel"].iter().enumerate() {
        let points = if *kind == "touchStart" || *kind == "touchMove" {
            json!([{ "id": 0, "x": 20, "y": 20, "radiusX": 1, "radiusY": 1, "force": 1 }])
        } else { json!([]) };
        let response = request(&mut ctx, index as u64 + 2, "Input.dispatchTouchEvent",
            json!({"type": kind, "touchPoints": points})).await;
        assert!(response.result.is_none(), "touch must not report success: {response:?}");
        let error = response.error.expect("unsupported touch must produce a protocol error");
        assert_eq!(error.code, -32601);
        assert!(error.message.contains("touch input is not implemented"), "{error:?}");
        assert_eq!(response.id, index as u64 + 2);
        assert_eq!(response.session_id.as_deref(), Some("touch-capability-session"));
    }
    let state = request(&mut ctx, 6, "Runtime.evaluate", json!({
        "expression": "JSON.stringify({events: seen, value: document.getElementById('field').value})", "returnByValue": true
    })).await.result.unwrap();
    let state: Value = serde_json::from_str(state["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(state, json!({"events": [], "value": ""}));
    // A rejected touch command must leave the page/session usable for supported input.
    let text = request(&mut ctx, 7, "Input.insertText", json!({"text": "still usable"})).await;
    assert!(text.error.is_none(), "{text:?}");
    let state = request(&mut ctx, 8, "Runtime.evaluate", json!({
        "expression": "document.getElementById('field').value", "returnByValue": true
    })).await.result.unwrap();
    assert_eq!(state["result"]["value"], "still usable");
}
