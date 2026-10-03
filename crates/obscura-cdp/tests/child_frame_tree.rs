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
                    "<html><body><iframe src=\"/grandchild.html\"></iframe></body></html>"
                } else if request.starts_with("GET /grandchild.html ") {
                    "<html><body><p>deep</p></body></html>"
                } else if request.starts_with("GET /plain.html ") {
                    "<html><body><p>plain</p></body></html>"
                } else {
                    "<html><body><iframe src=\"/child.html\"></iframe></body></html>"
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
async fn get_frame_tree_reports_nested_child_frames() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let plain_url = format!("{url}plain.html");
    let mut ctx = CdpContext::new();
    let session = &attached_session(&mut ctx).await;
    cdp(&mut ctx, 0, "Page.enable", json!({}), session).await;

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
    // enough. Events belong only to the session that enabled Page.
    let child_id = child["frame"]["id"].as_str().unwrap().to_string();
    let page_id = ctx.sessions.get(session.as_str()).cloned().unwrap();
    for (candidate, owner) in &ctx.sessions {
        if owner != &page_id || candidate == session {
            continue;
        }
        assert!(
            !ctx.pending_events.iter().any(|e| {
                e.method == "Page.frameAttached"
                    && e.params["frameId"] == child_id
                    && e.session_id.as_deref() == Some(candidate.as_str())
            }),
            "Page-disabled session {candidate} received a child-frame event"
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

    // A spawned navigation temporarily moves its page out of the context. A
    // drain caused by another command must not forget that page's child-frame
    // announcements and duplicate them when the page comes back.
    let page_index = ctx
        .pages
        .iter()
        .position(|page| page.id == page_id)
        .expect("page in context");
    let task_owned_page = ctx.pages.remove(page_index);
    cdp(&mut ctx, 4, "Browser.getVersion", json!({}), session).await;
    ctx.pages.push(task_owned_page);
    let reinsert_start = ctx.pending_events.len();
    cdp(&mut ctx, 5, "Runtime.evaluate", json!({"expression": "1"}), session).await;
    assert!(!ctx.pending_events[reinsert_start..].iter().any(|event| {
        matches!(event.method.as_str(), "Page.frameAttached" | "Page.frameNavigated")
            && (event.params["frameId"] == child_id
                || event.params["frame"]["id"] == child_id)
            && event.session_id.as_deref() == Some(session.as_str())
    }), "task-owned page lost its frame announcement state");

    let late_session = ctx
        .sessions
        .iter()
        .find(|(candidate, owner)| {
            owner.as_str() == page_id.as_str() && candidate.as_str() != session.as_str()
        })
        .map(|(candidate, _)| candidate.clone())
        .expect("second page session");
    let late_start = ctx.pending_events.len();
    cdp(&mut ctx, 6, "Page.enable", json!({}), &late_session).await;
    assert!(ctx.pending_events[late_start..].iter().any(|event| {
        event.method == "Page.frameAttached"
            && event.params["frameId"] == child_id
            && event.session_id.as_deref() == Some(late_session.as_str())
    }));
    assert!(!ctx.pending_events[late_start..].iter().any(|event| {
        event.method == "Page.frameAttached"
            && event.params["frameId"] == child_id
            && event.session_id.as_deref() == Some(session.as_str())
    }));

    cdp(&mut ctx, 7, "Page.disable", json!({}), &late_session).await;
    let reenable_start = ctx.pending_events.len();
    cdp(&mut ctx, 8, "Page.enable", json!({}), &late_session).await;
    assert!(ctx.pending_events[reenable_start..].iter().any(|event| {
        event.method == "Page.frameAttached"
            && event.params["frameId"] == child_id
            && event.session_id.as_deref() == Some(late_session.as_str())
    }));
    assert!(!ctx.pending_events[reenable_start..].iter().any(|event| {
        event.method == "Page.frameAttached"
            && event.params["frameId"] == child_id
            && event.session_id.as_deref() == Some(session.as_str())
    }));

    let removal_start = ctx.pending_events.len();
    cdp(
        &mut ctx,
        9,
        "Page.navigate",
        json!({"url": plain_url, "waitUntil": "load"}),
        session,
    )
    .await;
    for expected_session in [session.as_str(), late_session.as_str()] {
        assert_eq!(
            ctx.pending_events[removal_start..]
                .iter()
                .filter(|event| {
                    event.method == "Page.frameDetached"
                        && event.params["frameId"] == child_id
                        && event.session_id.as_deref() == Some(expected_session)
                })
                .count(),
            1,
            "child removal was not routed exactly once to {expected_session}",
        );
    }
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
        cdp(&mut ctx, 4, "Page.enable", json!({}), runtime_session).await;
    }
    let tree = cdp(&mut ctx, 5, "Page.getFrameTree", json!({}), &session).await;
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
