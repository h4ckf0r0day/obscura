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
        let body = r#"<!doctype html><html><head><style>
          input { display: block; width: 40px; height: 40px; margin: 10px }
        </style></head><body>
          <input id="box" type="checkbox">
          <input id="first" type="radio" name="choice" checked>
          <input id="second" type="radio" name="choice">
          <script>
            window.writes = []; window.events = []; window.cancel = false;
            for (const id of ['box', 'first', 'second']) {
              const el = document.getElementById(id);
              if (id === 'box') el.indeterminate = true;
              for (const key of ['checked', 'indeterminate']) {
                let proto = Object.getPrototypeOf(el), descriptor;
                while (proto && !(descriptor = Object.getOwnPropertyDescriptor(proto, key))) {
                  proto = Object.getPrototypeOf(proto);
                }
                Object.defineProperty(el, key, {
                  configurable: true,
                  get() { return descriptor.get.call(this); },
                  set(value) { writes.push(id + ':' + key); descriptor.set.call(this, value); }
                });
              }
              for (const kind of ['click', 'input', 'change']) {
                el.addEventListener(kind, e => {
                  events.push([id, kind, el.checked, el.indeterminate]);
                  if (kind === 'click' && cancel) e.preventDefault();
                });
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
    let sid = "author-accessors-session";
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

async fn snapshot(ctx: &mut CdpContext, sid: &str) -> Value {
    let value = evaluate(ctx, 90, r#"JSON.stringify({
        box: document.getElementById('box').checked,
        indeterminate: document.getElementById('box').indeterminate,
        first: document.getElementById('first').checked,
        second: document.getElementById('second').checked,
        writes, events
    })"#, sid).await;
    serde_json::from_str(value["result"]["value"].as_str().unwrap()).unwrap()
}

async fn activate(ctx: &mut CdpContext, sid: &str, id: &str, pointer: bool) {
    if pointer {
        click_element(ctx, 20, sid, &format!("#{id}")).await;
    } else {
        evaluate(ctx, 20, &format!("document.getElementById('{id}').click()"), sid).await;
    }
}

async fn verify_activation(pointer: bool) {
    let (mut ctx, sid) = setup().await;
    activate(&mut ctx, &sid, "box", pointer).await;
    let s = snapshot(&mut ctx, &sid).await;
    assert_eq!(s["writes"], json!([]), "native activation must bypass author setters: {s}");
    assert_eq!(s["box"], true);
    assert_eq!(s["indeterminate"], false);
    assert_eq!(s["events"], json!([
        ["box", "click", true, false], ["box", "input", true, false], ["box", "change", true, false]
    ]));
    evaluate(&mut ctx, 30, "events = []", &sid).await;
    activate(&mut ctx, &sid, "second", pointer).await;
    let s = snapshot(&mut ctx, &sid).await;
    assert_eq!(s["writes"], json!([]), "radio peers must bypass author setters: {s}");
    assert_eq!(s["first"], false);
    assert_eq!(s["second"], true);
    assert_eq!(s["events"], json!([
        ["second", "click", true, false], ["second", "input", true, false], ["second", "change", true, false]
    ]));
    evaluate(&mut ctx, 40, "document.getElementById('box').checked = false", &sid).await;
    let s = snapshot(&mut ctx, &sid).await;
    assert_eq!(s["writes"], json!(["box:checked"]), "script assignment still invokes author setter");
    assert_eq!(s["box"], false);
}

async fn verify_cancellation(pointer: bool) {
    let (mut ctx, sid) = setup().await;
    evaluate(&mut ctx, 10, "cancel = true", &sid).await;
    activate(&mut ctx, &sid, "box", pointer).await;
    activate(&mut ctx, &sid, "second", pointer).await;
    let s = snapshot(&mut ctx, &sid).await;
    assert_eq!(s["writes"], json!([]), "activation and rollback bypass author setters: {s}");
    assert_eq!(s["box"], false);
    assert_eq!(s["indeterminate"], true);
    assert_eq!(s["first"], true);
    assert_eq!(s["second"], false);
    assert_eq!(s["events"], json!([
        ["box", "click", true, false], ["second", "click", true, false]
    ]), "canceled clicks expose pre-activation state but emit no input/change");
}

#[tokio::test(flavor = "current_thread")]
async fn native_pointer_activation_bypasses_author_setters() { verify_activation(true).await; }
#[tokio::test(flavor = "current_thread")]
async fn element_click_activation_bypasses_author_setters() { verify_activation(false).await; }
#[tokio::test(flavor = "current_thread")]
async fn native_pointer_cancellation_restores_internal_state() { verify_cancellation(true).await; }
#[tokio::test(flavor = "current_thread")]
async fn element_click_cancellation_restores_internal_state() { verify_cancellation(false).await; }
