use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let result = dispatch(
        &CdpRequest {
            id: 1,
            method: method.into(),
            params,
            session_id: Some("input".into()),
        },
        ctx,
    )
    .await;
    assert!(result.error.is_none(), "{method}: {:?}", result.error);
    result.result.unwrap()
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

async fn setup(html: &str) -> CdpContext {
    let mut ctx = CdpContext::new();
    let id = ctx.create_page();
    ctx.sessions.insert("input".into(), id);
    cdp(
        &mut ctx,
        "Page.navigate",
        json!({"url": "data:text/html,<body></body>"}),
    )
    .await;
    evaluate(
        &mut ctx,
        &format!(
            "document.body.innerHTML = {}; globalThis.seen = [];",
            json!(html)
        ),
    )
    .await;
    ctx
}

async fn tab(ctx: &mut CdpContext, modifiers: u8) {
    for kind in ["rawKeyDown", "keyUp"] {
        cdp(ctx, "Input.dispatchKeyEvent", json!({
            "type": kind, "key": "Tab", "code": "Tab", "windowsVirtualKeyCode": 9, "modifiers": modifiers,
        })).await;
    }
}

async fn edit(ctx: &mut CdpContext) {
    cdp(ctx, "Input.insertText", json!({"text": "3"})).await;
}

#[tokio::test(flavor = "current_thread")]
async fn tab_commits_number_edits_before_blur_and_keyup_reaches_the_next_control() {
    let mut ctx = setup("<input id=field type=number value=1><button id=next>Next</button>").await;
    evaluate(&mut ctx, "field.focus(); field.value = ''; for (const type of ['input','keydown','change','blur','focusout','focus','focusin','keyup']) document.addEventListener(type,e=>seen.push([e.type,e.target.id,document.activeElement.id,e.isTrusted]),true);").await;
    edit(&mut ctx).await;
    tab(&mut ctx, 0).await;
    let actual = evaluate(
        &mut ctx,
        "JSON.stringify({active:document.activeElement.id,value:field.value,seen})",
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(actual.as_str().unwrap()).unwrap(),
        json!({"active":"next","value":"3","seen":[
            ["input","field","field",true],["keydown","field","field",true],
            ["change","field","",true],["blur","field","",true],
            ["focusout","field","",true],["focus","next","next",true],
            ["focusin","next","next",true],["keyup","next","next",true]
        ]})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn focus_changes_commit_user_edits_once_but_ignore_script_only_changes() {
    for tag in ["input", "textarea"] {
        for leave in ["field.blur()", "next.focus()"] {
            let mut ctx = setup(&format!(
                "<{tag} id=field></{tag}><button id=next>Next</button>"
            ))
            .await;
            evaluate(&mut ctx, "field.value='a'; field.onchange=e=>seen.push([field.value,e.isTrusted]); field.focus();").await;
            edit(&mut ctx).await;
            evaluate(&mut ctx, leave).await;
            evaluate(
                &mut ctx,
                "field.blur(); field.focus(); field.value='script'; field.blur();",
            )
            .await;
            assert_eq!(
                evaluate(&mut ctx, "JSON.stringify(seen)").await,
                json!("[[\"a3\",true]]")
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prevented_tab_keeps_pending_edits_until_focus_is_lost() {
    let mut ctx = setup("<input id=field><button id=next>Next</button>").await;
    evaluate(&mut ctx, "field.focus(); field.onchange=()=>seen.push(field.value); field.onkeydown=e=>e.preventDefault();").await;
    edit(&mut ctx).await;
    tab(&mut ctx, 0).await;
    assert_eq!(
        evaluate(&mut ctx, "JSON.stringify([document.activeElement.id,seen])").await,
        json!("[\"field\",[]]")
    );
    evaluate(&mut ctx, "field.blur()").await;
    assert_eq!(
        evaluate(&mut ctx, "JSON.stringify(seen)").await,
        json!("[\"3\"]")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn returning_to_the_original_value_and_synthetic_input_do_not_commit() {
    let mut ctx = setup("<input id=field value=a><button id=next>Next</button>").await;
    evaluate(
        &mut ctx,
        "field.focus(); field.onchange=()=>seen.push(field.value);",
    )
    .await;
    edit(&mut ctx).await;
    evaluate(&mut ctx, "field.value='a'; field.blur(); field.focus(); field.value='script'; field.dispatchEvent(new Event('input',{bubbles:true})); field.blur();").await;
    assert_eq!(
        evaluate(&mut ctx, "JSON.stringify(seen)").await,
        json!("[]")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tab_honors_positive_order_reverse_direction_and_unavailable_controls() {
    let mut ctx = setup("<button id=two tabindex=2>Two</button><input disabled><input type=hidden><div inert><button>Inert</button></div><div style='display:none'><input></div><button id=one tabindex=1>One</button><input id=last readonly><button tabindex=-1>Skip</button>").await;
    for expected in ["one", "two", "last", "", "one"] {
        tab(&mut ctx, 0).await;
        assert_eq!(
            evaluate(&mut ctx, "document.activeElement.id").await,
            json!(expected)
        );
    }
    tab(&mut ctx, 8).await;
    assert_eq!(
        evaluate(&mut ctx, "document.activeElement.id").await,
        json!("")
    );
    tab(&mut ctx, 8).await;
    assert_eq!(
        evaluate(&mut ctx, "document.activeElement.id").await,
        json!("last")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn change_handler_replacing_the_form_does_not_focus_a_detached_control() {
    let mut ctx = setup("<main><input id=field type=number value=1><button id=next>Next</button></main><output id=total>2</output>").await;
    evaluate(&mut ctx, "field.focus(); field.value=''; field.onchange=()=>{total.textContent=String(Number(field.value)*2); document.querySelector('main').innerHTML='<input value=3><button>Next</button>';};").await;
    edit(&mut ctx).await;
    tab(&mut ctx, 0).await;
    assert_eq!(
        evaluate(
            &mut ctx,
            "JSON.stringify([total.textContent,document.activeElement.isConnected])"
        )
        .await,
        json!("[\"6\",true]")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_change_is_not_repeated_on_blur_but_synthetic_change_does_not_commit() {
    let mut ctx = setup("<input id=field><button id=next>Next</button>").await;
    evaluate(
        &mut ctx,
        "field.focus(); field.onchange=e=>seen.push(e.isTrusted);",
    )
    .await;
    edit(&mut ctx).await;
    evaluate(&mut ctx, "field.dispatchEvent(globalThis.__obscura_markTrusted(new Event('change',{bubbles:true}))); field.blur();").await;
    assert_eq!(
        evaluate(&mut ctx, "JSON.stringify(seen)").await,
        json!("[true]")
    );
    evaluate(&mut ctx, "field.focus(); seen.length=0;").await;
    edit(&mut ctx).await;
    evaluate(
        &mut ctx,
        "field.dispatchEvent(new Event('change',{bubbles:true})); field.blur();",
    )
    .await;
    assert_eq!(
        evaluate(&mut ctx, "JSON.stringify(seen)").await,
        json!("[false,true]")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn checked_radio_groups_have_one_tab_stop() {
    let mut ctx = setup("<button id=first>First</button><input id=radio1 type=radio name=choice><input id=radio2 type=radio name=choice checked><input id=radio3 type=radio name=choice><button id=last>Last</button>").await;
    for expected in ["first", "radio2", "last"] {
        tab(&mut ctx, 0).await;
        assert_eq!(
            evaluate(&mut ctx, "document.activeElement.id").await,
            json!(expected)
        );
    }
    tab(&mut ctx, 8).await;
    assert_eq!(
        evaluate(&mut ctx, "document.activeElement.id").await,
        json!("radio2")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn closed_details_only_exposes_its_first_direct_summary_to_tab() {
    let mut ctx = setup("<details><summary id=summary>Summary</summary><input id=hidden></details><input type=HIDDEN><button id=last>Last</button>").await;
    tab(&mut ctx, 0).await;
    assert_eq!(
        evaluate(&mut ctx, "document.activeElement.id").await,
        json!("summary")
    );
    tab(&mut ctx, 0).await;
    assert_eq!(
        evaluate(&mut ctx, "document.activeElement.id").await,
        json!("last")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn focus_chosen_by_a_change_handler_is_preserved() {
    for target in ["field", "other"] {
        let mut ctx =
            setup("<input id=field value=1><button id=next>Next</button><input id=other>").await;
        evaluate(&mut ctx, &format!("field.focus(); field.onchange=()=>{{seen.push('change'); document.getElementById({}).focus();}};", json!(target))).await;
        edit(&mut ctx).await;
        evaluate(&mut ctx, "next.focus()").await;
        assert_eq!(
            evaluate(&mut ctx, "document.activeElement.id").await,
            json!(target)
        );
        assert_eq!(
            evaluate(&mut ctx, "JSON.stringify(seen)").await,
            json!("[\"change\"]")
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn focusable_body_does_not_skip_the_beginning_of_the_tab_order() {
    let mut ctx =
        setup("<button id=positive tabindex=1>Positive</button><button id=zero>Zero</button>")
            .await;
    evaluate(
        &mut ctx,
        "document.body.id='root'; document.body.tabIndex=0;",
    )
    .await;
    for expected in ["positive", "root", "zero", "root", "positive"] {
        tab(&mut ctx, 0).await;
        assert_eq!(
            evaluate(&mut ctx, "document.activeElement.id").await,
            json!(expected)
        );
    }
}
