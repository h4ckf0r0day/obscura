//! HeapProfiler.takeHeapSnapshot streams a real V8 snapshot as chunk events.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session.map(str::to_string),
        },
        ctx,
    )
    .await;
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

#[tokio::test(flavor = "current_thread")]
async fn snapshot_is_streamed_as_chunks_and_parses() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let s = Some(session.as_str());
    // Many distinct non-ASCII strings so the snapshot exceeds one chunk and
    // chunk boundaries can fall inside multi-byte characters.
    cdp(
        &mut ctx,
        1,
        "Runtime.evaluate",
        json!({"expression": "globalThis.__keep = [new (class ObscuraMarker {})(), 'ünïcødé-✓'.repeat(200000), Array.from({length: 60000}, (_, i) => 'ünï✓-' + i)]"}),
        s,
    )
    .await;
    cdp(&mut ctx, 2, "HeapProfiler.enable", json!({}), s).await;
    ctx.pending_events.clear();
    cdp(&mut ctx, 3, "HeapProfiler.takeHeapSnapshot", json!({"reportProgress": false}), s).await;

    let chunks: Vec<_> = ctx
        .pending_events
        .iter()
        .filter(|e| e.method == "HeapProfiler.addHeapSnapshotChunk")
        .collect();
    assert!(chunks.len() >= 2, "expected at least 2 chunks, got {}", chunks.len());
    for e in &chunks {
        assert_eq!(e.session_id.as_deref(), Some(session.as_str()));
    }
    let joined: String = chunks
        .iter()
        .map(|e| e.params["chunk"].as_str().unwrap())
        .collect();
    let parsed: Value = serde_json::from_str(&joined).unwrap();
    assert!(parsed.get("snapshot").is_some() && parsed.get("nodes").is_some());
    assert!(joined.contains("ObscuraMarker"));
}

#[tokio::test(flavor = "current_thread")]
async fn snapshot_without_a_page_is_an_error() {
    let mut ctx = CdpContext::new();
    let resp = dispatch(
        &CdpRequest {
            id: 1,
            method: "HeapProfiler.takeHeapSnapshot".to_string(),
            params: json!({}),
            session_id: None,
        },
        &mut ctx,
    )
    .await;
    assert!(resp.error.is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn collect_garbage_and_enable_still_succeed() {
    let mut ctx = CdpContext::new();
    let (_, session) = created_and_attached(&mut ctx).await;
    let s = Some(session.as_str());
    for (id, m) in [(1, "enable"), (2, "collectGarbage"), (3, "disable")] {
        let r = cdp(&mut ctx, id, &format!("HeapProfiler.{m}"), json!({}), s).await;
        assert_eq!(r, json!({}));
    }
}
