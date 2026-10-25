// A CDP client must not read local files unless the server was started with
// --allow-file-access, whichever route the navigation takes. The real
// Page.navigate path is the server's spawn-and-defer path, not the domain
// handler, and a page's own `location.href` assignment is consumed after
// Runtime.evaluate; both must be gated.

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

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Send one command and return its response, collecting any `sessionId`
/// announced by an event on the way (Target.attachedToTarget).
async fn call(ws: &mut Ws, id: u64, method: &str, params: Value, session: Option<&str>) -> (Value, Option<String>) {
    let mut msg = json!({"id": id, "method": method, "params": params});
    if let Some(session) = session {
        msg["sessionId"] = json!(session);
    }
    ws.send(Message::Text(msg.to_string().into())).await.unwrap();
    let mut announced = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = tokio::time::timeout(remaining, ws.next())
            .await
            .unwrap_or_else(|_| panic!("timeout waiting for {method}"))
            .expect("ws closed")
            .unwrap();
        let Message::Text(text) = frame else { continue };
        let v: Value = serde_json::from_str(&text).unwrap();
        if let Some(s) = v.get("params").and_then(|p| p.get("sessionId")).and_then(Value::as_str) {
            announced = Some(s.to_string());
        }
        if v.get("id").and_then(Value::as_u64) == Some(id) {
            return (v, announced);
        }
    }
}

fn evaluate_value(response: &Value) -> String {
    response["result"]["result"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn cdp_cannot_reach_local_files_without_allow_file_access() {
    let path = std::env::temp_dir().join(format!("obscura-cdp-file-gate-{}.html", std::process::id()));
    std::fs::write(&path, "<p>local-secret</p>").unwrap();
    let file_url = url::Url::from_file_path(&path).unwrap().to_string();

    let port = pick_port().await;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            tokio::time::sleep(Duration::from_millis(200)).await;

            let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/devtools/browser"))
                .await
                .expect("connect");
            let (_, session) = call(&mut ws, 1, "Target.createTarget", json!({"url": "about:blank"}), None).await;
            let session = session.expect("attached session");
            let sid = session.as_str();

            // The spawn-and-defer Page.navigate path.
            let (reply, _) = call(&mut ws, 2, "Page.navigate", json!({"url": file_url}), Some(sid)).await;
            let message = reply["error"]["message"].as_str().unwrap_or_default().to_string();
            assert!(message.contains("file://"), "Page.navigate must refuse file://: {reply}");
            let (reply, _) = call(&mut ws, 3, "Runtime.evaluate", json!({"expression": "location.href", "returnByValue": true}), Some(sid)).await;
            assert!(!evaluate_value(&reply).starts_with("file:"), "{reply}");

            // A page-driven navigation consumed after Runtime.evaluate.
            call(&mut ws, 4, "Page.navigate", json!({"url": "data:text/html,<p>web</p>"}), Some(sid)).await;
            let assign = format!("location.href = {}", serde_json::to_string(&file_url).unwrap());
            call(&mut ws, 5, "Runtime.evaluate", json!({"expression": assign}), Some(sid)).await;
            let (reply, _) = call(
                &mut ws,
                6,
                "Runtime.evaluate",
                json!({"expression": "location.href + ' | ' + document.body.textContent", "returnByValue": true}),
                Some(sid),
            )
            .await;
            let state = evaluate_value(&reply);
            assert!(state.starts_with("data:"), "page must stay on its web document: {state}");
            assert!(!state.contains("local-secret"), "{state}");
        })
        .await;
    let _ = std::fs::remove_file(&path);
}
