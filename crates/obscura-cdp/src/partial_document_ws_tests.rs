//! Partial documents must keep their failure semantics through the managed CDP server.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

#[path = "../../obscura-net/tests/support/partial_server.rs"]
mod fixture;

struct Ws {
    inner: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    next_id: u64,
    replies: HashMap<u64, Value>,
    events: Vec<Value>,
}

impl Ws {
    fn record(&mut self, value: Value) {
        if let Some(id) = value["id"].as_u64() {
            self.replies.insert(id, value);
        } else {
            self.events.push(value);
        }
    }

    async fn command(&mut self, session: Option<&str>, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.inner.send(Message::Text(json!({
            "id":id, "method":method, "params":params, "sessionId":session,
        }).to_string().into())).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(reply) = self.replies.remove(&id) { return reply; }
            let remaining = deadline.checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("no reply to {method}"));
            let message = tokio::time::timeout(remaining, self.inner.next()).await
                .unwrap_or_else(|_| panic!("no reply to {method}"))
                .expect("CDP connection closed").expect("CDP WebSocket read");
            if let Message::Text(text) = message {
                self.record(serde_json::from_str(&text).unwrap());
            }
        }
    }

    async fn collect_for(&mut self, duration: Duration) {
        let deadline = tokio::time::Instant::now() + duration;
        loop {
            match tokio::time::timeout_at(deadline, self.inner.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    self.record(serde_json::from_str(&text).unwrap());
                }
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
                Err(_) => break,
                other => panic!("CDP connection ended while collecting events: {other:?}"),
            }
        }
    }

    async fn barrier(&mut self) {
        successful(self.command(None, "Target.getTargets", json!({})).await);
        self.collect_for(Duration::from_millis(50)).await;
    }

    fn page_events(&self, session: &str) -> Vec<&Value> {
        self.events.iter().filter(|event| event["sessionId"] == session).collect()
    }
}

fn successful(reply: Value) -> Value {
    assert!(reply.get("error").is_none(), "{reply}");
    reply["result"].clone()
}

async fn start_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = tokio::task::spawn_local(async move {
        let _ = crate::server::start_with_full_serve_options(
            port, "127.0.0.1", None, false, None, false, None, true,
        ).await;
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return (port, server);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("CDP server did not start");
}

async fn open_page(port: u16) -> (Ws, String) {
    let (inner, _) = connect_async(format!("ws://127.0.0.1:{port}/devtools/browser")).await.unwrap();
    let mut ws = Ws { inner, next_id:0, replies:HashMap::new(), events:Vec::new() };
    let created = successful(ws.command(None, "Target.createTarget", json!({"url":"about:blank"})).await);
    let attached = successful(ws.command(None, "Target.attachToTarget",
        json!({"targetId":created["targetId"],"flatten":true})).await);
    let session = attached["sessionId"].as_str().unwrap().to_string();
    for method in ["Page.enable", "Network.enable", "Runtime.enable"] {
        successful(ws.command(Some(&session), method, json!({})).await);
    }
    successful(ws.command(Some(&session), "Page.setLifecycleEventsEnabled", json!({"enabled":true})).await);
    (ws, session)
}

async fn evaluate(ws: &mut Ws, session: &str, expression: &str) -> Value {
    successful(ws.command(Some(session), "Runtime.evaluate",
        json!({"expression":expression,"returnByValue":true})).await)["result"]["value"].clone()
}

fn assert_no_completion(events: &[&Value]) {
    for event in events {
        let method = event["method"].as_str().unwrap_or_default();
        assert!(!matches!(method, "Network.loadingFinished" | "Page.loadEventFired" | "Page.domContentEventFired"),
            "failed document must not complete: {event}");
        if method == "Page.lifecycleEvent" {
            assert!(!matches!(event["params"]["name"].as_str(), Some("load" | "DOMContentLoaded")),
                "failed document must not publish a completed lifecycle: {event}");
        }
    }
}

fn assert_partial_failure(ws: &Ws, session: &str, url: &str) {
    let events = ws.page_events(session);
    let responses: Vec<_> = events.iter().filter(|event| event["method"] == "Network.responseReceived"
        && event["params"]["type"] == "Document").collect();
    assert_eq!(responses.len(), 1, "one real document response: {events:?}");
    assert_eq!(responses[0]["params"]["response"]["url"], url);
    assert_eq!(responses[0]["params"]["response"]["status"], 200);
    let failed: Vec<_> = events.iter().filter(|event| event["method"] == "Network.loadingFailed").collect();
    assert_eq!(failed.len(), 1, "exactly one failure: {events:?}");
    assert_eq!(failed[0]["params"]["requestId"], responses[0]["params"]["requestId"]);
    assert_eq!(failed[0]["params"]["errorText"], "net::ERR_TIMED_OUT");
    let response_index = events.iter().position(|event| event["method"] == "Network.responseReceived").unwrap();
    let failure_index = events.iter().position(|event| event["method"] == "Network.loadingFailed").unwrap();
    assert!(response_index < failure_index, "headers precede transfer failure");
    assert!(events.iter().any(|event| event["method"] == "Page.frameNavigated"
        && event["params"]["frame"]["url"] == url), "received HTML became the current document");
    assert_no_completion(&events);
}

async fn assert_partial_inert(ws: &mut Ws, session: &str) {
    assert_eq!(evaluate(ws, session, r#"(() => [document.title,document.readyState,
        typeof oldMarker,typeof prefixRan,typeof eofRan,typeof preloadRan,typeof frameRan,
        document.getElementById('arrived').textContent])()"#).await,
        json!(["partial","loading","undefined","undefined","undefined","undefined","undefined","received"]));
    let tree = successful(ws.command(Some(session), "Page.getFrameTree", json!({})).await);
    assert!(tree["frameTree"]["childFrames"].as_array().is_none_or(Vec::is_empty),
        "retained iframe markup must not activate its author script: {tree}");
}

#[tokio::test(flavor = "current_thread")]
async fn managed_partial_document_reports_headers_once_without_load_completion() {
    std::env::set_var("OBSCURA_NAV_TIMEOUT_MS", "350");
    tokio::task::LocalSet::new().run_until(async {
        let (origin, _) = fixture::serve().await;
        let (port, server) = start_server().await;
        let (mut ws, session) = open_page(port).await;
        successful(ws.command(Some(&session), "Page.navigate", json!({"url":
            "data:text/html,<title>old</title><script>globalThis.oldMarker=true;</script>"})).await);
        successful(ws.command(Some(&session), "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":"globalThis.preloadRan=true;"})).await);
        ws.barrier().await;
        ws.events.clear();
        let url = format!("{origin}/slow");
        let failed = ws.command(Some(&session), "Page.navigate", json!({"url":url})).await;
        assert!(failed.get("error").is_some(), "partial navigation stays a failure: {failed}");
        ws.barrier().await;
        assert_partial_failure(&ws, &session, &url);
        assert_partial_inert(&mut ws, &session).await;
        ws.collect_for(Duration::from_millis(100)).await;
        assert_no_completion(&ws.page_events(&session));
        successful(ws.command(Some(&session), "Page.navigate", json!({"url":format!("{origin}/healthy")})).await);
        assert_eq!(evaluate(&mut ws, &session, "[document.title,document.readyState,healthyRan,preloadRan]").await,
            json!(["healthy","complete",true,true]));
        ws.inner.close(None).await.unwrap();
        server.abort();
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_empty_failures_preserve_context_remote_nodes_and_dom_registration() {
    std::env::set_var("OBSCURA_NAV_TIMEOUT_MS", "350");
    tokio::task::LocalSet::new().run_until(async {
        let (origin, _) = fixture::serve().await;
        let (port, server) = start_server().await;
        let (mut ws, session) = open_page(port).await;
        successful(ws.command(Some(&session), "Page.navigate", json!({"url":
            "data:text/html,<title>old</title><p id=keep>old body</p><script>globalThis.oldMarker=true;</script>"})).await);
        let document = successful(ws.command(Some(&session), "DOM.getDocument", json!({"depth":-1})).await);
        let selected = successful(ws.command(Some(&session), "DOM.querySelector",
            json!({"nodeId":document["root"]["nodeId"],"selector":"#keep"})).await);
        let node = selected["nodeId"].as_u64().unwrap();
        assert!(node > 0);
        let remote = successful(ws.command(Some(&session), "Runtime.evaluate", json!({
            "expression":"(() => {const n=document.getElementById('keep');n.answer=42;return n})()"})).await);
        let object = remote["result"]["objectId"].as_str().unwrap().to_string();
        let context = ws.page_events(&session).iter().rev().find(|event|
            event["method"] == "Runtime.executionContextCreated"
            && event["params"]["context"]["auxData"]["isDefault"] == true)
            .expect("old execution context")["params"]["context"]["id"].as_i64().unwrap();
        for path in ["noheaders", "nobytes"] {
            ws.barrier().await;
            ws.events.clear();
            let failed = ws.command(Some(&session), "Page.navigate", json!({"url":format!("{origin}/{path}")})).await;
            assert!(failed.get("error").is_some(), "{path}: {failed}");
            ws.barrier().await;
            let events = ws.page_events(&session);
            assert_eq!(events.iter().filter(|event| event["method"] == "Network.loadingFailed").count(), 1, "{path}: {events:?}");
            assert!(!events.iter().any(|event| matches!(event["method"].as_str(),
                Some("Page.frameNavigated" | "Runtime.executionContextsCleared" | "Runtime.executionContextDestroyed"
                    | "Runtime.executionContextCreated" | "DOM.documentUpdated"))), "uncommitted failure changed document: {path}: {events:?}");
            assert_no_completion(&events);
            let unchanged = successful(ws.command(Some(&session), "Runtime.evaluate", json!({
                "contextId":context,"expression":"[document.title,oldMarker]","returnByValue":true})).await);
            assert_eq!(unchanged["result"]["value"], json!(["old",true]), "{path}");
            let called = successful(ws.command(Some(&session), "Runtime.callFunctionOn", json!({
                "objectId":object,"functionDeclaration":"function(){return this.answer+':'+this.textContent}","returnByValue":true})).await);
            assert_eq!(called["result"]["value"], "42:old body", "{path}");
            let registered = successful(ws.command(Some(&session), "DOM.requestNode", json!({"objectId":object})).await);
            assert_eq!(registered["nodeId"], node, "DOM.getDocument registration survived {path}");
        }
        ws.inner.close(None).await.unwrap();
        server.abort();
    }).await;
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn managed_screencast_acks_do_not_activate_a_partial_document() {
    use base64::Engine as _;
    std::env::set_var("OBSCURA_NAV_TIMEOUT_MS", "350");
    tokio::task::LocalSet::new().run_until(async {
        let (origin, _) = fixture::serve().await;
        let (port, server) = start_server().await;
        let (mut ws, session) = open_page(port).await;
        successful(ws.command(Some(&session), "Page.navigate", json!({"url":
            "data:text/html,<title>old</title><body style='background:blue'><script>globalThis.oldMarker=true;</script>"})).await);
        successful(ws.command(Some(&session), "Page.addScriptToEvaluateOnNewDocument",
            json!({"source":"globalThis.preloadRan=true;"})).await);
        successful(ws.command(Some(&session), "Page.startScreencast",
            json!({"format":"png","maxWidth":64,"maxHeight":64})).await);
        let initial = ws.page_events(&session).into_iter().find(|event| event["method"] == "Page.screencastFrame")
            .expect("initial recording frame")["params"]["sessionId"].clone();
        successful(ws.command(Some(&session), "Page.screencastFrameAck", json!({"sessionId":initial})).await);
        ws.barrier().await;
        ws.events.clear();
        let url = format!("{origin}/slow");
        let failed = ws.command(Some(&session), "Page.navigate", json!({"url":url})).await;
        assert!(failed.get("error").is_some(), "{failed}");
        ws.barrier().await;
        assert_partial_failure(&ws, &session, &url);
        let frames: Vec<_> = ws.page_events(&session).into_iter()
            .filter(|event| event["method"] == "Page.screencastFrame").cloned().collect();
        assert!(!frames.is_empty(), "active recording observes received HTML");
        for frame in frames {
            let png = base64::engine::general_purpose::STANDARD.decode(frame["params"]["data"].as_str().unwrap()).unwrap();
            let image = image::load_from_memory(&png).unwrap().to_rgba8();
            assert!(image.width() > 0 && image.height() > 0);
            let first = image.get_pixel(0, 0).0;
            assert!(image.pixels().any(|pixel| pixel.0 != first), "partial text produces a nonblank capture");
            successful(ws.command(Some(&session), "Page.screencastFrameAck",
                json!({"sessionId":frame["params"]["sessionId"]})).await);
        }
        ws.collect_for(Duration::from_millis(150)).await;
        assert_partial_inert(&mut ws, &session).await;
        assert_no_completion(&ws.page_events(&session));
        successful(ws.command(Some(&session), "Page.stopScreencast", json!({})).await);
        ws.inner.close(None).await.unwrap();
        server.abort();
    }).await;
}
