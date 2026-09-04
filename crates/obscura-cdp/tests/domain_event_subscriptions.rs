use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: Option<&str>,
) -> Value {
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.map(str::to_string),
        },
        ctx,
    )
    .await;
    assert!(
        response.error.is_none(),
        "CDP {method} failed: {:?}",
        response.error,
    );
    response.result.unwrap_or_else(|| json!({}))
}

async fn attach(ctx: &mut CdpContext, target_id: &str, id: u64) -> String {
    let result = cdp(
        ctx,
        id,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )
    .await;
    result["sessionId"]
        .as_str()
        .expect("attached session id")
        .to_string()
}

async fn enable_domains(ctx: &mut CdpContext, session_id: &str, first_id: u64) {
    cdp(ctx, first_id, "Page.enable", json!({}), Some(session_id)).await;
    cdp(
        ctx,
        first_id + 1,
        "Page.setLifecycleEventsEnabled",
        json!({"enabled": true}),
        Some(session_id),
    )
    .await;
    cdp(
        ctx,
        first_id + 2,
        "Runtime.enable",
        json!({}),
        Some(session_id),
    )
    .await;
    cdp(
        ctx,
        first_id + 3,
        "Network.enable",
        json!({}),
        Some(session_id),
    )
    .await;
}

fn event_count(ctx: &CdpContext, session_id: &str, method: &str) -> usize {
    ctx.pending_events
        .iter()
        .filter(|event| event.session_id.as_deref() == Some(session_id))
        .filter(|event| event.method == method)
        .count()
}

fn lifecycle_count(ctx: &CdpContext, session_id: &str, name: &str) -> usize {
    ctx.pending_events
        .iter()
        .filter(|event| event.session_id.as_deref() == Some(session_id))
        .filter(|event| event.method == "Page.lifecycleEvent")
        .filter(|event| event.params["name"] == name)
        .count()
}

fn document_request_id(ctx: &CdpContext, session_id: &str) -> String {
    ctx.pending_events
        .iter()
        .find(|event| {
            event.session_id.as_deref() == Some(session_id)
                && event.method == "Network.responseReceived"
                && event.params["type"] == "Document"
        })
        .and_then(|event| event.params["requestId"].as_str())
        .expect("document response request id")
        .to_string()
}

async fn serve_pages() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for request_number in 1..=3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = socket.read(&mut buffer).await.unwrap();
            let body = format!("<html><body>page-{request_number}</body></html>");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    format!("http://{addr}")
}

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn ws_send(ws: &mut Ws, value: Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn ws_next_json(ws: &mut Ws) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for CDP message")
            .expect("CDP socket closed")
            .expect("CDP WebSocket error");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("CDP JSON message");
        }
    }
}

async fn ws_response(ws: &mut Ws, id: u64) -> Value {
    loop {
        let message = ws_next_json(ws).await;
        if message["id"] == id {
            return message;
        }
    }
}

async fn ws_attach(ws: &mut Ws, id: u64, target_id: &str) -> String {
    ws_send(
        ws,
        json!({
            "id": id,
            "method": "Target.attachToTarget",
            "params": {"targetId": target_id, "flatten": true},
        }),
    )
    .await;
    ws_response(ws, id).await["result"]["sessionId"]
        .as_str()
        .expect("attached session id")
        .to_string()
}

async fn ws_enable_domains(ws: &mut Ws, session_id: &str, first_id: u64) {
    for (offset, method, params) in [
        (0, "Page.enable", json!({})),
        (1, "Page.setLifecycleEventsEnabled", json!({"enabled": true})),
        (2, "Runtime.enable", json!({})),
        (3, "Network.enable", json!({})),
    ] {
        let id = first_id + offset;
        ws_send(
            ws,
            json!({"id": id, "method": method, "sessionId": session_id, "params": params}),
        )
        .await;
        let response = ws_response(ws, id).await;
        assert!(response.get("error").is_none(), "{method} failed: {response}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn navigation_events_follow_each_sessions_domain_subscriptions() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base_url = serve_pages().await;
    let mut ctx = CdpContext::new();
    let created = cdp(
        &mut ctx,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        None,
    )
    .await;
    let target_id = created["targetId"].as_str().unwrap().to_string();
    let first = attach(&mut ctx, &target_id, 2).await;
    let second = attach(&mut ctx, &target_id, 3).await;
    let other_created = cdp(
        &mut ctx,
        4,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        None,
    )
    .await;
    let other_target = other_created["targetId"].as_str().unwrap().to_string();
    let other = attach(&mut ctx, &other_target, 5).await;
    enable_domains(&mut ctx, &first, 10).await;
    enable_domains(&mut ctx, &second, 20).await;
    enable_domains(&mut ctx, &other, 40).await;
    ctx.pending_events.clear();

    cdp(
        &mut ctx,
        30,
        "Page.navigate",
        json!({"url": format!("{base_url}/first"), "waitUntil": "load"}),
        Some(&first),
    )
    .await;

    for session_id in [&first, &second] {
        assert_eq!(event_count(&ctx, session_id, "Page.frameNavigated"), 1);
        assert_eq!(event_count(&ctx, session_id, "Page.domContentEventFired"), 1);
        assert_eq!(event_count(&ctx, session_id, "Page.loadEventFired"), 1);
        assert_eq!(lifecycle_count(&ctx, session_id, "init"), 1);
        assert_eq!(lifecycle_count(&ctx, session_id, "commit"), 1);
        assert_eq!(lifecycle_count(&ctx, session_id, "DOMContentLoaded"), 1);
        assert_eq!(lifecycle_count(&ctx, session_id, "load"), 1);
        assert_eq!(event_count(&ctx, session_id, "Runtime.executionContextsCleared"), 1);
        assert_eq!(event_count(&ctx, session_id, "Runtime.executionContextCreated"), 2);
        assert_eq!(event_count(&ctx, session_id, "Network.requestWillBeSent"), 1);
        assert_eq!(event_count(&ctx, session_id, "Network.responseReceived"), 1);
        assert_eq!(event_count(&ctx, session_id, "Network.loadingFinished"), 1);
    }
    assert_eq!(event_count(&ctx, &other, "Page.frameNavigated"), 0);
    assert_eq!(event_count(&ctx, &other, "Page.lifecycleEvent"), 0);
    assert_eq!(event_count(&ctx, &other, "Runtime.executionContextCreated"), 0);
    assert_eq!(event_count(&ctx, &other, "Network.responseReceived"), 0);

    let request_id = document_request_id(&ctx, &first);
    cdp(
        &mut ctx,
        31,
        "Network.disable",
        json!({}),
        Some(&second),
    )
    .await;
    let body = cdp(
        &mut ctx,
        32,
        "Network.getResponseBody",
        json!({"requestId": request_id}),
        Some(&first),
    )
    .await;
    assert!(body["body"].as_str().is_some_and(|body| body.contains("page-1")));

    cdp(
        &mut ctx,
        33,
        "Page.setLifecycleEventsEnabled",
        json!({"enabled": false}),
        Some(&second),
    )
    .await;
    ctx.pending_events.clear();
    cdp(
        &mut ctx,
        34,
        "Page.navigate",
        json!({"url": format!("{base_url}/second"), "waitUntil": "load"}),
        Some(&first),
    )
    .await;

    assert_eq!(event_count(&ctx, &second, "Page.frameNavigated"), 1);
    assert_eq!(event_count(&ctx, &second, "Runtime.executionContextsCleared"), 1);
    assert_eq!(event_count(&ctx, &second, "Page.lifecycleEvent"), 0);
    assert_eq!(event_count(&ctx, &second, "Network.requestWillBeSent"), 0);
    assert_eq!(event_count(&ctx, &first, "Page.frameNavigated"), 1);
    assert_eq!(lifecycle_count(&ctx, &first, "load"), 1);
    assert_eq!(event_count(&ctx, &first, "Network.responseReceived"), 1);

    let last_network_request = document_request_id(&ctx, &first);
    cdp(&mut ctx, 35, "Page.disable", json!({}), Some(&second)).await;
    cdp(&mut ctx, 36, "Runtime.disable", json!({}), Some(&second)).await;

    cdp(
        &mut ctx,
        37,
        "Target.detachFromTarget",
        json!({"sessionId": first}),
        None,
    )
    .await;
    ctx.pending_events.clear();
    cdp(
        &mut ctx,
        38,
        "Page.navigate",
        json!({"url": format!("{base_url}/third"), "waitUntil": "load"}),
        Some(&second),
    )
    .await;

    assert!(ctx
        .pending_events
        .iter()
        .all(|event| event.session_id.as_deref() != Some(first.as_str())));
    assert_eq!(event_count(&ctx, &second, "Page.frameNavigated"), 0);
    assert_eq!(event_count(&ctx, &second, "Runtime.executionContextsCleared"), 0);
    assert_eq!(event_count(&ctx, &second, "Page.lifecycleEvent"), 0);
    assert_eq!(event_count(&ctx, &second, "Network.responseReceived"), 0);

    let cleared_body = dispatch(
        &CdpRequest {
            id: 39,
            method: "Network.getResponseBody".to_string(),
            params: json!({"requestId": last_network_request}),
            session_id: Some(second.clone()),
        },
        &mut ctx,
    )
    .await;
    assert!(cleared_body.error.is_some(), "last subscriber detach retained response bodies");
}

#[test]
fn websocket_enable_commands_drive_multi_session_routing() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();

    local.block_on(&runtime, async move {
        std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
        let base_url = serve_pages().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        tokio::task::spawn_local(async move {
            let _ = obscura_cdp::server::start(port).await;
        });

        let url = format!("ws://127.0.0.1:{port}/devtools/browser");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut ws = loop {
            match connect_async(&url).await {
                Ok((ws, _)) => break ws,
                Err(_error) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("CDP connection failed: {error}"),
            }
        };
        ws_send(
            &mut ws,
            json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}}),
        )
        .await;
        let target_id = ws_response(&mut ws, 1).await["result"]["targetId"]
            .as_str()
            .expect("created target id")
            .to_string();
        let first = ws_attach(&mut ws, 2, &target_id).await;
        let second = ws_attach(&mut ws, 3, &target_id).await;
        ws_enable_domains(&mut ws, &first, 10).await;
        ws_enable_domains(&mut ws, &second, 20).await;

        ws_send(
            &mut ws,
            json!({
                "id": 30,
                "method": "Page.navigate",
                "sessionId": first,
                "params": {"url": format!("{base_url}/initial"), "waitUntil": "load"},
            }),
        )
        .await;

        let mut frame = std::collections::HashMap::<String, usize>::new();
        let mut lifecycle_load = std::collections::HashMap::<String, usize>::new();
        let mut network_response = std::collections::HashMap::<String, usize>::new();
        let mut navigated = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !navigated
            || frame.get(&first).copied().unwrap_or(0) < 1
            || frame.get(&second).copied().unwrap_or(0) < 1
            || lifecycle_load.get(&first).copied().unwrap_or(0) < 1
            || lifecycle_load.get(&second).copied().unwrap_or(0) < 1
            || network_response.get(&first).copied().unwrap_or(0) < 1
            || network_response.get(&second).copied().unwrap_or(0) < 1
        {
            assert!(tokio::time::Instant::now() < deadline, "incomplete CDP event fan-out");
            let message = ws_next_json(&mut ws).await;
            if message["id"] == 30 {
                navigated = true;
                continue;
            }
            let Some(session_id) = message["sessionId"].as_str().map(str::to_string) else {
                continue;
            };
            match message["method"].as_str() {
                Some("Page.frameNavigated") => *frame.entry(session_id).or_default() += 1,
                Some("Page.lifecycleEvent") if message["params"]["name"] == "load" => {
                    *lifecycle_load.entry(session_id).or_default() += 1;
                }
                Some("Network.responseReceived") => {
                    *network_response.entry(session_id).or_default() += 1;
                }
                _ => {}
            }
        }

        ws_send(
            &mut ws,
            json!({"id": 31, "method": "Target.getTargets", "params": {}}),
        )
        .await;
        loop {
            let message = ws_next_json(&mut ws).await;
            if message["id"] == 31 {
                break;
            }
            let Some(session_id) = message["sessionId"].as_str().map(str::to_string) else {
                continue;
            };
            match message["method"].as_str() {
                Some("Page.frameNavigated") => *frame.entry(session_id).or_default() += 1,
                Some("Page.lifecycleEvent") if message["params"]["name"] == "load" => {
                    *lifecycle_load.entry(session_id).or_default() += 1;
                }
                Some("Network.responseReceived") => {
                    *network_response.entry(session_id).or_default() += 1;
                }
                _ => {}
            }
        }

        assert_eq!(frame.get(&first), Some(&1));
        assert_eq!(frame.get(&second), Some(&1));
        assert_eq!(lifecycle_load.get(&first), Some(&1));
        assert_eq!(lifecycle_load.get(&second), Some(&1));
        assert_eq!(network_response.get(&first), Some(&1));
        assert_eq!(network_response.get(&second), Some(&1));

        let first_fetch_url = format!("{base_url}/api-first");
        ws_send(
            &mut ws,
            json!({
                "id": 32,
                "method": "Runtime.evaluate",
                "sessionId": first,
                "params": {
                    "expression": format!("fetch('{}').then(function (r) {{ return r.text(); }})", first_fetch_url),
                    "awaitPromise": true,
                    "returnByValue": true,
                },
            }),
        )
        .await;
        let mut first_fetch_response = false;
        let mut first_fetch_request_id = None;
        let mut first_fetch_counts = std::collections::HashMap::<(String, String), usize>::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !first_fetch_response
            || [first.as_str(), second.as_str()].iter().any(|session_id| {
                [
                    "Network.requestWillBeSent",
                    "Network.responseReceived",
                    "Network.loadingFinished",
                ]
                .iter()
                .any(|method| {
                    first_fetch_counts
                        .get(&(session_id.to_string(), method.to_string()))
                        .copied()
                        .unwrap_or(0)
                        < 1
                })
            })
        {
            assert!(tokio::time::Instant::now() < deadline, "incomplete script-fetch fan-out");
            let message = ws_next_json(&mut ws).await;
            if message["id"] == 32 {
                assert!(message.get("error").is_none(), "fetch evaluation failed: {message}");
                first_fetch_response = true;
                continue;
            }
            let Some(session_id) = message["sessionId"].as_str() else {
                continue;
            };
            let Some(method) = message["method"].as_str() else {
                continue;
            };
            if method == "Network.requestWillBeSent"
                && message["params"]["request"]["url"] == first_fetch_url
            {
                first_fetch_request_id = message["params"]["requestId"]
                    .as_str()
                    .map(str::to_string);
            }
            if first_fetch_request_id.as_deref() == message["params"]["requestId"].as_str() {
                *first_fetch_counts
                    .entry((session_id.to_string(), method.to_string()))
                    .or_default() += 1;
            }
        }
        let first_fetch_request_id = first_fetch_request_id.expect("script fetch request id");
        for session_id in [&first, &second] {
            for method in [
                "Network.requestWillBeSent",
                "Network.responseReceived",
                "Network.loadingFinished",
            ] {
                assert_eq!(
                    first_fetch_counts.get(&(session_id.clone(), method.to_string())),
                    Some(&1),
                );
            }
        }

        ws_send(
            &mut ws,
            json!({
                "id": 33,
                "method": "Network.getResponseBody",
                "sessionId": first,
                "params": {"requestId": first_fetch_request_id},
            }),
        )
        .await;
        let body_response = ws_response(&mut ws, 33).await;
        assert_eq!(body_response["result"]["body"].as_str(), Some("<html><body>page-2</body></html>"));

        ws_send(
            &mut ws,
            json!({"id": 34, "method": "Network.disable", "sessionId": second, "params": {}}),
        )
        .await;
        assert!(ws_response(&mut ws, 34).await.get("error").is_none());

        let second_fetch_url = format!("{base_url}/api-second");
        ws_send(
            &mut ws,
            json!({
                "id": 35,
                "method": "Runtime.evaluate",
                "sessionId": first,
                "params": {
                    "expression": format!("fetch('{}').then(function (r) {{ return r.text(); }})", second_fetch_url),
                    "awaitPromise": true,
                    "returnByValue": true,
                },
            }),
        )
        .await;
        let mut second_fetch_response = false;
        let mut second_fetch_request_id = None;
        let mut second_fetch_counts = std::collections::HashMap::<(String, String), usize>::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !second_fetch_response
            || [
                "Network.requestWillBeSent",
                "Network.responseReceived",
                "Network.loadingFinished",
            ]
            .iter()
            .any(|method| {
                second_fetch_counts
                    .get(&(first.clone(), method.to_string()))
                    .copied()
                    .unwrap_or(0)
                    < 1
            })
        {
            assert!(tokio::time::Instant::now() < deadline, "incomplete second script fetch");
            let message = ws_next_json(&mut ws).await;
            if message["id"] == 35 {
                assert!(message.get("error").is_none(), "fetch evaluation failed: {message}");
                second_fetch_response = true;
                continue;
            }
            let Some(session_id) = message["sessionId"].as_str() else {
                continue;
            };
            let Some(method) = message["method"].as_str() else {
                continue;
            };
            if method == "Network.requestWillBeSent"
                && message["params"]["request"]["url"] == second_fetch_url
            {
                second_fetch_request_id = message["params"]["requestId"]
                    .as_str()
                    .map(str::to_string);
            }
            if second_fetch_request_id.as_deref() == message["params"]["requestId"].as_str() {
                *second_fetch_counts
                    .entry((session_id.to_string(), method.to_string()))
                    .or_default() += 1;
            }
        }
        ws_send(&mut ws, json!({"id": 36, "method": "Target.getTargets", "params": {}})).await;
        loop {
            let message = ws_next_json(&mut ws).await;
            if message["id"] == 36 {
                break;
            }
            let Some(session_id) = message["sessionId"].as_str() else {
                continue;
            };
            let Some(method) = message["method"].as_str() else {
                continue;
            };
            if second_fetch_request_id.as_deref() == message["params"]["requestId"].as_str() {
                *second_fetch_counts
                    .entry((session_id.to_string(), method.to_string()))
                    .or_default() += 1;
            }
        }
        for method in [
            "Network.requestWillBeSent",
            "Network.responseReceived",
            "Network.loadingFinished",
        ] {
            assert_eq!(
                second_fetch_counts.get(&(first.clone(), method.to_string())),
                Some(&1),
            );
            assert_eq!(
                second_fetch_counts
                    .get(&(second.clone(), method.to_string()))
                    .copied()
                    .unwrap_or(0),
                0,
            );
        }
        assert!(second_fetch_request_id.is_some());
        let _ = ws.close(None).await;
    });
}
