// Conformance parity: a handful of standard Web APIs that real Chrome
// exposes but bootstrap.js left undefined, so vendor JS that feature-detects
// them dies with "X is not a function". Each case fails on main, passes
// after the stubs land. These are spec surface gaps, not anti-bot work.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_once() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = "<html><body><div id=a></div></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
    });
    format!("http://{addr}/")
}

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: &str,
) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

async fn eval(ctx: &mut CdpContext, id: u64, expr: &str, session_id: &str) -> Value {
    cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
        session_id,
    )
    .await
}

async fn setup() -> (CdpContext, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_once().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());
    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": url, "waitUntil": "load"}),
        session_id,
    )
    .await;
    (ctx, session_id.to_string())
}

#[tokio::test(flavor = "current_thread")]
async fn element_toggle_attribute_is_callable_and_toggles() {
    let (mut ctx, sid) = setup().await;
    // Element.prototype.toggleAttribute is on every real Chrome. Bootstrap
    // leaves it undefined, so any framework that calls el.toggleAttribute()
    // (Lit, Stencil, several ad SDKs) throws.
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            const el = document.getElementById('a');
            const t = typeof el.toggleAttribute;
            const first = el.toggleAttribute('hidden');
            const afterFirst = el.hasAttribute('hidden');
            const second = el.toggleAttribute('hidden');
            const afterSecond = el.hasAttribute('hidden');
            return JSON.stringify({ type: t, first, afterFirst, second, afterSecond });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["type"], "function");
    assert_eq!(val["first"], true, "first toggle should add the attribute");
    assert_eq!(val["afterFirst"], true);
    assert_eq!(val["second"], false, "second toggle should remove it");
    assert_eq!(val["afterSecond"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn document_adopt_node_moves_node() {
    let (mut ctx, sid) = setup().await;
    // document.adoptNode is standard DOM; bootstrap leaves it undefined.
    // Frameworks that move nodes between documents (iframes, portals) call it.
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            const t = typeof document.adoptNode;
            const span = document.createElement('span');
            span.id = 'moved';
            const adopted = document.adoptNode(span);
            return JSON.stringify({
                type: t,
                sameNode: adopted === span,
                ownerDoc: adopted.ownerDocument === document,
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["type"], "function");
    assert_eq!(val["sameNode"], true);
    assert_eq!(val["ownerDoc"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn assignment_shadows_window_named_element_property() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            'use strict';
            const named = window.a;
            window.a = 42;
            const descriptor = Object.getOwnPropertyDescriptor(window, 'a');
            return JSON.stringify({
                startedAsElement: named === document.getElementById('a'),
                value: window.a,
                writable: descriptor.writable,
                hasGetter: typeof descriptor.get === 'function',
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["startedAsElement"], true);
    assert_eq!(val["value"], 42);
    assert_eq!(val["writable"], true);
    assert_eq!(val["hasGetter"], false);
}

// `window.origin` (and the bare `origin` identifier) is a real Window global,
// not just `location.origin`. Code that reads the bare identifier (e.g.
// Quasar's boot sequence) throws ReferenceError if it is missing. Chrome
// exposes it as a replaceable accessor: the descriptor is configurable with a
// getter and a setter, and `origin = "x"` succeeds (shadowing it with an own
// data property) even in strict-mode page code. These guard that shape.
#[tokio::test(flavor = "current_thread")]
async fn window_origin_is_a_bare_global_mirroring_location_origin() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            // Bare identifier resolution: no `window.` qualifier.
            const bare = origin;
            return JSON.stringify({
                bareResolves: bare === location.origin,
                windowSame: window.origin === location.origin,
                globalThisSame: globalThis.origin === location.origin,
                nonEmpty: typeof bare === 'string' && bare.length > 0,
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["bareResolves"], true, "bare `origin` must resolve to location.origin");
    assert_eq!(val["windowSame"], true);
    assert_eq!(val["globalThisSame"], true);
    assert_eq!(val["nonEmpty"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn window_origin_descriptor_is_a_replaceable_accessor() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            const d = Object.getOwnPropertyDescriptor(window, 'origin');
            return JSON.stringify({
                configurable: d.configurable,
                enumerable: d.enumerable,
                hasGetter: typeof d.get === 'function',
                hasSetter: typeof d.set === 'function',
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    // Headless Chrome parity: configurable accessor with both a getter and a
    // setter. A non-configurable getter-only property (the previous shape)
    // makes strict-mode assignment throw and blocks redefinition.
    assert_eq!(val["configurable"], true, "origin must be configurable, like Chrome");
    assert_eq!(val["enumerable"], true);
    assert_eq!(val["hasGetter"], true);
    assert_eq!(val["hasSetter"], true, "origin must have a setter so assignment does not throw");
}

#[tokio::test(flavor = "current_thread")]
async fn assigning_window_origin_shadows_it_in_strict_mode() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            'use strict';
            const before = origin;
            // In a getter-only world this assignment throws
            // "Cannot set property origin ... which has only a getter".
            origin = 'override';
            const d = Object.getOwnPropertyDescriptor(window, 'origin');
            return JSON.stringify({
                beforeWasLocationOrigin: before === location.origin,
                value: window.origin,
                bareValue: origin,
                writable: d.writable === true,
                hasGetter: typeof d.get === 'function',
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["beforeWasLocationOrigin"], true);
    // Replaceable: assignment succeeds and shadows the accessor with a writable
    // data property, matching window.event / window.scheduler.
    assert_eq!(val["value"], "override");
    assert_eq!(val["bareValue"], "override");
    assert_eq!(val["writable"], true);
    assert_eq!(val["hasGetter"], false, "assignment must replace the accessor with a data property");
}
