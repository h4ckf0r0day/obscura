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

#[tokio::test]
async fn document_replacement_emits_ordered_lifecycle_without_replacing_the_context() {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("write-session".into(), page.clone());
    call(&mut ctx, "Page.enable", json!({}), "write-session").await;
    call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":true}), "write-session").await;
    call(&mut ctx, "Runtime.enable", json!({}), "write-session").await;
    call(&mut ctx, "Page.navigate", json!({"url":"data:text/html,<title>old</title><p>old</p>"}), "write-session").await;
    ctx.pending_events.clear();
    for _ in 0..2 {
        call(&mut ctx, "Runtime.evaluate", json!({"expression":r#"(() => {
            document.open(); console.debug('write-marker');
            document.write('<title>new</title><p id="replacement">written</p>');
            document.close();
        })()"#}), "write-session").await;
        ctx.get_session_page_mut(&Some("write-session".into())).unwrap().settle(40).await;
        let result = call(&mut ctx, "Runtime.evaluate", json!({
            "expression":"[document.title,document.readyState,document.querySelector('#replacement').textContent]",
            "returnByValue":true,
        }), "write-session").await;
        assert_eq!(result["result"]["value"], json!(["new", "complete", "written"]));
        let lifecycle: Vec<_> = ctx.pending_events.iter().filter(|e| e.method == "Page.lifecycleEvent")
            .map(|e| e.params["name"].as_str().unwrap()).collect();
        assert_eq!(lifecycle, ["init", "DOMContentLoaded", "load"]);
        assert!(!ctx.pending_events.iter().any(|e| matches!(e.method.as_str(),
            "Runtime.executionContextsCleared" | "Runtime.executionContextDestroyed" | "Page.frameNavigated")));
        let marker = ctx.pending_events.iter().position(|e| e.method == "Runtime.consoleAPICalled").unwrap();
        let loaded = ctx.pending_events.iter().position(|e| e.params["name"] == "DOMContentLoaded").unwrap();
        assert!(marker < loaded, "setContent marker must precede completion");
        ctx.pending_events.clear();
    }
}


#[tokio::test(flavor = "current_thread")]
async fn document_lifecycle_subscriptions_are_independent_of_runtime() {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("lifecycle".into(), page.clone());
    ctx.sessions.insert("console".into(), page.clone());
    call(&mut ctx, "Page.enable", json!({}), "lifecycle").await;
    call(&mut ctx, "Page.navigate", json!({"url":"data:text/html,<p>old</p>"}), "lifecycle").await;
    call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":true}), "lifecycle").await;
    ctx.pending_events.clear();

    // No Runtime subscriber: document completion must still be delivered.
    // Then add a distinct Runtime-only subscriber, and finally disable it.
    for phase in 0..3 {
        if phase == 1 {
            call(&mut ctx, "Runtime.enable", json!({}), "console").await;
        } else if phase == 2 {
            call(&mut ctx, "Runtime.disable", json!({}), "console").await;
        }
        ctx.pending_events.clear();
        call(&mut ctx, "Runtime.evaluate", json!({"expression":r#"(() => {
            document.open(); console.debug('subscription-marker');
            document.write('<p>replacement</p>'); document.close();
        })()"#}), "lifecycle").await;
        ctx.get_session_page_mut(&Some("lifecycle".into())).unwrap().settle(40).await;
        call(&mut ctx, "Runtime.evaluate", json!({"expression":"document.readyState"}), "lifecycle").await;
        let lifecycle: Vec<_> = ctx.pending_events.iter()
            .filter(|event| event.method == "Page.lifecycleEvent")
            .map(|event| (event.session_id.as_deref(), event.params["name"].as_str().unwrap()))
            .collect();
        assert_eq!(lifecycle, vec![
            (Some("lifecycle"), "init"),
            (Some("lifecycle"), "DOMContentLoaded"),
            (Some("lifecycle"), "load"),
        ], "phase {phase}");
        let console: Vec<_> = ctx.pending_events.iter()
            .filter(|event| event.method == "Runtime.consoleAPICalled")
            .map(|event| event.session_id.as_deref()).collect();
        if phase == 1 {
            assert_eq!(console, vec![Some("console")]);
        } else {
            assert!(console.is_empty(), "Runtime events without a subscriber");
        }
    }

    // Disabling Page lifecycle must not be undone by enabling Runtime.
    call(&mut ctx, "Page.setLifecycleEventsEnabled", json!({"enabled":false}), "lifecycle").await;
    call(&mut ctx, "Runtime.enable", json!({}), "console").await;
    ctx.pending_events.clear();
    call(&mut ctx, "Runtime.evaluate", json!({"expression":
        "document.open(); document.write('<p>quiet</p>'); document.close()"
    }), "lifecycle").await;
    ctx.get_session_page_mut(&Some("lifecycle".into())).unwrap().settle(40).await;
    call(&mut ctx, "Runtime.evaluate", json!({"expression":"document.readyState"}), "lifecycle").await;
    assert!(!ctx.pending_events.iter().any(|event| event.method == "Page.lifecycleEvent"));
}
