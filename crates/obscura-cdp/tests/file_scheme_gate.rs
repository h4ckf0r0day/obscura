//! Regression test for CDP-01 / SSRF-05: the file:// navigation gate
//! (`--allow-file-access`, off by default) must be enforced on the
//! `Page.navigate` *interception* path, not only in `do_navigate`.
//!
//! After a normal attach the session resolves, so `Page.navigate` is routed
//! through `process_with_interception` (server.rs), which previously called
//! `navigate_with_wait` with no gate — letting any CDP client read arbitrary
//! local files with default flags. This test drives that exact path.

use std::time::Duration;

use futures_util::{SinkExt, Stream, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Read websocket messages until one whose `id` matches `want_id`; return it.
async fn read_response(
    ws: &mut (impl Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    want_id: u64,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .ok_or("timeout waiting for response")?;
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
        if v.get("id").and_then(|x| x.as_u64()) == Some(want_id) {
            return Ok(v);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn page_navigate_file_scheme_rejected_by_default() {
    let port = pick_port().await;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            tokio::task::spawn_local(async move {
                // start() builds the context with allow_file_access = false.
                let _ = obscura_cdp::server::start(port).await;
            });
            tokio::time::sleep(Duration::from_millis(200)).await;

            let url = format!("ws://127.0.0.1:{}/devtools/browser", port);
            let (mut ws, _) = connect_async(&url).await.expect("ws connect");

            // createTarget -> auto-attach gives us a sessionId, so the
            // subsequent Page.navigate resolves the session and is routed
            // through process_with_interception (the path under test).
            ws.send(Message::Text(
                json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();

            let mut session_id: Option<String> = None;
            while session_id.is_none() {
                let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                    .await
                    .expect("timeout for createTarget")
                    .expect("ws closed")
                    .expect("ws error");
                if let Message::Text(t) = msg {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if let Some(s) = v
                        .get("params")
                        .and_then(|p| p.get("sessionId"))
                        .and_then(|s| s.as_str())
                    {
                        session_id = Some(s.to_string());
                    }
                }
            }
            let sid = session_id.unwrap();

            // file:// must be rejected by the gate, before any fs read — so the
            // error is the gate message on every platform (no "file not found").
            ws.send(Message::Text(
                json!({"id": 10, "method": "Page.navigate", "sessionId": sid,
                       "params": {"url": "file:///etc/passwd"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let resp = read_response(&mut ws, 10).await.expect("navigate response");
            let err = resp.get("error");
            assert!(
                err.is_some(),
                "file:// Page.navigate must be rejected on the interception path, got: {resp}"
            );
            let msg = err.unwrap().get("message").and_then(|m| m.as_str()).unwrap_or("");
            assert!(
                msg.contains("file://") && msg.contains("disabled"),
                "expected the file:// gate message, got: {msg}"
            );
            assert!(
                resp.get("result").is_none(),
                "rejected navigate must not return a result: {resp}"
            );

            // Control: a normal navigate on the SAME session must still succeed,
            // proving the early-return gate left page state intact.
            ws.send(Message::Text(
                json!({"id": 11, "method": "Page.navigate", "sessionId": sid,
                       "params": {"url": "data:text/html,<html><body><h1>ok</h1></body></html>"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let ok = read_response(&mut ws, 11).await.expect("control navigate response");
            assert!(
                ok.get("result").is_some() && ok.get("error").is_none(),
                "normal navigate after a rejected file:// must still succeed: {ok}"
            );
        })
        .await;
}
