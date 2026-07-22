//! Assigning `scrollTop` / `scrollLeft` must fire a `scroll` event.
//!
//! `scrollTo`/`scrollBy` already dispatched one; direct assignment did not, so
//! the very common lazy-load idiom
//!
//! ```js
//! el.addEventListener('scroll', loadMore);
//! el.scrollTop = el.scrollHeight;      // never reached loadMore
//! ```
//!
//! silently did nothing and scroll-driven feeds stalled after their first batch.
//!
//! Checked against a real Chrome over CDP with this same probe: assigning
//! `scrollTop` fires exactly one `scroll` event there. These tests assert the
//! same observable behaviour, not the synthetic geometry around it — the offset
//! is intentionally not clamped, since without layout any maximum is a guess.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_once() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = "<html><body><div id=\"box\"><p>a</p><p>b</p></div></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session_id: &str) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

/// `awaitPromise` matters here: the scroll event is dispatched from a
/// `setTimeout(..., 0)`, so the probe has to yield before reading the counter.
async fn eval(ctx: &mut CdpContext, id: u64, expr: &str, session_id: &str) -> Value {
    cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
        session_id,
    )
    .await
}

async fn setup() -> (CdpContext, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_once().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), session_id).await;
    (ctx, session_id.to_string())
}

async fn probe(ctx: &mut CdpContext, sid: &str, body: &str) -> Value {
    let expr = format!(
        r#"(() => {{
            const el = document.getElementById('box');
            let fired = 0;
            el.addEventListener('scroll', () => {{ fired++; }});
            {body}
            return new Promise(r => setTimeout(() => r(JSON.stringify({{
                fired, top: el.scrollTop, left: el.scrollLeft,
            }})), 0));
        }})()"#
    );
    let v = eval(ctx, 2, &expr, sid).await;
    serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn assigning_scroll_top_fires_one_scroll_event() {
    let (mut ctx, sid) = setup().await;
    let r = probe(&mut ctx, &sid, "el.scrollTop = 100;").await;
    assert_eq!(r["fired"], 1, "scrollTop assignment must fire exactly one scroll event");
    assert_eq!(r["top"], 100, "scrollTop must round-trip the assigned value");
}

#[tokio::test(flavor = "current_thread")]
async fn assigning_scroll_left_fires_one_scroll_event() {
    let (mut ctx, sid) = setup().await;
    let r = probe(&mut ctx, &sid, "el.scrollLeft = 40;").await;
    assert_eq!(r["fired"], 1, "scrollLeft assignment must fire exactly one scroll event");
    assert_eq!(r["left"], 40, "scrollLeft must round-trip the assigned value");
}

/// Re-assigning the same offset is not a scroll, so it must stay silent —
/// otherwise a loader that writes `scrollTop` on every frame re-enters forever.
#[tokio::test(flavor = "current_thread")]
async fn reassigning_the_same_offset_is_silent() {
    let (mut ctx, sid) = setup().await;
    let r = probe(&mut ctx, &sid, "el.scrollTop = 100; el.scrollTop = 100;").await;
    assert_eq!(r["fired"], 1, "only the offset change fires; the repeat must not");
}

/// `scrollTo` moves both axes, and a real browser reports one scroll per
/// movement rather than one per axis. The setters suppress their own event
/// inside `scrollTo`/`scrollBy` so the operation stays a single event.
#[tokio::test(flavor = "current_thread")]
async fn scroll_to_coalesces_both_axes_into_one_event() {
    let (mut ctx, sid) = setup().await;
    let r = probe(&mut ctx, &sid, "el.scrollTo(30, 60);").await;
    assert_eq!(r["fired"], 1, "scrollTo must fire one event, not one per axis");
    assert_eq!(r["top"], 60);
    assert_eq!(r["left"], 30);
}

/// The lazy-load idiom the fix exists for: a listener that appends more rows
/// when the feed is scrolled. Before the fix this counted 0 and feeds froze.
#[tokio::test(flavor = "current_thread")]
async fn scroll_driven_lazy_loader_advances() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(() => {
            const el = document.getElementById('box');
            let batches = 0;
            el.addEventListener('scroll', () => {
                if (batches >= 3) return;
                batches++;
                for (let i = 0; i < 5; i++) el.appendChild(document.createElement('p'));
            });
            el.scrollTop = 500;
            return new Promise(r => setTimeout(() => {
                el.scrollTop = 1000;
                setTimeout(() => r(JSON.stringify({
                    batches, rows: el.querySelectorAll('p').length,
                })), 0);
            }, 0));
        })()"#,
        &sid,
    )
    .await;
    let r = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(r["batches"], 2, "each forward scroll must reach the loader");
    assert_eq!(r["rows"], 12, "2 starting rows + 2 batches of 5");
}
