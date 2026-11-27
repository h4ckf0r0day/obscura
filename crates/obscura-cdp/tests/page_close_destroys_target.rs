//! `Page.close` closed nothing: obscura fell through to its
//! `Unknown Page method` catch-all, so a go-rod client that closed a tab got an
//! error back and - worse - no `Target.targetDestroyed` event. rod's `Page.Close`
//! sends `Page.close` and then sits in a `for msg := range messages` loop until
//! that event arrives (rod/page.go), so every crawled URL ended with
//! `Failed close page: Unknown Page method: close` and the page stayed in the
//! browser's page map.
//!
//! The teardown is the one `Target.closeTarget` already performs: detach each
//! attached session, announce the destruction, drop the page. Chromium answers
//! `{}` to `Page.close` too, so both methods share that helper.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session_id: &str) -> Value {
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

fn emitted(ctx: &CdpContext, event: &str) -> Vec<Value> {
    ctx.pending_events
        .iter()
        .filter(|e| e.method == event)
        .map(|e| e.params.clone())
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn page_close_destroys_the_target_and_emits_target_destroyed() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(&mut ctx, 1, "Page.close", json!({}), session_id).await;

    assert!(
        ctx.pages.iter().all(|p| p.id != page_id),
        "Page.close must drop the page, pages left: {:?}",
        ctx.pages.iter().map(|p| &p.id).collect::<Vec<_>>()
    );

    let destroyed = emitted(&ctx, "Target.targetDestroyed");
    assert!(
        destroyed
            .iter()
            .any(|p| p.get("targetId").and_then(|v| v.as_str()) == Some(page_id.as_str())),
        "rod waits for Target.targetDestroyed before Close returns; events: {destroyed:#?}"
    );

    let detached = emitted(&ctx, "Target.detachedFromTarget");
    assert!(
        detached
            .iter()
            .any(|p| p.get("sessionId").and_then(|v| v.as_str()) == Some(session_id)),
        "the attached session must be detached too; events: {detached:#?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stop_loading_answers_success_like_chromium() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    let result = cdp(&mut ctx, 1, "Page.stopLoading", json!({}), session_id).await;

    assert_eq!(result, json!({}), "go-rod calls this before every navigate");
}

#[tokio::test(flavor = "current_thread")]
async fn page_close_runs_beforeunload_and_waits_for_dialog_resolution() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session = "close-confirmation";
    ctx.sessions.insert(session.into(), page_id.clone());
    cdp(&mut ctx, 1, "Runtime.evaluate", json!({"expression":
        "globalThis.closeAttempts = 0; addEventListener('beforeunload', e => { closeAttempts++; e.preventDefault(); });"}), session).await;
    cdp(&mut ctx, 6, "Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "a", "code": "KeyA"}), session).await;
    cdp(&mut ctx, 2, "Page.close", json!({}), session).await;
    let page = ctx.get_page_mut(&page_id).expect("a confirmation must not destroy the target");
    assert_eq!(page.evaluate("closeAttempts").as_f64(), Some(1.0));
    let opening = emitted(&ctx, "Page.javascriptDialogOpening");
    assert_eq!(opening.len(), 1);
    assert_eq!(opening[0]["type"], "beforeunload");
    assert!(emitted(&ctx, "Target.targetDestroyed").is_empty());

    cdp(&mut ctx, 3, "Page.handleJavaScriptDialog", json!({"accept": false}), session).await;
    assert!(ctx.get_page(&page_id).is_some(), "dismissing must keep the page");
    assert_eq!(emitted(&ctx, "Page.javascriptDialogClosed")[0]["result"], false);
    cdp(&mut ctx, 4, "Page.close", json!({}), session).await;
    assert_eq!(ctx.get_page_mut(&page_id).unwrap().evaluate("closeAttempts").as_f64(), Some(2.0));
    cdp(&mut ctx, 5, "Page.handleJavaScriptDialog", json!({"accept": true}), session).await;
    assert!(ctx.get_page(&page_id).is_none());
    assert_eq!(emitted(&ctx, "Target.targetDestroyed").len(), 1);
    assert_eq!(emitted(&ctx, "Page.javascriptDialogClosed")[1]["result"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn page_close_does_not_prompt_without_user_activation() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session = "close-no-activation";
    ctx.sessions.insert(session.into(), page_id.clone());
    cdp(&mut ctx, 1, "Runtime.evaluate", json!({"expression":
        "addEventListener('beforeunload', e => e.preventDefault());"}), session).await;
    cdp(&mut ctx, 2, "Page.close", json!({}), session).await;
    assert!(ctx.get_page(&page_id).is_none());
    assert!(emitted(&ctx, "Page.javascriptDialogOpening").is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn page_close_honors_the_beforeunload_handler_return_value() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session = "close-handler-return";
    ctx.sessions.insert(session.into(), page_id.clone());
    cdp(&mut ctx, 1, "Runtime.evaluate", json!({"expression":
        "onbeforeunload = e => { if (!e.isTrusted || !e.cancelable) throw new Error('not native'); return 'Unsaved'; };"}), session).await;
    cdp(&mut ctx, 2, "Input.dispatchMouseEvent", json!({"type": "mousePressed", "x": 1, "y": 1, "button": "left"}), session).await;
    cdp(&mut ctx, 3, "Page.close", json!({}), session).await;
    assert!(ctx.get_page(&page_id).is_some());
    assert_eq!(emitted(&ctx, "Page.javascriptDialogOpening").len(), 1);
    cdp(&mut ctx, 4, "Target.closeTarget", json!({"targetId": page_id}), session).await;
    assert!(ctx.get_page(&page_id).is_none(), "force-close must bypass confirmation");
}

#[tokio::test(flavor = "current_thread")]
async fn page_close_honors_an_empty_beforeunload_handler_return_value() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session = "close-empty-return";
    ctx.sessions.insert(session.into(), page_id.clone());
    let synthetic = cdp(&mut ctx, 0, "Runtime.evaluate", json!({"expression": "globalThis.onbeforeunload = () => false; globalThis.dispatchEvent(new Event('beforeunload', {cancelable: true}))", "returnByValue": true}), session).await;
    assert_eq!(synthetic["result"]["value"], false, "generic events must keep ordinary false-return cancellation");
    cdp(&mut ctx, 1, "Runtime.evaluate", json!({"expression": "onbeforeunload = () => '';"}), session).await;
    cdp(&mut ctx, 2, "Input.dispatchKeyEvent", json!({"type": "keyDown", "key": "a", "code": "KeyA"}), session).await;
    cdp(&mut ctx, 3, "Page.close", json!({}), session).await;
    assert!(ctx.get_page(&page_id).is_some(), "every non-null handler return requests confirmation");
    assert_eq!(emitted(&ctx, "Page.javascriptDialogOpening").len(), 1);
}

#[cfg(feature = "render")]
async fn assert_stop_loading_cancels_images(native: bool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::task::LocalSet::new().run_until(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (requested_tx, requested_rx) = tokio::sync::oneshot::channel();
        let fixture = tokio::task::spawn_local(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 2048];
            stream.read(&mut buffer).await.unwrap();
            let body = if native { "<!doctype html><title>Images</title>" }
                else { "<!doctype html><img src='/held.png'>" };
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(stream.read(&mut buffer).await.unwrap() > 0);
            requested_tx.send(()).unwrap();
            assert_eq!(stream.read(&mut buffer).await.unwrap(), 0, "stop must close the HTTP request");
        });
        let context = obscura_browser::context::BrowserContext::with_storage_and_network(
            "stop-resources-test".into(), None, false, None, None, true,
        );
        let mut ctx = CdpContext::new_with_shared_context(std::sync::Arc::new(context));
        let page_id = ctx.create_page();
        let session = "stop-resources";
        ctx.sessions.insert(session.into(), page_id.clone());
        cdp(&mut ctx, 1, "Page.navigate", json!({"url": format!("http://{address}/"), "waitUntil": "domcontentloaded"}), session).await;
        if native {
            cdp(&mut ctx, 10, "Runtime.evaluate", json!({"expression": "globalThis.imageErrors = 0; globalThis.images = [new Image(), new Image()]; for (const image of images) { image.src = '/held.png'; image.decode().catch(() => imageErrors++); }"}), session).await;
            ctx.get_page_mut(&page_id).unwrap().js.as_mut().unwrap().run_event_loop_bounded(10).await.unwrap();
        }
        ctx.get_page_mut(&page_id).unwrap().queue_pending_render_resources();
        tokio::time::timeout(std::time::Duration::from_secs(5), requested_rx).await.expect("image request").unwrap();
        cdp(&mut ctx, 2, "Page.stopLoading", json!({}), session).await;
        ctx.get_page_mut(&page_id).unwrap().js.as_mut().unwrap().run_event_loop_bounded(10).await.unwrap();
        let page = ctx.get_page_mut(&page_id).unwrap();
        if native { assert_eq!(page.evaluate("imageErrors").as_f64(), Some(2.0)); }
        assert!(!page.has_pending_render_resources(), "stop must cancel pending image loads");
        page.queue_pending_render_resources();
        assert!(!page.has_pending_render_resources(), "the autonomous pump must not restart canceled loads");
        assert!(page.js.as_ref().unwrap().render_resource_is_known(&format!("http://{address}/held.png")));
        tokio::time::timeout(std::time::Duration::from_secs(2), fixture).await.expect("image socket closes").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
        let page = ctx.get_page_mut(&page_id).unwrap();
        page.queue_pending_render_resources();
        assert!(!page.has_pending_render_resources(), "stopped loads must not restart after the failure retry window");
    }).await;
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn stop_loading_cancels_background_resources_without_restarting_them() {
    assert_stop_loading_cancels_images(false).await;
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn stop_loading_cancels_native_image_requests_and_decode_waiters() {
    assert_stop_loading_cancels_images(true).await;
}
