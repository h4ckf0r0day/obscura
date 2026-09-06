#![cfg(feature = "render")]

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = socket.read(&mut buf).await.unwrap();
        // The labels carry their own boxes so a coordinate click has a target
        // regardless of inline-wrapper geometry.
        let body = r#"<!doctype html><html><head><style>
            html, body { margin: 0; font: 16px monospace }
            label { display: block; width: 200px; height: 40px }
        </style></head><body>
          <label id="explicit" for="boxa">a</label><input id="boxa" type="checkbox">
          <label id="implicit">b <input id="boxb" type="checkbox"></label>
          <label id="deep" for="boxc"><span><b id="deep-text">c</b></span></label>
          <input id="boxc" type="checkbox">
          <label id="off" for="boxd">d</label><input id="boxd" type="checkbox" disabled>
          <script>
            window.events = [];
            for (const id of ['boxa','boxb','boxc','boxd']) {
              const el = document.getElementById(id);
              for (const t of ['click','input','change']) {
                el.addEventListener(t, e => window.events.push(id + ':' + t + ':' + e.isTrusted));
              }
            }
          </script>
        </body></html>"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, sid: &str) -> Value {
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(sid.to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "CDP {method} failed: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn evaluate(ctx: &mut CdpContext, id: u64, expression: &str, sid: &str) -> Value {
    cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
        sid,
    )
    .await
}

async fn setup() -> (CdpContext, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_fixture().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let sid = "label-activation-session";
    ctx.sessions.insert(sid.to_string(), page_id);
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), sid).await;
    (ctx, sid.to_string())
}

/// Click the centre of `selector` the way a real pointer would.
async fn click_element(ctx: &mut CdpContext, id: u64, sid: &str, selector: &str) {
    let rect = evaluate(
        ctx,
        id,
        &format!(
            "JSON.stringify(document.querySelector('{selector}').getBoundingClientRect().toJSON())"
        ),
        sid,
    )
    .await;
    let rect: Value = serde_json::from_str(rect["result"]["value"].as_str().unwrap()).unwrap();
    let x = rect["x"].as_f64().unwrap() + rect["width"].as_f64().unwrap() / 2.0;
    let y = rect["y"].as_f64().unwrap() + rect["height"].as_f64().unwrap() / 2.0;
    for kind in ["mousePressed", "mouseReleased"] {
        cdp(
            ctx,
            id + 1,
            "Input.dispatchMouseEvent",
            json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
            sid,
        )
        .await;
    }
}

async fn state(ctx: &mut CdpContext, id: u64, sid: &str) -> Value {
    let result = evaluate(
        ctx,
        id,
        r#"JSON.stringify({
            a: document.getElementById('boxa').checked,
            b: document.getElementById('boxb').checked,
            c: document.getElementById('boxc').checked,
            d: document.getElementById('boxd').checked,
            events: window.events.join(',')
        })"#,
        sid,
    )
    .await;
    serde_json::from_str(result["result"]["value"].as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn mouse_click_on_a_label_activates_its_control() {
    let (mut ctx, sid) = setup().await;
    click_element(&mut ctx, 10, &sid, "#explicit").await;
    click_element(&mut ctx, 20, &sid, "#implicit").await;
    let state = state(&mut ctx, 30, &sid).await;
    assert_eq!(state["a"], true, "explicit for= label: {state}");
    assert_eq!(state["b"], true, "implicit nested label: {state}");
    assert_eq!(
        state["events"],
        "boxa:click:true,boxa:input:true,boxa:change:true,\
         boxb:click:true,boxb:input:true,boxb:change:true",
        "each control fires click, input, change once, in order: {state}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mouse_click_deep_inside_a_label_activates_its_control() {
    let (mut ctx, sid) = setup().await;
    click_element(&mut ctx, 10, &sid, "#deep-text").await;
    let state = state(&mut ctx, 20, &sid).await;
    assert_eq!(state["c"], true, "click on a nested element resolves its label: {state}");
}

#[tokio::test(flavor = "current_thread")]
async fn mouse_click_on_a_label_for_a_disabled_control_does_nothing() {
    let (mut ctx, sid) = setup().await;
    click_element(&mut ctx, 10, &sid, "#off").await;
    click_element(&mut ctx, 20, &sid, "#boxd").await;
    let state = state(&mut ctx, 30, &sid).await;
    assert_eq!(state["d"], false, "disabled control must not toggle: {state}");
    assert_eq!(state["events"], "", "disabled control must dispatch nothing: {state}");
}
