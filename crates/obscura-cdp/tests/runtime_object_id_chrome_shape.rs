//! Chrome mints objectIds as `<isolateId>.<executionContextId>.<objectId>`.
//! Patchright derives the main-world context from
//! `parseInt(objectId.split(".")[1])`, so an id of any other shape leaves the
//! client re-sending `Runtime.evaluate globalThis` until its timeout.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn raw(ctx: &mut CdpContext, method: &str, params: Value, session: &str) -> (Option<Value>, bool) {
    let resp = dispatch(
        &CdpRequest { id: 1, method: method.into(), params, session_id: Some(session.into()) },
        ctx,
    )
    .await;
    (resp.result, resp.error.is_some())
}

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value, session: &str) -> Value {
    let (result, failed) = raw(ctx, method, params, session).await;
    assert!(!failed, "CDP {method} failed");
    result.unwrap_or_else(|| json!({}))
}

async fn new_page(ctx: &mut CdpContext, session: &str) {
    let page_id = ctx.create_page();
    ctx.sessions.insert(session.into(), page_id);
    cdp(ctx, "Page.navigate", json!({"url": "about:blank", "waitUntil": "load"}), session).await;
}

fn oid(v: &Value) -> String {
    v.pointer("/result/objectId").and_then(Value::as_str).expect("objectId").to_string()
}

/// Parse like Patchright: the middle segment as an integer.
fn context_of(oid: &str) -> i64 {
    let parts: Vec<&str> = oid.split('.').collect();
    assert_eq!(parts.len() >= 3, true, "not <isolate>.<context>.<id>: {oid}");
    assert!(parts[0].parse::<u64>().is_ok(), "isolate segment: {oid}");
    assert!(parts[2].split("::").next().unwrap().parse::<u64>().is_ok(), "object segment: {oid}");
    parts[1].parse().unwrap_or_else(|_| panic!("context segment: {oid}"))
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_returns_chrome_shaped_ids_stable_per_context() {
    let mut ctx = CdpContext::new();
    new_page(&mut ctx, "s1").await;

    let a = oid(&cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "globalThis"}), "s1").await);
    let b = oid(&cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "({x:1})"}), "s1").await);
    assert_eq!(context_of(&a), context_of(&b));
    assert_ne!(a, b);

    // An isolated world gets its own context id, and objects minted in it say so.
    let world = cdp(&mut ctx, "Page.createIsolatedWorld",
        json!({"worldName": "w"}), "s1").await;
    let world_id = world["executionContextId"].as_i64().expect("context id");
    let c = oid(&cdp(&mut ctx, "Runtime.evaluate",
        json!({"expression": "({y:2})", "contextId": world_id}), "s1").await);
    assert_eq!(context_of(&c), world_id);
    assert_ne!(context_of(&c), context_of(&a));

    // The default context is not left stamped with the isolated world's id.
    let d = oid(&cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "({z:3})"}), "s1").await);
    assert_eq!(context_of(&d), context_of(&a));
}

#[tokio::test(flavor = "current_thread")]
async fn derived_ids_keep_the_owning_context() {
    let mut ctx = CdpContext::new();
    new_page(&mut ctx, "s1").await;
    let world = cdp(&mut ctx, "Page.createIsolatedWorld",
        json!({"worldName": "w"}), "s1").await;
    let world_id = world["executionContextId"].as_i64().unwrap();

    let root = oid(&cdp(&mut ctx, "Runtime.evaluate",
        json!({"expression": "({inner:{k:1}})", "contextId": world_id}), "s1").await);

    // callFunctionOn with no context uses the handle's context.
    let called = oid(&cdp(&mut ctx, "Runtime.callFunctionOn",
        json!({"functionDeclaration": "function(){return this.inner}", "objectId": root}), "s1").await);
    assert_eq!(context_of(&called), world_id);

    // getProperties children keep it, and the `::name` suffix does not break parsing.
    let props = cdp(&mut ctx, "Runtime.getProperties", json!({"objectId": root}), "s1").await;
    let child = props["result"][0]["value"]["objectId"].as_str().expect("child id").to_string();
    assert!(child.contains("::"));
    assert_eq!(context_of(&child), world_id);

    // Child ids stay usable as handles.
    let v = cdp(&mut ctx, "Runtime.callFunctionOn",
        json!({"functionDeclaration": "function(){return this.k}", "objectId": child, "returnByValue": true}), "s1").await;
    assert_eq!(v["result"]["value"].as_f64(), Some(1.0));

    // Released handles are rejected, a malformed id is an error and not a panic.
    cdp(&mut ctx, "Runtime.releaseObject", json!({"objectId": root}), "s1").await;
    let (_, failed) = raw(&mut ctx, "Runtime.callFunctionOn",
        json!({"functionDeclaration": "function(){return 1}", "objectId": root}), "s1").await;
    assert!(failed, "released objectId must not resolve");
    for bad in ["", "garbage", "1.2", "..", "1.99999999999999999999.3", "{\"injectedScriptId\":1,\"id\":1}"] {
        let (_, failed) = raw(&mut ctx, "Runtime.callFunctionOn",
            json!({"functionDeclaration": "function(){return 1}", "objectId": bad}), "s1").await;
        assert!(failed, "bogus objectId {bad:?} must be an error");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn awaited_evaluation_and_two_pages_do_not_cross_contexts() {
    let mut ctx = CdpContext::new();
    new_page(&mut ctx, "s1").await;
    new_page(&mut ctx, "s2").await;

    let p1 = oid(&cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "globalThis"}), "s1").await);
    let p2 = oid(&cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "globalThis"}), "s2").await);
    assert_ne!(context_of(&p1), context_of(&p2), "pages own distinct contexts");

    let w1 = cdp(&mut ctx, "Page.createIsolatedWorld", json!({"worldName": "w"}), "s1").await;
    let w1_id = w1["executionContextId"].as_i64().unwrap();
    let awaited = oid(&cdp(&mut ctx, "Runtime.evaluate",
        json!({"expression": "Promise.resolve({a:1})", "awaitPromise": true, "contextId": w1_id}), "s1").await);
    assert_eq!(context_of(&awaited), w1_id);

    // Page 1's last command targeted its isolated world; page 2 is unaffected.
    let again = oid(&cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "({})"}), "s2").await);
    assert_eq!(context_of(&again), context_of(&p2));

    // Cross-session use of a handle must not resolve.
    let (_, failed) = raw(&mut ctx, "Runtime.callFunctionOn",
        json!({"functionDeclaration": "function(){return 1}", "objectId": p1}), "s2").await;
    assert!(failed, "page 1's handle must not resolve on page 2");
}

#[tokio::test(flavor = "current_thread")]
async fn dom_resolve_node_returns_chrome_shaped_ids() {
    let mut ctx = CdpContext::new();
    new_page(&mut ctx, "s1").await;
    cdp(&mut ctx, "Runtime.evaluate",
        json!({"expression": "document.body.innerHTML='<h1 id=t>x</h1>'"}), "s1").await;
    let doc = cdp(&mut ctx, "DOM.getDocument", json!({}), "s1").await;
    let root = doc["root"]["nodeId"].as_i64().unwrap();
    let q = cdp(&mut ctx, "DOM.querySelector", json!({"nodeId": root, "selector": "h1"}), "s1").await;
    let node = q["nodeId"].as_i64().unwrap();
    let r = cdp(&mut ctx, "DOM.resolveNode", json!({"nodeId": node}), "s1").await;
    let id = r["object"]["objectId"].as_str().expect("objectId").to_string();
    let _ = context_of(&id);
    let t = cdp(&mut ctx, "Runtime.callFunctionOn",
        json!({"functionDeclaration": "function(){return this.id}", "objectId": id, "returnByValue": true}), "s1").await;
    assert_eq!(t["result"]["value"], json!("t"));
}
