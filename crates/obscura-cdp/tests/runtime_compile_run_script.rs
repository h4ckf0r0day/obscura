//! `Runtime.compileScript` checks syntax and optionally keeps the source under
//! a script id; `Runtime.runScript` evaluates a kept script like
//! `Runtime.evaluate`.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::{CdpRequest, CdpResponse};
use serde_json::{json, Value};

async fn raw(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> CdpResponse {
    dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session.map(str::to_string),
        },
        ctx,
    )
    .await
}

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Value {
    let resp = raw(ctx, id, method, params, session).await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

/// Opens a page the way Puppeteer and Playwright do, rather than inserting a
/// session for a hand-made page, and returns `(targetId, sessionId)`.
async fn created_and_attached(ctx: &mut CdpContext) -> (String, String) {
    let created = cdp(ctx, 900, "Target.createTarget", json!({"url": "about:blank"}), None).await;
    let target_id = created["targetId"].as_str().expect("no targetId").to_string();
    let session = attach_to(ctx, &target_id, 901).await;
    (target_id, session)
}

async fn attach_to(ctx: &mut CdpContext, target_id: &str, id: u64) -> String {
    let attached = cdp(
        ctx,
        id,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )
    .await;
    attached["sessionId"]
        .as_str()
        .expect("no sessionId")
        .to_string()
}

async fn compile(ctx: &mut CdpContext, session: &str, expression: &str, persist: bool) -> Value {
    cdp(
        ctx,
        1,
        "Runtime.compileScript",
        json!({"expression": expression, "sourceURL": "", "persistScript": persist}),
        Some(session),
    )
    .await
}

#[tokio::test]
async fn compiled_script_runs_and_returns_value() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let compiled = compile(&mut ctx, &session, "1 + 2", true).await;
    let id = compiled["scriptId"].as_str().expect("no scriptId");
    let ran = cdp(
        &mut ctx,
        2,
        "Runtime.runScript",
        json!({"scriptId": id, "returnByValue": true}),
        Some(&session),
    )
    .await;
    assert_eq!(ran["result"]["value"].as_f64(), Some(3.0), "unexpected reply: {ran}");
}

#[tokio::test]
async fn run_script_awaits_promises() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let compiled = compile(&mut ctx, &session, "Promise.resolve(7)", true).await;
    let id = compiled["scriptId"].as_str().expect("no scriptId");
    let ran = cdp(
        &mut ctx,
        2,
        "Runtime.runScript",
        json!({"scriptId": id, "awaitPromise": true, "returnByValue": true}),
        Some(&session),
    )
    .await;
    assert_eq!(ran["result"]["value"].as_f64(), Some(7.0), "unexpected reply: {ran}");
}

#[tokio::test]
async fn syntax_error_reports_exception_details() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let compiled = compile(&mut ctx, &session, "1 +", true).await;
    assert!(compiled.get("scriptId").is_none(), "unexpected reply: {compiled}");
    let details = &compiled["exceptionDetails"];
    assert!(
        details["text"].as_str().is_some_and(|text| !text.is_empty()),
        "unexpected reply: {compiled}"
    );
    assert_eq!(details["lineNumber"], 0);
    assert!(details["exceptionId"].is_number(), "unexpected reply: {compiled}");

    let compiled = compile(&mut ctx, &session, "let a = 1;
let = ;", true).await;
    let details = &compiled["exceptionDetails"];
    assert!(details["exceptionId"].is_number(), "unexpected reply: {compiled}");
    assert_eq!(details["lineNumber"], 1, "unexpected reply: {compiled}");
    assert!(details["columnNumber"].is_number(), "unexpected reply: {compiled}");
}

#[tokio::test]
async fn unpersisted_script_returns_no_id() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let compiled = compile(&mut ctx, &session, "1 + 2", false).await;
    assert!(compiled.get("scriptId").is_none(), "unexpected reply: {compiled}");
    assert!(compiled.get("exceptionDetails").is_none(), "unexpected reply: {compiled}");
}

#[tokio::test]
async fn unknown_or_foreign_script_id_is_rejected() {
    let mut ctx = CdpContext::new();
    let (_, session_a) = created_and_attached(&mut ctx).await;
    let (_, session_b) = created_and_attached(&mut ctx).await;
    assert_ne!(session_a, session_b);

    let unknown = raw(
        &mut ctx,
        1,
        "Runtime.runScript",
        json!({"scriptId": "compiled-999"}),
        Some(&session_a),
    )
    .await;
    let err = unknown.error.expect("expected an error");
    assert!(
        format!("{err:?}").contains("No script with given id"),
        "unexpected error: {err:?}"
    );

    let compiled = compile(&mut ctx, &session_a, "1 + 2", true).await;
    let id = compiled["scriptId"].as_str().expect("no scriptId");
    let foreign = raw(
        &mut ctx,
        2,
        "Runtime.runScript",
        json!({"scriptId": id}),
        Some(&session_b),
    )
    .await;
    let err = foreign.error.expect("expected an error");
    assert!(
        format!("{err:?}").contains("No script with given id"),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn evaluate_still_works() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let result = cdp(
        &mut ctx,
        1,
        "Runtime.evaluate",
        json!({"expression": "6 * 7", "returnByValue": true}),
        Some(&session),
    )
    .await;
    assert_eq!(result["result"]["value"].as_f64(), Some(42.0), "unexpected reply: {result}");
}

/// `/` embeds `/child.html`, so a context id can point at a frame whose
/// document differs from the main one.
async fn serve() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let body = if request.starts_with("GET /child.html ") {
                    "<html><body><h2>Child heading</h2></body></html>"
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

#[tokio::test(flavor = "current_thread")]
async fn run_script_honors_execution_context_id() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    cdp(&mut ctx, 1, "Runtime.enable", json!({}), Some(&session)).await;
    cdp(&mut ctx, 2, "Page.navigate", json!({"url": url}), Some(&session)).await;
    cdp(&mut ctx, 3, "Runtime.evaluate", json!({"expression": "1"}), Some(&session)).await;
    let tree = cdp(&mut ctx, 4, "Page.getFrameTree", json!({}), Some(&session)).await;
    let child_id = tree["frameTree"]["childFrames"][0]["frame"]["id"]
        .as_str()
        .expect("no child frame")
        .to_string();
    let context_id = ctx
        .pending_events
        .iter()
        .find(|event| {
            event.method == "Runtime.executionContextCreated"
                && event.session_id.as_deref() == Some(session.as_str())
                && event.params["context"]["auxData"]["frameId"] == child_id.as_str()
                && event.params["context"]["auxData"]["isDefault"] == true
        })
        .and_then(|event| event.params["context"]["id"].as_i64())
        .expect("no default execution context for the child frame");

    let compiled = compile(&mut ctx, &session, "document.URL", true).await;
    let id = compiled["scriptId"].as_str().expect("no scriptId").to_string();
    let in_child = cdp(
        &mut ctx,
        5,
        "Runtime.runScript",
        json!({"scriptId": id, "executionContextId": context_id, "returnByValue": true}),
        Some(&session),
    )
    .await;
    assert_eq!(
        in_child["result"]["value"],
        format!("{url}child.html"),
        "unexpected reply: {in_child}"
    );
    let in_main = cdp(
        &mut ctx,
        6,
        "Runtime.runScript",
        json!({"scriptId": id, "returnByValue": true}),
        Some(&session),
    )
    .await;
    assert_eq!(in_main["result"]["value"], url, "unexpected reply: {in_main}");
}

#[tokio::test]
async fn run_script_rejects_an_unknown_execution_context_id() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let compiled = compile(&mut ctx, &session, "1 + 2", true).await;
    let id = compiled["scriptId"].as_str().expect("no scriptId");
    let resp = raw(
        &mut ctx,
        2,
        "Runtime.runScript",
        json!({"scriptId": id, "executionContextId": 424242}),
        Some(&session),
    )
    .await;
    let err = format!("{:?}", resp.error.expect("expected an error"));
    assert!(
        err.contains("Cannot find context with specified id: 424242"),
        "unexpected error: {err}"
    );
}

/// An awaited `runScript` must be deferred like an awaited `evaluate`: while it
/// waits on a `fetch()` that `Fetch` interception has paused, the connection
/// still has to deliver `Fetch.requestPaused` and accept the reply that
/// releases the request. A blocking `runScript` would sit until its timeout.
#[tokio::test(flavor = "current_thread")]
async fn awaited_run_script_does_not_block_fetch_interception() {
    use futures_util::{SinkExt, StreamExt};
    use std::time::Duration;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            tokio::time::sleep(Duration::from_millis(250)).await;
            let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/devtools/browser"))
                .await
                .unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let mut next_id = 0u64;
            let mut session = String::new();
            let mut paused = false;
            let mut sent: Vec<(u64, String)> = Vec::new();

            next_id += 1;
            sent.push((next_id, "create".to_string()));
            ws.send(Message::Text(
                json!({"id": next_id, "method": "Target.createTarget", "params": {"url": "about:blank"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();

            loop {
                let frame = tokio::time::timeout_at(deadline, ws.next())
                    .await
                    .unwrap_or_else(|_| panic!("timed out; paused={paused}, sent={sent:?}"))
                    .expect("connection closed")
                    .unwrap();
                let Message::Text(text) = frame else { continue };
                let msg: Value = serde_json::from_str(&text).unwrap();
                let mut outgoing: Option<(String, &str, Value)> = None;
                if msg["method"] == "Fetch.requestPaused" && !paused {
                    paused = true;
                    outgoing = Some((
                        "fulfill".to_string(),
                        "Fetch.fulfillRequest",
                        json!({
                            "requestId": msg["params"]["requestId"],
                            "responseCode": 200,
                            "responseHeaders": [{"name": "Content-Type", "value": "text/plain"}],
                            "body": "aGk=",
                        }),
                    ));
                } else if let Some(id) = msg["id"].as_u64() {
                    let label = sent
                        .iter()
                        .find(|(sent_id, _)| *sent_id == id)
                        .map(|(_, label)| label.as_str());
                    match label {
                        Some("create") => {
                            let target = msg["result"]["targetId"].as_str().unwrap();
                            session = format!("{target}-session");
                            outgoing = Some((
                                "fetch.enable".to_string(),
                                "Fetch.enable",
                                json!({"patterns": [{"urlPattern": "*"}]}),
                            ));
                        }
                        Some("fetch.enable") => {
                            outgoing = Some((
                                "compile".to_string(),
                                "Runtime.compileScript",
                                json!({
                                    "expression": "fetch('https://obscura-run-script.invalid/x').then(r => r.text())",
                                    "sourceURL": "",
                                    "persistScript": true,
                                }),
                            ));
                        }
                        Some("compile") => {
                            outgoing = Some((
                                "run".to_string(),
                                "Runtime.runScript",
                                json!({
                                    "scriptId": msg["result"]["scriptId"],
                                    "awaitPromise": true,
                                    "returnByValue": true,
                                }),
                            ));
                        }
                        Some("run") => {
                            assert!(paused, "script finished without a paused request: {msg}");
                            assert!(msg.get("error").is_none(), "runScript failed: {msg}");
                            assert_eq!(msg["result"]["result"]["value"], "hi", "reply: {msg}");
                            return;
                        }
                        _ => {}
                    }
                }
                if let Some((label, method, params)) = outgoing {
                    next_id += 1;
                    sent.push((next_id, label));
                    let mut message = json!({"id": next_id, "method": method, "params": params});
                    if method != "Target.createTarget" {
                        message["sessionId"] = json!(session);
                    }
                    ws.send(Message::Text(message.to_string().into())).await.unwrap();
                }
            }
        })
        .await;
}
