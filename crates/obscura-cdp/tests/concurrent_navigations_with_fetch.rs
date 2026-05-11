//! Issue #19 hard repro: 5 parallel clients with Fetch interception enabled
//! must not abort V8.
//!
//! Per maintainer comment on PR #36 (https://github.com/h4ckf0r0day/obscura/pull/36#issuecomment-4328238087):
//!
//!   "Tested locally and the V8 fatal still reproduces when Fetch.enable
//!    interception is on with concurrent navigations:
//!    - 5 clients enable Fetch with patterns: ["*"]
//!    - All 5 issue Page.navigate concurrently
//!    - Server aborts with: Check failed: heap->isolate() == Isolate::TryGetCurrent()"
//!
//! The smoke test (concurrent_navigations.rs) used data: URLs which skip
//! the subresource fetch path that drives the abort. This test:
//!   1. Calls Fetch.enable patterns:["*"] on each client
//!   2. Navigates to a URL with subresources (so op_fetch_url is reached
//!      and parks on resolve_rx.await INSIDE the v8_lock guard)
//!   3. Auto-replies to Fetch.requestPaused with Fetch.continueRequest
//!      (so the parked op resumes — that's where two Isolates can collide)
//!
//! Run with `cargo test -p obscura-cdp --test concurrent_navigations_with_fetch
//! -- --nocapture --ignored`.

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

/// One client:
///   1. Target.createTarget → sessionId
///   2. Fetch.enable patterns:["*"]
///   3. Page.navigate to URL
///   4. Loop: respond to Fetch.requestPaused with Fetch.continueRequest
///      until Page.navigate's response arrives.
async fn one_client_with_fetch(port: u16, id_base: u64, target_url: &str) -> Result<(), String> {
    let url = format!("ws://127.0.0.1:{}/devtools/browser", port);
    let (mut ws, _) = connect_async(&url).await.map_err(|e| e.to_string())?;

    // 1. Target.createTarget
    let create = json!({
        "id": id_base,
        "method": "Target.createTarget",
        "params": {"url": "about:blank"},
    });
    ws.send(Message::Text(create.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;

    let mut session_id: Option<String> = None;
    while session_id.is_none() {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .map_err(|_| "timeout waiting for createTarget".to_string())?
            .ok_or("ws closed")?
            .map_err(|e| e.to_string())?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            _ => continue,
        };
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(s) = v
            .get("params")
            .and_then(|p| p.get("sessionId"))
            .and_then(|s| s.as_str())
        {
            session_id = Some(s.to_string());
        }
    }
    let sid = session_id.unwrap();

    // 2. Fetch.enable patterns:["*"]
    let fetch_enable = json!({
        "id": id_base + 1,
        "method": "Fetch.enable",
        "sessionId": sid,
        "params": {
            "patterns": [{"urlPattern": "*"}],
        },
    });
    ws.send(Message::Text(fetch_enable.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;

    // Wait for Fetch.enable response.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("timeout waiting for Fetch.enable response".to_string());
        }
        let remaining = deadline - tokio::time::Instant::now();
        let msg = tokio::time::timeout(remaining, ws.next())
            .await
            .map_err(|_| "timeout".to_string())?
            .ok_or("ws closed")?
            .map_err(|e| e.to_string())?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            _ => continue,
        };
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if v.get("id").and_then(|x| x.as_u64()) == Some(id_base + 1) {
            break;
        }
    }

    // 3. Page.navigate
    let nav = json!({
        "id": id_base + 2,
        "method": "Page.navigate",
        "sessionId": sid,
        "params": {"url": target_url},
    });
    ws.send(Message::Text(nav.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;

    // 4. Drain events. Auto-respond to Fetch.requestPaused with continueRequest.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut auto_id = id_base + 1000;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("client {} timeout waiting for navigate", id_base));
        }
        let remaining = deadline - tokio::time::Instant::now();
        let msg = tokio::time::timeout(remaining, ws.next())
            .await
            .map_err(|_| "timeout".to_string())?
            .ok_or("ws closed mid-navigate")?
            .map_err(|e| e.to_string())?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            _ => continue,
        };
        let v: Value = match serde_json::from_str::<Value>(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // navigate response?
        if v.get("id").and_then(|x| x.as_u64()) == Some(id_base + 2) {
            return Ok(());
        }

        // Fetch.requestPaused event?
        if v.get("method").and_then(|m| m.as_str()) == Some("Fetch.requestPaused") {
            let req_id = v
                .get("params")
                .and_then(|p| p.get("requestId"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            auto_id += 1;
            let cont = json!({
                "id": auto_id,
                "method": "Fetch.continueRequest",
                "sessionId": sid,
                "params": {"requestId": req_id},
            });
            ws.send(Message::Text(cont.to_string().into()))
                .await
                .map_err(|e| e.to_string())?;
        }
    }
}

/// PR #36 maintainer's exact repro.
#[tokio::test(flavor = "current_thread")]
#[ignore = "boots a real CDP server + makes outbound HTTP; opt-in with --ignored"]
async fn fetch_intercept_concurrency_5_does_not_abort_v8() {
    // Use example.com first — minimal subresources but real network. If this
    // doesn't reproduce, escalate to a JS-heavy page in a follow-up.
    let target_url = "https://example.com/";

    let port = pick_port().await;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            tokio::time::sleep(Duration::from_millis(250)).await;

            let mut handles = Vec::new();
            for i in 0..5u64 {
                let id_base = (i + 1) * 1000;
                let url = target_url.to_string();
                handles.push(tokio::task::spawn_local(async move {
                    one_client_with_fetch(port, id_base, &url).await
                }));
            }

            let mut ok = 0usize;
            let mut errors = Vec::new();
            for (i, h) in handles.into_iter().enumerate() {
                match h.await {
                    Ok(Ok(())) => ok += 1,
                    Ok(Err(e)) => errors.push(format!("client {}: {}", i, e)),
                    Err(e) => errors.push(format!("client {} join: {}", i, e)),
                }
            }
            if !errors.is_empty() {
                panic!("errors: {:#?}", errors);
            }
            assert_eq!(ok, 5, "all 5 concurrent clients must complete navigate");
        })
        .await;
}
