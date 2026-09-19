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
