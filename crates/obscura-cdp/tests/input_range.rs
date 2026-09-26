use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
async fn call(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let result = dispatch(
        &CdpRequest {
            id: 1,
            method: method.into(),
            params,
            session_id: Some("range".into()),
        },
        ctx,
    )
    .await;
    assert!(result.error.is_none(), "{result:?}");
    result.result.unwrap()
}
async fn js(ctx: &mut CdpContext, code: &str) -> Value {
    call(
        ctx,
        "Runtime.evaluate",
        json!({"expression":code,"returnByValue":true}),
    )
    .await["result"]["value"]
        .clone()
}
async fn setup() -> CdpContext {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("range".into(), page);
    js(&mut ctx, "document.body.innerHTML='<input id=r type=range min=0 max=100 step=10 value=50>';globalThis.r=document.getElementById('r');globalThis.events=[];for(const t of ['input','change'])r.addEventListener(t,e=>events.push([t,r.value,e.isTrusted]));r.getBoundingClientRect=()=>({left:0,top:0,width:200,height:20});document.elementFromPoint=()=>r;").await;
    ctx
}
#[tokio::test(flavor = "current_thread")]
async fn range_sanitizes_values_without_script_events() {
    let mut ctx = setup().await;
    assert_eq!(
        js(
            &mut ctx,
            "JSON.stringify(['','bad','95','-10','101','0x10'].map(v=>{r.value=v;return r.value}))"
        )
        .await,
        json!("[\"50\",\"50\",\"100\",\"0\",\"100\",\"50\"]")
    );
    assert_eq!(js(&mut ctx, "events.length").await, json!(0.0));
    assert_eq!(js(&mut ctx,"r.removeAttribute('min');r.removeAttribute('max');r.removeAttribute('step');r.removeAttribute('value');r.value='';r.value").await,json!("50"));
}
#[tokio::test(flavor = "current_thread")]
async fn range_pointer_drag_commits_once_and_keyboard_cancels() {
    let mut ctx = setup().await;
    call(
        &mut ctx,
        "Input.dispatchMouseEvent",
        json!({"type":"mousePressed","x":150,"y":10,"button":"left"}),
    )
    .await;
    call(
        &mut ctx,
        "Input.dispatchMouseEvent",
        json!({"type":"mouseMoved","x":190,"y":10,"buttons":1}),
    )
    .await;
    call(
        &mut ctx,
        "Input.dispatchMouseEvent",
        json!({"type":"mouseReleased","x":190,"y":10,"button":"left"}),
    )
    .await;
    assert_eq!(
        js(&mut ctx, "JSON.stringify(events)").await,
        json!("[[\"input\",\"80\",true],[\"input\",\"100\",true],[\"change\",\"100\",true]]")
    );
    js(&mut ctx, "events=[];r.value='50';r.focus()").await;
    call(
        &mut ctx,
        "Input.dispatchKeyEvent",
        json!({"type":"rawKeyDown","key":"ArrowRight"}),
    )
    .await;
    assert_eq!(js(&mut ctx, "r.value").await, json!("60"));
    js(
        &mut ctx,
        "events=[];r.addEventListener('keydown',e=>e.preventDefault())",
    )
    .await;
    call(
        &mut ctx,
        "Input.dispatchKeyEvent",
        json!({"type":"rawKeyDown","key":"End"}),
    )
    .await;
    assert_eq!(
        js(&mut ctx, "r.value+':'+events.length").await,
        json!("60:0")
    );
}
#[tokio::test(flavor = "current_thread")]
async fn range_disabled_cancelled_and_secondary_pointer_do_not_change() {
    let mut ctx = setup().await;
    for setup in [
        "r.disabled=true",
        "r.disabled=false;r.onmousedown=e=>e.preventDefault()",
        "r.onmousedown=null",
    ] {
        js(&mut ctx, setup).await;
        let button = if setup == "r.onmousedown=null" {
            "right"
        } else {
            "left"
        };
        for kind in ["mousePressed", "mouseReleased"] {
            call(
                &mut ctx,
                "Input.dispatchMouseEvent",
                json!({"type":kind,"x":190,"y":10,"button":button}),
            )
            .await;
        }
        assert_eq!(
            js(&mut ctx, "r.value+':'+events.length").await,
            json!("50:0")
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn range_rtl_attribute_reverses_horizontal_keys() {
    let mut ctx = setup().await;
    js(&mut ctx, "r.setAttribute('dir','rtl');r.focus()").await;
    call(
        &mut ctx,
        "Input.dispatchKeyEvent",
        json!({"type":"rawKeyDown","key":"ArrowRight"}),
    )
    .await;
    assert_eq!(js(&mut ctx, "r.value").await, json!("40"));
}
