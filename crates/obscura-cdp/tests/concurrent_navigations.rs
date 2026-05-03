//! Issue #19 smoke test: 5 parallel CDP clients each performing
//! `Target.createTarget` + `Page.navigate` must not abort the process.
//!
//! NOTE on coverage. The deterministic abort in #19 requires the navigations
//! to interleave on the shared `LocalSet` thread, which only happens once
//! `navigate_single` actually yields — the heaviest yields come from
//! subresource fetches in `futures::future::join_all` (page.rs:285) when the
//! page has scripts/images to pull. `data:` URLs skip every fetch, so this
//! test exercises the chokepoint shape (5 clients hitting `dispatch`
//! concurrently) without driving the original abort. Treat it as a smoke
//! check: it ensures the V8-lock plumbing compiles and isn't hitting an
//! obvious deadlock, not as a strict regression for the abort.
//!
//! For an end-to-end repro of the abort, the issue author used
//! `scrapegraphai.com` driven from Node CDP at concurrency 5. Reproducing
//! that here would require standing up a local server with JS subresources;
//! out of scope for this PR.
//!
//! Run with `cargo test -p obscura-cdp --test concurrent_navigations
//! -- --nocapture --ignored` (the `ignored` gate keeps it out of the default
//! suite — it boots a real CDP server, which is heavier than a unit test).

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

async fn one_client(port: u16, id_base: u64) -> Result<(), String> {
    let url = format!("ws://127.0.0.1:{}/devtools/browser", port);
    let (mut ws, _) = connect_async(&url).await.map_err(|e| e.to_string())?;

    // Target.createTarget — get a sessionId via Target.attachToTarget.
    let create = json!({
        "id": id_base,
        "method": "Target.createTarget",
        "params": {"url": "about:blank"},
    });
    ws.send(Message::Text(create.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;

    let mut session_id: Option<String> = None;
    let mut target_id: Option<String> = None;
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
        if let Some(t) = v.get("result").and_then(|r| r.get("targetId")).and_then(|s| s.as_str()) {
            target_id = Some(t.to_string());
        }
        if let Some(s) = v
            .get("params")
            .and_then(|p| p.get("sessionId"))
            .and_then(|s| s.as_str())
        {
            session_id = Some(s.to_string());
        }
    }

    let sid = session_id.unwrap();
    let _ = target_id; // kept for debugging — unused by the assertion path

    // Page.navigate to a data URL — exercises init_js + execute_scripts
    // without touching the network, which is what the V8 race needs.
    let nav = json!({
        "id": id_base + 1,
        "method": "Page.navigate",
        "sessionId": sid,
        "params": {"url": "data:text/html,<html><body><h1>x</h1></body></html>"},
    });
    ws.send(Message::Text(nav.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("timeout waiting for navigate response".to_string());
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
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if v.get("id").and_then(|x| x.as_u64()) == Some(id_base + 1) {
            return Ok(());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "boots a real CDP server; opt-in with --ignored"]
async fn concurrency_5_does_not_abort_v8() {
    let port = pick_port().await;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            tokio::task::spawn_local(async move {
                let _ = obscura_cdp::server::start(port).await;
            });
            // Give the listener a beat.
            tokio::time::sleep(Duration::from_millis(150)).await;

            let mut handles = Vec::new();
            for i in 0..5u64 {
                let id_base = (i + 1) * 1000;
                handles.push(tokio::task::spawn_local(async move {
                    one_client(port, id_base).await
                }));
            }

            let mut ok = 0usize;
            for (i, h) in handles.into_iter().enumerate() {
                match h.await {
                    Ok(Ok(())) => ok += 1,
                    Ok(Err(e)) => panic!("client {} failed: {}", i, e),
                    Err(e) => panic!("client {} join error: {}", i, e),
                }
            }
            assert_eq!(ok, 5, "all 5 concurrent clients must complete navigate");
        })
        .await;
}
