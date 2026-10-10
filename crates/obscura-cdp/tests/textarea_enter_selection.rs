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

#[tokio::test(flavor = "current_thread")]
async fn readonly_fields_reject_deletion_and_newlines_but_keep_keyboard_events() {
    let mut ctx = setup().await;
    for tag in ["input", "textarea"] {
        evaluate(&mut ctx, &format!(
            "document.body.innerHTML = '<{tag} id=field></{tag}>'; field.focus();\
             for (const type of ['keydown', 'keypress', 'beforeinput', 'input', 'keyup'])\
             field.addEventListener(type, event => changes.push(event.type));"
        )).await;
        for (key, text, virtual_key) in [("Backspace", "", 8), ("Delete", "", 46), ("Enter", "\r", 13)] {
            if tag == "input" && key == "Enter" { continue; }
            for readonly in [true, false] {
                evaluate(&mut ctx, &format!(
                    "field.toggleAttribute('readonly', {readonly}); field.value = 'abCD';\
                     field.setSelectionRange(1, 3); changes.length = 0;"
                )).await;
                cdp(&mut ctx, "Input.dispatchKeyEvent", json!({
                    "type": "keyDown", "key": key, "text": text,
                    "windowsVirtualKeyCode": virtual_key,
                })).await;
                cdp(&mut ctx, "Input.dispatchKeyEvent", json!({
                    "type": "keyUp", "key": key, "windowsVirtualKeyCode": virtual_key,
                })).await;
                let expected = match (readonly, key) {
                    (true, "Enter") => json!({"value":"abCD", "start":1, "end":3, "changes":["keydown","keypress","keyup"]}),
                    (true, _) => json!({"value":"abCD", "start":1, "end":3, "changes":["keydown","keyup"]}),
                    (false, "Enter") => json!({"value":"a\nD", "start":2, "end":2, "changes":["keydown","keypress","input","keyup"]}),
                    (false, _) => json!({"value":"aD", "start":1, "end":1, "changes":["keydown","input","keyup"]}),
                };
                assert_eq!(state(&mut ctx).await, expected, "{tag} {key} readonly={readonly}");
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn canceled_keydown_preserves_fields_and_delivers_keyup_without_editing_defaults() {
    let mut ctx = setup().await;
    for tag in ["input", "textarea"] {
        evaluate(&mut ctx, &format!(
            "document.body.innerHTML = '<{tag} id=field></{tag}>'; field.focus();\
             field.addEventListener('keydown', event => event.preventDefault());\
             for (const type of ['keydown', 'keypress', 'beforeinput', 'input', 'keyup'])\
             field.addEventListener(type, event => changes.push(event.type));"
        )).await;
        for event_type in ["keyDown", "rawKeyDown"] {
            for (key, text, virtual_key) in [
                ("Z", "Z", 90), ("Backspace", "", 8),
                ("Delete", "", 46), ("Enter", "\r", 13),
            ] {
                evaluate(&mut ctx, "field.value = 'abCD'; field.setSelectionRange(1, 3); changes.length = 0;").await;
                cdp(&mut ctx, "Input.dispatchKeyEvent", json!({
                    "type": event_type, "key": key, "text": text,
                    "windowsVirtualKeyCode": virtual_key,
                })).await;
                cdp(&mut ctx, "Input.dispatchKeyEvent", json!({
                    "type": "keyUp", "key": key, "windowsVirtualKeyCode": virtual_key,
                })).await;
                assert_eq!(state(&mut ctx).await, json!({
                    "value": "abCD", "start": 1, "end": 3,
                    "changes": ["keydown", "keyup"],
                }), "{tag} {event_type} {key}");
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn text_insertion_dispatches_cancelable_beforeinput_and_trusted_input_events() {
    let mut ctx = setup().await;
    for tag in ["input", "textarea"] {
        for (method, params) in [
            ("Input.insertText", json!({"text": "Z"})),
            ("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Z", "text": "Z"})),
            ("Input.dispatchKeyEvent", json!({"type": "char", "text": "Z"})),
        ] {
            evaluate(&mut ctx, &format!(r#"(function() {{
                document.body.innerHTML = '<{tag} id="field"></{tag}>';
                var field = document.getElementById('field');
                field.value = 'abCD'; field.focus(); field.setSelectionRange(1, 3);
                globalThis.editEvents = [];
                for (var type of ['beforeinput', 'input']) field.addEventListener(type, function(e) {{
                    editEvents.push([e.type, e instanceof InputEvent, e.data, e.inputType,
                        e.isTrusted, e.bubbles, e.composed, e.cancelable, field.value]);
                }});
            }})()"#)).await;
            cdp(&mut ctx, method, params).await;
            assert_eq!(evaluate(&mut ctx, "document.getElementById('field').value").await, "aZD");
            assert_eq!(evaluate(&mut ctx, "editEvents").await, json!([
                ["beforeinput", true, "Z", "insertText", true, true, true, true, "abCD"],
                ["input", true, "Z", "insertText", true, true, true, false, "aZD"]
            ]), "{tag}: {method}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn canceled_or_readonly_text_insertion_preserves_value_and_selection() {
    let mut ctx = setup().await;
    for tag in ["input", "textarea"] {
        for guard in [
            "field.addEventListener('beforeinput', function(e) { e.preventDefault(); });",
            "field.readOnly = true;",
            "field.setAttribute('readonly', '');",
        ] {
            evaluate(&mut ctx, &format!(r#"(function() {{
                document.body.innerHTML = '<{tag} id="field"></{tag}>';
                var field = document.getElementById('field');
                field.value = 'abCD'; field.focus(); field.setSelectionRange(1, 3);
                globalThis.editEvents = [];
                for (var type of ['beforeinput', 'input']) field.addEventListener(type, function(e) {{
                    editEvents.push(e.type);
                }});
                {guard}
            }})()"#)).await;
            cdp(&mut ctx, "Input.insertText", json!({"text": "Z"})).await;
            assert_eq!(evaluate(&mut ctx, "(function() { var field = document.getElementById('field'); return [field.value, field.selectionStart, field.selectionEnd, editEvents]; })()").await,
                json!(["abCD", 1, 3, ["beforeinput"]]), "{tag}: {guard}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn text_insertion_reads_value_and_selection_after_beforeinput_handlers() {
    let mut ctx = setup().await;
    evaluate(&mut ctx, r#"field.addEventListener('beforeinput', function() {
            field.value = 'xy'; field.setSelectionRange(0, 1);
        });"#).await;
    for (method, params, caret) in [
        ("Input.insertText", json!({"text": "Z"}), 2),
        ("Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "Z", "text": "Z"}), 1),
        ("Input.dispatchKeyEvent", json!({"type": "char", "text": "Z"}), 1),
    ] {
        evaluate(&mut ctx, "field.value = 'abCD'; field.setSelectionRange(1, 3);").await;
        cdp(&mut ctx, method, params).await;
        let result = state(&mut ctx).await;
        assert_eq!(result["value"], "Zy");
        assert_eq!(result["start"], caret, "{method}");
        assert_eq!(result["end"], caret, "{method}");
    }
}
