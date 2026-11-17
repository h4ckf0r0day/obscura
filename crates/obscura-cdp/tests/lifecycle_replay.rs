use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn call(ctx: &mut CdpContext, method: &str, params: Value, session: &str) -> Value {
    let response = dispatch(&CdpRequest {
        id: 1, method: method.into(), params, session_id: Some(session.into()),
    }, ctx).await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap()
}

fn replay(ctx: &CdpContext, session: &str, frame: &Value) -> Vec<Value> {
    ctx.pending_events.iter().map(|event| {
        assert_eq!(event.method, "Page.lifecycleEvent");
        assert_eq!(event.session_id.as_deref(), Some(session));
        assert_eq!(event.params["frameId"], frame["id"]);
        assert_eq!(event.params["loaderId"], frame["loaderId"]);
        assert!(event.params["timestamp"].as_f64().unwrap().is_finite());
        event.params.clone()
    }).collect()
}

#[tokio::test]
async fn late_lifecycle_subscription_replays_only_to_the_attached_session() {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("managed".into(), page.clone());
    call(&mut ctx, "Page.enable", json!({}), "managed").await;
    ctx.sessions.insert("attached".into(), page);
    let tree = call(&mut ctx, "Page.getFrameTree", json!({}), "attached").await;
    ctx.pending_events.clear();
    call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":true}), "attached").await;
    let first = replay(&ctx, "attached", &tree["frameTree"]["frame"]);
    let names: Vec<_> = first.iter().map(|event| event["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["commit", "DOMContentLoaded", "load", "networkAlmostIdle", "networkIdle"]);

    // Chrome replays on each enable, retaining the document's original timings.
    for enabled in [true, false, true] {
        ctx.pending_events.clear();
        call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":enabled}), "attached").await;
        let events = replay(&ctx, "attached", &tree["frameTree"]["frame"]);
        assert_eq!(events, if enabled { first.clone() } else { vec![] });
    }
}

#[tokio::test]
async fn lifecycle_replay_tracks_the_latest_navigation_and_original_timestamps() {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("driver".into(), page.clone());
    ctx.sessions.insert("observer".into(), page);
    call(&mut ctx, "Page.enable", json!({}), "driver").await;
    let initial = call(&mut ctx, "Page.getFrameTree", json!({}), "observer").await;
    for url in ["data:text/html,<p>first</p>", "data:text/html,<p>second</p>"] {
        ctx.pending_events.clear();
        call(&mut ctx, "Page.navigate", json!({"url":url}), "driver").await;
        let emitted: Vec<_> = ctx.pending_events.iter()
            .filter(|event| event.method == "Page.lifecycleEvent" && event.params["name"] != "init")
            .map(|event| event.params.clone()).collect();
        let tree = call(&mut ctx, "Page.getFrameTree", json!({}), "observer").await;
        assert_ne!(tree["frameTree"]["frame"]["loaderId"], initial["frameTree"]["frame"]["loaderId"]);
        ctx.pending_events.clear();
        call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":true}), "observer").await;
        let events = replay(&ctx, "observer", &tree["frameTree"]["frame"]);
        assert!(!events.is_empty());
        for event in emitted {
            assert!(events.contains(&event), "missing original lifecycle event: {event}");
        }
    }
}

#[tokio::test]
async fn document_replacement_discards_previous_idle_phases_without_a_subscriber() {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("writer".into(), page);
    call(&mut ctx, "Page.enable", json!({}), "writer").await;
    call(&mut ctx, "Page.navigate", json!({"url":"data:text/html,<p>old</p>"}), "writer").await;
    call(&mut ctx, "Runtime.evaluate", json!({"expression":
        "document.open(); document.write('<p>replacement</p>'); document.close()"
    }), "writer").await;
    ctx.get_session_page_mut(&Some("writer".into())).unwrap().settle(40).await;
    call(&mut ctx, "Runtime.evaluate", json!({"expression":"document.readyState"}), "writer").await;
    let tree = call(&mut ctx, "Page.getFrameTree", json!({}), "writer").await;
    ctx.pending_events.clear();
    call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":true}), "writer").await;
    let events = replay(&ctx, "writer", &tree["frameTree"]["frame"]);
    let names: Vec<_> = events.iter().map(|event| event["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["commit", "DOMContentLoaded", "load"]);
}

#[tokio::test]
async fn failed_navigation_does_not_replay_completion_from_the_previous_document() {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("observer".into(), page.clone());
    call(&mut ctx, "Page.enable", json!({}), "observer").await;
    let frame_id = ctx.get_page(&page).unwrap().frame_id.clone();
    ctx.get_session_page_mut(&Some("observer".into())).unwrap().lifecycle =
        obscura_browser::LifecycleState::Failed;
    obscura_cdp::domains::page::emit_navigation_events(
        &mut ctx, &Some("observer".into()), &frame_id, "failed-loader",
        "https://invalid.test/", &page, &[], obscura_browser::lifecycle::WaitUntil::Load, false,
    );
    let tree = call(&mut ctx, "Page.getFrameTree", json!({}), "observer").await;
    ctx.pending_events.clear();
    call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":true}), "observer").await;
    let events = replay(&ctx, "observer", &tree["frameTree"]["frame"]);
    let names: Vec<_> = events.iter().map(|event| event["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["commit"]);
}
