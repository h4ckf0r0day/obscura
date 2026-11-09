//! Regression test for the protocol half of issue #600: `Page.getFrameTree`
//! reported `childFrames: []` however many frames a page had built, and no
//! `Page.frameAttached` was ever emitted, so a Playwright or Puppeteer client
//! saw a single-frame page and could never address the child.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// `/` embeds `/child.html`, which itself embeds `/grandchild.html`, so the
/// tree is deep enough to show nesting rather than a flat list.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let body = if request.starts_with("GET /child.html ") {
                    "<html><body><h2>Child heading</h2><iframe src=\"/grandchild.html\"></iframe></body></html>"
                } else if request.starts_with("GET /grandchild.html ") {
                    "<html><body><p>deep</p></body></html>"
                } else {
                    "<html><body><h1>Main heading</h1><iframe src=\"/child.html\"></iframe></body></html>"
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session: &str) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session.to_string()),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

/// Gets a page and a session the way a real client does, rather than by
/// inserting a session for a hand-made page: `Target.createTarget` then
/// `Target.attachToTarget`, which is the only route Puppeteer and Playwright
/// can take and therefore the only one worth asserting against.
async fn attached_session(ctx: &mut CdpContext) -> String {
    let created = dispatch(
        &CdpRequest {
            id: 900,
            method: "Target.createTarget".to_string(),
            params: json!({"url": "about:blank"}),
            session_id: None,
        },
        ctx,
    )
    .await
    .result
    .expect("Target.createTarget produced no result");
    let target_id = created["targetId"].as_str().expect("no targetId").to_string();

    let attached = dispatch(
        &CdpRequest {
            id: 901,
            method: "Target.attachToTarget".to_string(),
            params: json!({"targetId": target_id, "flatten": true}),
            session_id: None,
        },
        ctx,
    )
    .await
    .result
    .expect("Target.attachToTarget produced no result");
    attached["sessionId"]
        .as_str()
        .expect("no sessionId")
        .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn child_default_and_isolated_contexts_evaluate_their_own_documents() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let session = attached_session(&mut ctx).await;
    cdp(&mut ctx, 1, "Runtime.enable", json!({}), &session).await;
    cdp(&mut ctx, 2, "Page.navigate", json!({"url": url}), &session).await;
    cdp(&mut ctx, 3, "Runtime.evaluate", json!({"expression": "1"}), &session).await;
    let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), &session).await;
    let child = &tree["frameTree"]["childFrames"][0];
    let grandchild = &child["childFrames"][0];

    for frame in [child, grandchild] {
        let id = frame["frame"]["id"].as_str().unwrap();
        let context = ctx.pending_events.iter().find(|event| {
            event.method == "Runtime.executionContextCreated"
                && event.session_id.as_deref() == Some(session.as_str())
                && event.params["context"]["auxData"]["frameId"] == id
                && event.params["context"]["auxData"]["isDefault"] == true
        }).unwrap_or_else(|| panic!("no default execution context for {id}"));
        let context_id = context.params["context"]["id"].as_i64().unwrap();
        let evaluated = cdp(&mut ctx, 5, "Runtime.evaluate", json!({
            "expression": "Promise.resolve(document.URL)",
            "contextId": context_id, "returnByValue": true, "awaitPromise": true,
        }), &session).await;
        assert_eq!(evaluated["result"]["value"], frame["frame"]["url"]);

        let isolated = cdp(&mut ctx, 6, "Page.createIsolatedWorld", json!({
            "frameId": id, "worldName": "child-utility-probe",
        }), &session).await;
        let called = cdp(&mut ctx, 7, "Runtime.callFunctionOn", json!({
            "functionDeclaration": "async function() { return document.URL; }",
            "executionContextId": isolated["executionContextId"],
            "returnByValue": true, "awaitPromise": true,
        }), &session).await;
        assert_eq!(called["result"]["value"], frame["frame"]["url"]);
    }

    let child_id = child["frame"]["id"].as_str().unwrap();
    let isolated = cdp(&mut ctx, 8, "Page.createIsolatedWorld", json!({
        "frameId": child_id, "worldName": "child-utility-probe",
    }), &session).await;
    let handle = cdp(&mut ctx, 9, "Runtime.evaluate", json!({
        "expression": "document.querySelector('h2')",
        "contextId": isolated["executionContextId"], "returnByValue": false,
    }), &session).await;
    assert_eq!(handle["result"]["subtype"], "node");
    let text = cdp(&mut ctx, 10, "Runtime.callFunctionOn", json!({
        "functionDeclaration": "function() { return this.textContent; }",
        "objectId": handle["result"]["objectId"], "returnByValue": true,
    }), &session).await;
    assert_eq!(text["result"]["value"], "Child heading");
    let main = cdp(&mut ctx, 11, "Runtime.evaluate", json!({
        "expression": "document.querySelector('h1').textContent", "returnByValue": true,
    }), &session).await;
    assert_eq!(main["result"]["value"], "Main heading");
}

#[tokio::test(flavor = "current_thread")]
async fn object_and_property_handles_preserve_the_evaluation_world() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let session = attached_session(&mut ctx).await;
    cdp(&mut ctx, 1, "Runtime.enable", json!({}), &session).await;
    cdp(&mut ctx, 2, "Page.navigate", json!({"url":url}), &session).await;
    cdp(&mut ctx, 3, "Runtime.evaluate", json!({"expression":"1"}), &session).await;
    let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), &session).await;
    let child = &tree["frameTree"]["childFrames"][0]["frame"];
    let child_context = ctx.pending_events.iter().find(|event| {
        event.method == "Runtime.executionContextCreated"
            && event.session_id.as_deref() == Some(session.as_str())
            && event.params["context"]["auxData"]["frameId"] == child["id"]
            && event.params["context"]["auxData"]["isDefault"] == true
    }).unwrap().params["context"]["id"].as_i64().unwrap();
    cdp(&mut ctx, 5, "Runtime.evaluate", json!({
        "expression":"globalThis.childObject = {nested:{answer:42}}", "contextId":child_context,
    }), &session).await;
    let cases = [
        ("document.querySelector('h1')", None, url.as_str()),
        ("document.querySelector('h2')", Some(child_context), child["url"].as_str().unwrap()),
        ("document.querySelector('iframe').contentDocument.querySelector('h2')", None, url.as_str()),
        ("document.querySelector('iframe').contentWindow", None, url.as_str()),
        ("childObject", Some(child_context), child["url"].as_str().unwrap()),
        ("document.querySelector('iframe').contentWindow.childObject", None, url.as_str()),
    ];
    let mut child_handles = Vec::new();
    let mut root_handles = Vec::new();
    for (expression, context, expected_url) in cases {
        for awaited in [false, true] {
            let mut params = json!({"expression":expression, "awaitPromise":awaited});
            if let Some(context) = context { params["contextId"] = json!(context); }
            let handle = cdp(&mut ctx, 6, "Runtime.evaluate", params, &session).await;
            let oid = handle["result"]["objectId"].as_str().unwrap().to_string();
            let called = cdp(&mut ctx, 7, "Runtime.callFunctionOn", json!({
                "functionDeclaration":"async function(value) { return [document.URL, Array.isArray(value), value instanceof Array]; }",
                "objectId":oid, "arguments":[{"value":[]}], "awaitPromise":true, "returnByValue":true,
            }), &session).await;
            assert_eq!(called["result"]["value"], json!([expected_url,true,true]), "{expression}");
            if expression.ends_with("childObject") {
                let props = cdp(&mut ctx, 8, "Runtime.getProperties", json!({"objectId":oid}), &session).await;
                let nested = props["result"].as_array().unwrap().iter().find(|prop| prop["name"] == "nested")
                    .unwrap()["value"]["objectId"].as_str().unwrap().to_string();
                let called = cdp(&mut ctx, 9, "Runtime.callFunctionOn", json!({
                    "functionDeclaration":"function() { return [document.URL, this.answer]; }",
                    "objectId":nested, "returnByValue":true,
                }), &session).await;
                assert_eq!(called["result"]["value"], json!([expected_url,42]));
                if context.is_some() { child_handles.push(nested); } else { root_handles.push(nested); }
            }
            if context.is_some() { child_handles.push(oid); } else { root_handles.push(oid); }
        }
    }
    cdp(&mut ctx, 10, "Runtime.evaluate", json!({
        "expression":"document.querySelector('iframe').remove()",
    }), &session).await;
    ctx.get_session_page_mut(&Some(session.clone())).unwrap().settle(1_000).await;
    cdp(&mut ctx, 11, "Runtime.evaluate", json!({"expression":"1"}), &session).await;
    for oid in &child_handles {
        for method in ["Runtime.callFunctionOn", "Runtime.getProperties"] {
            let result = dispatch(&CdpRequest {
                id:12, method:method.to_string(),
                params:json!({"objectId":oid,"functionDeclaration":"function() { return document.URL; }"}),
                session_id:Some(session.clone()),
            }, &mut ctx).await;
            assert_eq!(result.error.unwrap().message, "Execution context was destroyed", "{method}");
        }
    }
    for oid in &root_handles {
        let called = cdp(&mut ctx, 13, "Runtime.callFunctionOn", json!({
            "objectId":oid, "functionDeclaration":"function() { return document.URL; }", "returnByValue":true,
        }), &session).await;
        assert_eq!(called["result"]["value"], url);
    }
    cdp(&mut ctx, 14, "Runtime.releaseObject", json!({"objectId":root_handles[0]}), &session).await;
    let released = dispatch(&CdpRequest {
        id:15, method:"Runtime.getProperties".to_string(),
        params:json!({"objectId":root_handles[0]}), session_id:Some(session.clone()),
    }, &mut ctx).await;
    assert_eq!(released.error.unwrap().message, "Could not find object with given id");
}

#[tokio::test(flavor = "current_thread")]
async fn child_object_handles_reject_a_retired_execution_context() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let session = attached_session(&mut ctx).await;
    cdp(&mut ctx, 1, "Runtime.enable", json!({}), &session).await;
    cdp(&mut ctx, 2, "Page.navigate", json!({"url": url}), &session).await;
    cdp(&mut ctx, 3, "Runtime.evaluate", json!({"expression": "1"}), &session).await;
    let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), &session).await;
    let child = &tree["frameTree"]["childFrames"][0]["frame"];
    let context_id = ctx.pending_events.iter().find(|event| {
        event.method == "Runtime.executionContextCreated"
            && event.session_id.as_deref() == Some(session.as_str())
            && event.params["context"]["auxData"]["frameId"] == child["id"]
            && event.params["context"]["auxData"]["isDefault"] == true
    }).unwrap().params["context"]["id"].as_i64().unwrap();
    let handle = cdp(&mut ctx, 5, "Runtime.evaluate", json!({
        "expression": "({kind:'child'})", "contextId": context_id,
    }), &session).await;
    let live = cdp(&mut ctx, 6, "Runtime.callFunctionOn", json!({
        "functionDeclaration": "function() { return document.URL; }",
        "objectId": handle["result"]["objectId"], "returnByValue": true,
    }), &session).await;
    assert_eq!(live["result"]["value"], child["url"]);
    cdp(&mut ctx, 7, "Runtime.evaluate", json!({
        "expression": "document.querySelector('iframe').remove()",
    }), &session).await;
    ctx.get_session_page_mut(&Some(session.clone())).unwrap().settle(1_000).await;
    cdp(&mut ctx, 8, "Runtime.evaluate", json!({"expression": "1"}), &session).await;
    let tree = cdp(&mut ctx, 9, "Page.getFrameTree", json!({}), &session).await;
    assert!(tree["frameTree"]["childFrames"].as_array().is_none_or(Vec::is_empty));
    let retired = dispatch(&CdpRequest {
        id: 10, method: "Runtime.callFunctionOn".to_string(),
        params: json!({
            "functionDeclaration": "function() { return document.URL; }",
            "objectId": handle["result"]["objectId"], "returnByValue": true,
        }),
        session_id: Some(session.clone()),
    }, &mut ctx).await;
    assert_eq!(retired.error.unwrap().message, "Execution context was destroyed");
    let main = cdp(&mut ctx, 11, "Runtime.evaluate", json!({
        "expression": "document.querySelector('h1').textContent", "returnByValue": true,
    }), &session).await;
    assert_eq!(main["result"]["value"], "Main heading");
}

#[tokio::test(flavor = "current_thread")]
async fn late_runtime_enable_reports_child_default_and_registered_utility_contexts_once() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let session = attached_session(&mut ctx).await;
    let initial = cdp(&mut ctx, 1, "Page.getFrameTree", json!({}), &session).await;
    cdp(&mut ctx, 2, "Page.createIsolatedWorld", json!({
        "frameId": initial["frameTree"]["frame"]["id"], "worldName": "frame-auto-utility",
    }), &session).await;
    cdp(&mut ctx, 2, "Page.navigate", json!({"url": url}), &session).await;
    cdp(&mut ctx, 3, "Runtime.evaluate", json!({"expression": "1"}), &session).await;
    let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), &session).await;
    let child_id = tree["frameTree"]["childFrames"][0]["frame"]["id"].as_str().unwrap();
    ctx.pending_events.clear();
    cdp(&mut ctx, 5, "Runtime.enable", json!({}), &session).await;
    cdp(&mut ctx, 6, "Runtime.enable", json!({}), &session).await;
    let contexts = ctx.pending_events.iter().filter(|event| {
        event.method == "Runtime.executionContextCreated"
            && event.session_id.as_deref() == Some(session.as_str())
            && event.params["context"]["auxData"]["frameId"] == child_id
    }).collect::<Vec<_>>();
    assert_eq!(contexts.iter().filter(|event| {
        event.params["context"]["auxData"]["isDefault"] == true
    }).count(), 1);
    assert_eq!(contexts.iter().filter(|event| {
        event.params["context"]["name"] == "frame-auto-utility"
    }).count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn cross_origin_child_contexts_are_privileged_without_exposing_the_window_to_parent_script() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let html = format!("<!doctype html><h1>Main</h1><iframe src=\"{url}child.html\"></iframe>");
    let encoded = url::form_urlencoded::byte_serialize(html.as_bytes()).collect::<String>()
        .replace('+', "%20");
    let mut ctx = CdpContext::new();
    let session = attached_session(&mut ctx).await;
    cdp(&mut ctx, 1, "Runtime.enable", json!({}), &session).await;
    cdp(&mut ctx, 2, "Page.navigate", json!({
        "url": format!("data:text/html,{encoded}"),
    }), &session).await;
    cdp(&mut ctx, 3, "Runtime.evaluate", json!({"expression": "1"}), &session).await;
    let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), &session).await;
    let child = &tree["frameTree"]["childFrames"][0]["frame"];
    assert!(child["id"].is_string(), "cross-origin frame was not built: {tree}");
    let context_id = ctx.pending_events.iter().find(|event| {
        event.method == "Runtime.executionContextCreated"
            && event.session_id.as_deref() == Some(session.as_str())
            && event.params["context"]["auxData"]["frameId"] == child["id"]
            && event.params["context"]["auxData"]["isDefault"] == true
    }).unwrap().params["context"]["id"].as_i64().unwrap();
    let parent_private = cdp(&mut ctx, 5, "Runtime.evaluate", json!({
        "expression": "document.querySelector('iframe').contentDocument === null",
        "returnByValue": true,
    }), &session).await;
    assert_eq!(parent_private["result"]["value"], true);
    let argument_realm = cdp(&mut ctx, 6, "Runtime.callFunctionOn", json!({
        "functionDeclaration": "function(value) { return this.document.URL === document.URL && Object.getPrototypeOf(value) === Array.prototype; }",
        "executionContextId": context_id, "arguments": [{"value": [1, 2]}],
        "returnByValue": true,
    }), &session).await;
    assert_eq!(argument_realm["result"]["value"], true);
    let window = cdp(&mut ctx, 7, "Runtime.evaluate", json!({
        "expression": "globalThis", "contextId": context_id,
    }), &session).await;
    assert!(window["result"]["objectId"].is_string(), "{window}");
    let from_window = cdp(&mut ctx, 8, "Runtime.callFunctionOn", json!({
        "functionDeclaration": "function() { return this.document.URL; }",
        "objectId": window["result"]["objectId"], "returnByValue": true,
    }), &session).await;
    assert_eq!(from_window["result"]["value"], child["url"]);
    let parent_private = cdp(&mut ctx, 9, "Runtime.evaluate", json!({
        "expression": "document.querySelector('iframe').contentDocument === null",
        "returnByValue": true,
    }), &session).await;
    assert_eq!(parent_private["result"]["value"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn repeated_navigation_reannounces_child_contexts_and_rejects_the_previous_document() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let session = attached_session(&mut ctx).await;
    cdp(&mut ctx, 1, "Runtime.enable", json!({}), &session).await;
    let mut previous_context = None;
    for _ in 0..3 {
        ctx.pending_events.clear();
        cdp(&mut ctx, 2, "Page.navigate", json!({"url": url}), &session).await;
        cdp(&mut ctx, 3, "Runtime.evaluate", json!({"expression": "1"}), &session).await;
        let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), &session).await;
        let child = &tree["frameTree"]["childFrames"][0]["frame"];
        let context = ctx.pending_events.iter().find(|event| {
            event.method == "Runtime.executionContextCreated"
                && event.session_id.as_deref() == Some(session.as_str())
                && event.params["context"]["auxData"]["frameId"] == child["id"]
                && event.params["context"]["auxData"]["isDefault"] == true
        }).expect("replacement document did not advertise a child context");
        let context_id = context.params["context"]["id"].as_i64().unwrap();
        let reply = cdp(&mut ctx, 5, "Runtime.evaluate", json!({
            "expression": "document.URL", "contextId": context_id, "returnByValue": true,
        }), &session).await;
        assert_eq!(reply["result"]["value"], child["url"]);
        if let Some(previous) = previous_context {
            let stale = dispatch(&CdpRequest {
                id: 6, method: "Runtime.evaluate".to_string(),
                params: json!({"expression": "document.URL", "contextId": previous}),
                session_id: Some(session.clone()),
            }, &mut ctx).await;
            assert!(stale.error.is_some(), "previous document's context still routes");
        }
        previous_context = Some(context_id);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn get_frame_tree_reports_nested_child_frames() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let session = &attached_session(&mut ctx).await;

    // Deliberately no `waitUntil`: that is what Puppeteer and Playwright send,
    // and it resolves to DomContentLoaded rather than to load. Passing
    // "load" here hid the frame build behind a readiness level no real client
    // asks for, so the tree came back empty for every one of them.
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url}), session).await;
    // Frames are built when the page settles, which is not part of the
    // navigation itself.
    cdp(&mut ctx, 2, "Runtime.evaluate", json!({"expression": "1"}), session).await;

    let tree = cdp(&mut ctx, 3, "Page.getFrameTree", json!({}), session).await;
    let root = &tree["frameTree"];
    let child = &root["childFrames"][0];
    assert!(
        child["frame"]["url"]
            .as_str()
            .unwrap_or_default()
            .ends_with("/child.html"),
        "no child frame in the tree: {tree}"
    );
    assert_eq!(
        child["frame"]["parentId"], root["frame"]["id"],
        "the child does not point back at the main frame"
    );

    let grandchild = &child["childFrames"][0];
    assert!(
        grandchild["frame"]["url"]
            .as_str()
            .unwrap_or_default()
            .ends_with("/grandchild.html"),
        "a frame inside a frame is missing: {tree}"
    );
    assert_eq!(grandchild["frame"]["parentId"], child["frame"]["id"]);

    // A client builds its frame list from the events, so the tree alone is not
    // enough. They have to carry the session the client attached with, because
    // a client discards anything addressed to a session it does not hold, and
    // Target.createTarget leaves a second session on the same page.
    let child_id = child["frame"]["id"].as_str().unwrap().to_string();
    let page_id = ctx.sessions.get(session.as_str()).cloned().unwrap();
    // Announcing to one arbitrary session of the page is what the ordering of a
    // HashMap decides, so require every session on the page to be told. That
    // makes the client's own session covered whichever one it is.
    for (candidate, owner) in &ctx.sessions {
        if owner != &page_id {
            continue;
        }
        assert!(
            ctx.pending_events.iter().any(|e| {
                e.method == "Page.frameAttached"
                    && e.params["frameId"] == child_id
                    && e.session_id.as_deref() == Some(candidate.as_str())
            }),
            "session {candidate} on the page was never told the child frame attached"
        );
    }

    let mine = |e: &obscura_cdp::types::CdpEvent| e.session_id.as_deref() == Some(session.as_str());
    let attached = ctx
        .pending_events
        .iter()
        .position(|e| {
            e.method == "Page.frameAttached" && e.params["frameId"] == child_id && mine(e)
        })
        .expect("no Page.frameAttached for the child frame on the client's own session");
    let navigated = ctx
        .pending_events
        .iter()
        .position(|e| {
            e.method == "Page.frameNavigated" && e.params["frame"]["id"] == child_id && mine(e)
        })
        .expect("no Page.frameNavigated for the child frame on the client's own session");
    assert!(
        attached < navigated,
        "frameAttached must come before frameNavigated"
    );

    // Each frame is announced once, however many commands the client sends.
    let before = ctx.pending_events.len();
    cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), session).await;
    let repeats = ctx.pending_events[before..]
        .iter()
        .filter(|e| e.method == "Page.frameAttached")
        .count();
    assert_eq!(repeats, 0, "the same frame was announced twice");
}

#[tokio::test(flavor = "current_thread")]
async fn isolated_worlds_are_owned_by_their_exact_frame() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let session = attached_session(&mut ctx).await;

    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url}), &session).await;
    cdp(&mut ctx, 2, "Runtime.evaluate", json!({"expression": "1"}), &session).await;
    let target = ctx.sessions[&session].clone();
    let attached = dispatch(
        &CdpRequest {
            id: 3,
            method: "Target.attachToTarget".to_string(),
            params: json!({"targetId": target, "flatten": true}),
            session_id: None,
        },
        &mut ctx,
    ).await.result.unwrap();
    let second_session = attached["sessionId"].as_str().unwrap().to_string();
    for runtime_session in [&session, &second_session] {
        cdp(&mut ctx, 3, "Runtime.enable", json!({}), runtime_session).await;
    }
    let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), &session).await;
    let main_id = tree["frameTree"]["frame"]["id"].as_str().unwrap().to_string();
    let child_id = tree["frameTree"]["childFrames"][0]["frame"]["id"]
        .as_str().unwrap().to_string();
    let grandchild_id = tree["frameTree"]["childFrames"][0]["childFrames"][0]["frame"]["id"]
        .as_str().unwrap().to_string();

    let main_world = cdp(
        &mut ctx, 5, "Page.createIsolatedWorld",
        json!({"frameId": main_id.clone(), "worldName": "utility"}), &session,
    ).await["executionContextId"].as_i64().unwrap();
    let child_world = cdp(
        &mut ctx, 6, "Page.createIsolatedWorld",
        json!({"frameId": child_id.clone(), "worldName": "utility"}), &session,
    ).await["executionContextId"].as_i64().unwrap();
    let grandchild_world = cdp(
        &mut ctx, 7, "Page.createIsolatedWorld",
        json!({"frameId": grandchild_id.clone(), "worldName": "utility"}), &session,
    ).await["executionContextId"].as_i64().unwrap();
    assert_ne!(main_world, child_world);
    assert_ne!(child_world, grandchild_world);

    let unknown = dispatch(
        &CdpRequest {
            id: 8,
            method: "Page.createIsolatedWorld".to_string(),
            params: json!({"frameId": "missing-frame", "worldName": "utility"}),
            session_id: Some(session.clone()),
        },
        &mut ctx,
    ).await;
    assert!(unknown.error.unwrap().message.contains("No frame with given id"));

    ctx.pending_events.clear();
    cdp(
        &mut ctx,
        9,
        "Runtime.evaluate",
        json!({"expression": "document.querySelector('iframe').remove()"}),
        &session,
    ).await;
    for _ in 0..3 {
        ctx.get_session_page_mut(&Some(session.clone()))
            .unwrap()
            .run_autonomous_event_loop_turn()
            .await
            .unwrap();
    }
    cdp(&mut ctx, 10, "Runtime.evaluate", json!({"expression": "1"}), &session).await;

    for (frame_id, context_id) in [
        (&child_id, child_world),
        (&grandchild_id, grandchild_world),
    ] {
        for runtime_session in [&session, &second_session] {
            let destroyed = ctx.pending_events.iter().position(|event| {
                event.method == "Runtime.executionContextDestroyed"
                    && event.session_id.as_deref() == Some(runtime_session.as_str())
                    && event.params["executionContextId"] == context_id
            }).unwrap_or_else(|| panic!(
                "missing executionContextDestroyed for {frame_id}/{context_id}: {:?}",
                ctx.pending_events.iter().map(|event| (
                    event.method.as_str(),
                    event.session_id.as_deref(),
                    event.params.clone(),
                )).collect::<Vec<_>>(),
            ));
            let detached = ctx.pending_events.iter().position(|event| {
                event.method == "Page.frameDetached"
                    && event.session_id.as_deref() == Some(runtime_session.as_str())
                    && event.params["frameId"] == frame_id.as_str()
            }).expect("missing frameDetached");
            assert!(destroyed < detached, "context must be destroyed before its frame detaches");
        }
    }

    let main_still_routes = dispatch(
        &CdpRequest {
            id: 11,
            method: "Runtime.evaluate".to_string(),
            params: json!({"expression": "1", "contextId": main_world}),
            session_id: Some(session.clone()),
        },
        &mut ctx,
    ).await;
    assert!(main_still_routes.error.is_none());
    for (id, stale_context) in [(12, child_world), (13, grandchild_world)] {
        let stale = dispatch(
            &CdpRequest {
                id,
                method: "Runtime.evaluate".to_string(),
                params: json!({"expression": "1", "contextId": stale_context}),
                session_id: Some(session.clone()),
            },
            &mut ctx,
        ).await;
        assert!(stale.error.unwrap().message.contains("Cannot find context"));
    }

    ctx.pending_events.clear();
    cdp(
        &mut ctx, 14, "Page.navigate",
        json!({"url": "data:text/html,<p>replacement</p>", "waitUntil": "load"}),
        &session,
    ).await;
    let recreated = ctx.pending_events.iter().filter(|event| {
        event.method == "Runtime.executionContextCreated"
            && event.session_id.as_deref() == Some(session.as_str())
            && event.params["context"]["name"] == "utility"
    }).collect::<Vec<_>>();
    assert_eq!(recreated.len(), 1, "only the main-frame world should persist");
    assert_eq!(recreated[0].params["context"]["auxData"]["frameId"], main_id);

}
