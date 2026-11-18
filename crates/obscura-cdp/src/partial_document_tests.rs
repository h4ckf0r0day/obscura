use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::dispatch::{dispatch, CdpContext};
use crate::types::{CdpRequest, CdpResponse};

#[path = "../../obscura-net/tests/support/partial_server.rs"]
mod fixture;

async fn command(ctx: &mut CdpContext, session: &str, method: &str, params: Value) -> CdpResponse {
    dispatch(&CdpRequest { id: 1, method: method.into(), params, session_id: Some(session.into()) }, ctx).await
}

async fn ok(ctx: &mut CdpContext, session: &str, method: &str, params: Value) -> Value {
    let response = command(ctx, session, method, params).await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap()
}

async fn old_page(url: &str) -> (CdpContext, String, String, i64, u64, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    let session = format!("{page}-partial");
    ctx.sessions.insert(session.clone(), page.clone());
    ok(&mut ctx, &session, "Page.navigate", json!({"url":url})).await;
    ok(&mut ctx, &session, "Runtime.enable", json!({})).await;
    let object = ok(&mut ctx, &session, "Runtime.evaluate", json!({"expression":"globalThis.oldMarker=42;({marker:'old'})"})).await;
    let object = object["result"]["objectId"].as_str().unwrap().to_string();
    let document = ok(&mut ctx, &session, "DOM.getDocument", json!({})).await;
    let node = document["root"]["nodeId"].as_u64().unwrap();
    let context = ctx.default_context_id(&page).unwrap();
    ctx.get_page_mut(&page).unwrap().set_navigation_timeout(Duration::from_millis(350));
    ctx.pending_events.clear();
    (ctx, page, session, context, node, object)
}

fn stale_intercepted_document(ctx: &mut CdpContext, page: &str, url: &str) {
    ctx.get_page_mut(page).unwrap().network_events.push(obscura_browser::NetworkEvent {
        request_id: format!("{page}.99999"), intercepted: true, url: url.into(),
        method: "GET".into(), resource_type: "Document".into(), status: 200,
        headers: Default::default(), response_headers: Arc::new(Default::default()),
        body_size: 0, timestamp: 0.0, error_text: None,
    });
}

async fn assert_old_realm(ctx: &mut CdpContext, page: &str, session: &str, context: i64, node: u64, object: &str, failed_requests: usize) {
    assert_eq!(ctx.default_context_id(page), Some(context));
    assert!(ctx.dom_documents.contains_key(&Some(session.into())));
    for event in &ctx.pending_events {
        assert!(!matches!(event.method.as_str(), "Runtime.executionContextsCleared" | "Runtime.executionContextDestroyed"
            | "Page.frameNavigated" | "DOM.documentUpdated"), "false document replacement: {:?}", event);
    }
    assert_eq!(ctx.pending_events.iter().filter(|e| e.method == "Network.loadingFailed").count(), failed_requests);
    assert!(ctx.pending_events.iter().any(|e| e.method == "Page.frameStoppedLoading"));
    let value = ok(ctx, session, "Runtime.evaluate", json!({"expression":"oldMarker","contextId":context,"returnByValue":true})).await;
    assert_eq!(value["result"]["value"], json!(42.0));
    let value = ok(ctx, session, "Runtime.callFunctionOn", json!({"objectId":object,"functionDeclaration":"function(){return this.marker}","returnByValue":true})).await;
    assert_eq!(value["result"]["value"], "old");
    ok(ctx, session, "DOM.getOuterHTML", json!({"nodeId":node})).await;
}

#[tokio::test(flavor = "current_thread")]
async fn failed_uncommitted_navigation_preserves_context_nodes_and_handles() {
    let (origin, _) = fixture::serve().await;
    let old = "data:text/html,<title>old</title><input id=old>";
    let (mut ctx, page, session, context, node, object) = old_page(old).await;
    for path in ["noheaders", "nobytes", "json", "protocol", "oversize", "redirect-file"] {
        stale_intercepted_document(&mut ctx, &page, old);
        ctx.pending_events.clear();
        let response = command(&mut ctx, &session, "Page.navigate", json!({"url":format!("{origin}/{path}")})).await;
        assert!(response.error.is_some(), "{path}");
        assert_eq!(ctx.get_page(&page).unwrap().url_string(), old, "{path}");
        assert_old_realm(&mut ctx, &page, &session, context, node, &object, 1).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn same_url_reload_without_headers_preserves_the_document_identity() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/same", listener.local_addr().unwrap());
    let fail = Arc::new(AtomicBool::new(false));
    let server_fail = fail.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let fail = server_fail.clone();
            tokio::spawn(async move {
                let _ = socket.read(&mut [0; 4096]).await;
                if fail.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                } else {
                    let body = "<title>old</title><input id=old>";
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            });
        }
    });
    let (mut ctx, page, session, context, node, object) = old_page(&url).await;
    fail.store(true, Ordering::SeqCst);
    stale_intercepted_document(&mut ctx, &page, &url);
    let response = command(&mut ctx, &session, "Page.reload", json!({})).await;
    assert!(response.error.is_some());
    assert_eq!(ctx.get_page(&page).unwrap().url_string(), url);
    assert_old_realm(&mut ctx, &page, &session, context, node, &object, 1).await;
}

#[tokio::test(flavor = "current_thread")]
async fn failed_post_eval_navigation_preserves_the_realm_without_inventing_a_request() {
    let (origin, _) = fixture::serve().await;
    let old = "data:text/html,<title>old</title><input id=old>";
    let (mut ctx, page, session, context, node, object) = old_page(old).await;
    let source = format!("location.href={}", serde_json::to_string(&format!("{origin}/noheaders")).unwrap());
    let response = command(&mut ctx, &session, "Runtime.evaluate", json!({"expression":source})).await;
    assert!(response.error.is_some());
    assert_eq!(ctx.get_page(&page).unwrap().url_string(), old);
    assert!(!ctx.pending_events.iter().any(|event| event.method == "Network.requestWillBeSent"));
    assert_old_realm(&mut ctx, &page, &session, context, node, &object, 0).await;
}

#[tokio::test(flavor = "current_thread")]
async fn partial_document_errors_commit_contexts_and_report_only_the_failed_transfer() {
    let (origin, _) = fixture::serve().await;
    for method in ["Page.navigate", "Runtime.evaluate", "Runtime.callFunctionOn", "Input.dispatchKeyEvent"] {
        let (mut ctx, page, session, context, _, object) = old_page("data:text/html,<title>old</title>").await;
        let url = format!("{origin}/slow");
        if method == "Input.dispatchKeyEvent" {
            let source = format!("document.body.onkeydown=function(){{location.href={};}};document.body.focus()", serde_json::to_string(&url).unwrap());
            ok(&mut ctx, &session, "Runtime.evaluate", json!({"expression":source})).await;
            ok(&mut ctx, &session, method, json!({"type":"keyDown","key":"Enter","code":"Enter","windowsVirtualKeyCode":13})).await;
        }
        let params = match method {
            "Page.navigate" => json!({"url":url}),
            "Runtime.evaluate" => json!({"expression":format!("location.href={}", serde_json::to_string(&url).unwrap())}),
            "Input.dispatchKeyEvent" => json!({"expression":"void 0"}),
            _ => json!({"functionDeclaration":"function(url){location.href=url}","arguments":[{"value":url}]}),
        };
        let command_method = if method == "Input.dispatchKeyEvent" { "Runtime.evaluate" } else { method };
        let response = command(&mut ctx, &session, command_method, params).await;
        assert!(response.error.is_some(), "{method}");
        let retained = ctx.get_page(&page).unwrap();
        assert_eq!(retained.title, "partial", "{method}");
        assert_eq!(retained.url_string(), url);
        assert!(retained.js.is_none(), "retention must not activate a realm: {method}");
        assert_ne!(ctx.default_context_id(&page), Some(context), "{method}");
        assert!(!ctx.dom_documents.contains_key(&Some(session.clone())));
        let events = &ctx.pending_events;
        assert!(events.iter().any(|e| e.method == "Runtime.executionContextsCleared"), "{method}");
        assert!(events.iter().any(|e| e.method == "Page.frameNavigated"), "{method}");
        let responses: Vec<_> = events.iter().filter(|e| e.method == "Network.responseReceived" && e.params["type"] == "Document").collect();
        assert_eq!(responses.len(), 1, "{method}");
        let request_id = &responses[0].params["requestId"];
        let failed: Vec<_> = events.iter().filter(|e| e.method == "Network.loadingFailed" && &e.params["requestId"] == request_id).collect();
        assert_eq!(failed.len(), 1, "{method}");
        assert_eq!(failed[0].params["errorText"], "net::ERR_TIMED_OUT");
        assert!(!events.iter().any(|e| e.method == "Network.loadingFinished" && &e.params["requestId"] == request_id), "{method}");
        assert!(!events.iter().any(|e| matches!(e.method.as_str(), "Page.loadEventFired" | "Page.domContentEventFired")
            || (e.method == "Page.lifecycleEvent" && matches!(e.params["name"].as_str(), Some("load" | "DOMContentLoaded")))), "{method}");
        assert!(command(&mut ctx, &session, "Runtime.callFunctionOn", json!({"objectId":object,"functionDeclaration":"function(){return this.marker}"})).await.error.is_some());
        assert!(command(&mut ctx, &session, "Runtime.evaluate", json!({"expression":"1","contextId":context})).await.error.is_some());
        let value = ok(&mut ctx, &session, "Runtime.evaluate", json!({"expression":"[document.title,document.readyState,typeof oldMarker,typeof prefixRan,typeof eofRan,typeof frameRan]","returnByValue":true})).await;
        assert_eq!(value["result"]["value"], json!(["partial","loading","undefined","undefined","undefined","undefined"]));
    }
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn active_screencast_and_autonomous_pump_keep_recovered_documents_js_lazy() {
    use base64::Engine as _;
    let (origin, _) = fixture::serve().await;
    let (mut ctx, page, session, _, _, _) = old_page("data:text/html,<title>old</title>").await;
    ok(&mut ctx, &session, "Page.startScreencast", json!({"format":"png"})).await;
    let first = ctx.pending_events.iter().find(|e| e.method == "Page.screencastFrame").unwrap().params["sessionId"].clone();
    ok(&mut ctx, &session, "Page.screencastFrameAck", json!({"sessionId":first})).await;
    ctx.pending_events.clear();
    assert!(command(&mut ctx, &session, "Page.navigate", json!({"url":format!("{origin}/slow")})).await.error.is_some());
    assert!(ctx.get_page(&page).unwrap().js.is_none());
    let frame = ctx.pending_events.iter().find(|e| e.method == "Page.screencastFrame").expect("retained DOM frame");
    let bytes = base64::engine::general_purpose::STANDARD.decode(frame.params["data"].as_str().unwrap()).unwrap();
    let png = image::load_from_memory(&bytes).unwrap().to_rgb8();
    assert!(png.width() > 0 && png.height() > 0);
    assert!(png.pixels().any(|p| p[0] < 200 && p[1] < 200 && p[2] < 200), "received text must be visible");
    let frame_id = frame.params["sessionId"].clone();
    ok(&mut ctx, &session, "Page.screencastFrameAck", json!({"sessionId":frame_id})).await;
    ctx.screencasts.get_mut(&session).unwrap().autonomous_frame_pending = true;
    crate::domains::page::pump_screencast_frames(&mut ctx).await;
    assert!(ctx.get_page(&page).unwrap().js.is_none(), "automatic compositor work cannot initialize JS");
    assert!(crate::domains::page::queue_screencast_frame(&mut ctx, &Some(session.clone()), true).unwrap());
    assert!(ctx.get_page(&page).unwrap().js.is_none());
    let frame_id = ctx.pending_events.iter().rev().find(|e| e.method == "Page.screencastFrame").unwrap().params["sessionId"].clone();
    ok(&mut ctx, &session, "Page.screencastFrameAck", json!({"sessionId":frame_id})).await;
    // A small recording output cannot bypass the full-viewport DOM raster cap.
    ctx.get_page_mut(&page).unwrap().viewport = (10_000.0, 10_000.0);
    let stream = ctx.screencasts.get_mut(&session).unwrap();
    stream.max_width = Some(64);
    stream.max_height = Some(64);
    assert!(crate::domains::page::queue_screencast_frame(&mut ctx, &Some(session), true).is_err());
    assert!(ctx.get_page(&page).unwrap().js.is_none());
}
