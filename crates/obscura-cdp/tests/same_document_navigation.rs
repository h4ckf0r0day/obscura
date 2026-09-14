use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some("navigation".to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn setup() -> (CdpContext, String, String, i64) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = [0; 2048];
            let _ = socket.read(&mut buffer).await;
            let body = r##"<!doctype html><a id="route" href="#next">next</a>
                <script>route.onclick = event => {
                    event.preventDefault(); history.pushState({}, '', '/next');
                };</script>"##;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions
        .insert("navigation".to_string(), page_id.clone());
    cdp(&mut ctx, "Page.enable", json!({})).await;
    cdp(&mut ctx, "Runtime.enable", json!({})).await;
    let navigation = cdp(
        &mut ctx,
        "Page.navigate",
        json!({"url": url, "waitUntil": "load"}),
    )
    .await;
    let loader_id = navigation["loaderId"].as_str().unwrap().to_string();
    let context_id = ctx
        .pending_events
        .iter()
        .rev()
        .find_map(|event| {
            (event.method == "Runtime.executionContextCreated"
                && event.params["context"]["auxData"]["isDefault"] == true
                && event.params["context"]["auxData"]["frameId"] == page_id)
                .then(|| event.params["context"]["id"].as_i64().unwrap())
        })
        .unwrap();
    cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "globalThis.retainedMarker = 42", "contextId": context_id}),
    )
    .await;
    ctx.pending_events.clear();
    (ctx, page_id, loader_id, context_id)
}

async fn click(ctx: &mut CdpContext) {
    cdp(
        ctx,
        "Runtime.evaluate",
        json!({"expression": "document.elementFromPoint = () => document.getElementById('route')"}),
    )
    .await;
    for event_type in ["mousePressed", "mouseReleased"] {
        cdp(
            ctx,
            "Input.dispatchMouseEvent",
            json!({"type": event_type, "x": 0, "y": 0, "button": "left", "clickCount": 1}),
        )
        .await;
    }
}

async fn assert_history_navigation(clicked: bool) {
    let (mut ctx, page_id, loader_id, context_id) = setup().await;
    let isolated = cdp(
        &mut ctx,
        "Page.createIsolatedWorld",
        json!({"frameId": page_id, "worldName": "retained-world"}),
    )
    .await["executionContextId"]
        .as_i64()
        .unwrap();
    ctx.pending_events.clear();
    if clicked {
        click(&mut ctx).await;
    } else {
        cdp(
            &mut ctx,
            "Runtime.evaluate",
            json!({"expression": "history.pushState({}, '', '/next')", "contextId": context_id}),
        )
        .await;
    }
    let methods = ctx
        .pending_events
        .iter()
        .map(|event| event.method.as_str())
        .collect::<Vec<_>>();
    let navigation = ctx
        .pending_events
        .iter()
        .find(|event| event.method == "Page.navigatedWithinDocument")
        .unwrap_or_else(|| panic!("missing same-document event: {methods:?}"));
    assert_eq!(navigation.params["frameId"], page_id);
    assert_eq!(navigation.params["navigationType"], "historyApi");
    assert!(navigation.params["url"]
        .as_str()
        .unwrap()
        .ends_with("/next"));
    assert_eq!(navigation.session_id.as_deref(), Some("navigation"));
    for forbidden in [
        "Page.frameNavigated",
        "Runtime.executionContextsCleared",
        "Runtime.executionContextCreated",
    ] {
        assert!(
            !methods.contains(&forbidden),
            "unexpected {forbidden}: {methods:?}"
        );
    }
    let tree = cdp(&mut ctx, "Page.getFrameTree", json!({})).await;
    assert_eq!(tree["frameTree"]["frame"]["loaderId"], loader_id);
    for retained_context in [context_id, isolated] {
        let result = cdp(
            &mut ctx,
            "Runtime.evaluate",
            json!({"expression": "globalThis.retainedMarker", "contextId": retained_context, "returnByValue": true}),
        )
        .await;
        assert!(result.get("exceptionDetails").is_none(), "{result}");
        if retained_context == context_id {
            assert_eq!(result["result"]["value"].as_f64(), Some(42.0));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn evaluated_history_preserves_loader_and_execution_contexts() {
    assert_history_navigation(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn clicked_history_preserves_loader_and_execution_contexts() {
    assert_history_navigation(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn clicked_data_document_replaces_execution_contexts() {
    let (mut ctx, page_id, loader_id, context_id) = setup().await;
    cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "route.onclick = null; route.href = 'data:text/html,<h1>Destination</h1>'"}),
    )
    .await;
    ctx.pending_events.clear();
    click(&mut ctx).await;
    assert!(ctx
        .pending_events
        .iter()
        .any(|event| event.method == "Runtime.executionContextsCleared"));
    assert!(!ctx
        .pending_events
        .iter()
        .any(|event| event.method == "Page.navigatedWithinDocument"));
    let replacement = ctx
        .pending_events
        .iter()
        .find_map(|event| {
            (event.method == "Runtime.executionContextCreated"
                && event.params["context"]["auxData"]["isDefault"] == true
                && event.params["context"]["auxData"]["frameId"] == page_id)
                .then(|| event.params["context"]["id"].as_i64().unwrap())
        })
        .unwrap();
    assert_ne!(replacement, context_id);
    let tree = cdp(&mut ctx, "Page.getFrameTree", json!({})).await;
    assert_ne!(tree["frameTree"]["frame"]["loaderId"], loader_id);
    let result = cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "document.body.textContent", "contextId": replacement, "returnByValue": true}),
    )
    .await;
    assert_eq!(result["result"]["value"], "Destination");
}
