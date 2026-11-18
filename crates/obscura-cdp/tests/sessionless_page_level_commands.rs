//! Page-level DevTools connections (`/devtools/page/{id}` — the endpoint
//! /json/list advertises) send CDP commands WITHOUT a sessionId. Chrome binds
//! such a connection to that page; obscura used to answer every page-domain
//! command with "No page" / "No page for session", which broke raw page-ws
//! clients (lightbrowse, hand-rolled ws clients) while browser-level
//! multiplexing (playwright/puppeteer via Target.attachToTarget) kept working.
//!
//! The fix: the accept thread remembers the `/devtools/page/{id}` upgrade
//! target, and the processor binds sessionless commands from that connection
//! to an implicit session on the target page.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

#[tokio::test(flavor = "current_thread")]
async fn page_level_websocket_commands_work_without_a_session_id() {
    let port = pick_port().await;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Connect straight to the page-level endpoint /json/list
            // advertises — no Target.attachToTarget, no sessionId anywhere.
            let url = format!("ws://127.0.0.1:{}/devtools/page/page-1", port);
            let (mut ws, _) = connect_async(&url).await.unwrap();

            // Runtime.evaluate — used to fail with "No page".
            ws.send(Message::Text(
                json!({ "id": 1, "method": "Runtime.evaluate", "params": json!({ "expression": "1 + 1", "returnByValue": true }) })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let reply = next_reply_for_id(&mut ws, 1).await;
            assert_eq!(reply["result"]["result"]["value"], json!(2.0),
                "sessionless evaluate must reach the default page: {reply}");

            // Page domain — used to fail with "No page for session".
            ws.send(Message::Text(
                json!({ "id": 2, "method": "Page.enable", "params": {} })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let reply = next_reply_for_id(&mut ws, 2).await;
            assert!(reply.get("error").is_none(), "Page.enable: {reply}");

            ws.send(Message::Text(
                json!({ "id": 3, "method": "Emulation.setUserAgentOverride", "params": json!({ "userAgent": "sessionless/1.0" }) })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let reply = next_reply_for_id(&mut ws, 3).await;
            assert!(reply.get("error").is_none(), "UA override: {reply}");

            // The lightbrowse flow: Page.enable, then navigate, then WAIT for
            // Page.loadEventFired on the same sessionless connection.
            ws.send(Message::Text(
                json!({ "id": 4, "method": "Page.navigate", "params": json!({ "url": "about:blank" }) })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let reply = next_reply_for_id(&mut ws, 4).await;
            assert!(reply.get("error").is_none(), "navigate: {reply}");

            let saw_load = wait_for_event(&mut ws, "Page.loadEventFired", 5_000.0).await;
            assert!(saw_load, "sessionless connection must receive Page.loadEventFired");
        })
        .await;
}

async fn next_reply_for_id(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for reply {id}");
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("reply timeout")
            .expect("ws closed")
            .expect("ws error");
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return v;
            }
        }
    }
}

async fn wait_for_event(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    method: &str,
    _budget_ms: f64,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                if v["method"] == method {
                    return true;
                }
            }
            _ => break,
        }
    }
    false
}
