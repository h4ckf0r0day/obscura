//! A non-viewport element must report a scroll box sized from its content, and
//! must report the same box whatever viewport the stealth layer drew.
//!
//! Scroll-driven virtualized feeds (Google Maps search results, infinite lists)
//! page by "the user moved one viewport further, load the next batch". With the
//! 20px stub box every element used to report, `el.scrollTop = el.scrollHeight`
//! lands on 20 every time: the first assignment is progress, the rest are
//! no-ops, and the feed freezes after its first batch no matter how many scroll
//! passes the driver makes.
//!
//! This is a *reporting* fix: `scrollTop` stays unclamped (see
//! `scroll_event_on_assignment.rs`), because clamping to a synthetic maximum
//! deadlocks a loader that has not been given content to scroll over yet. And
//! the box is built from constants rather than `innerHeight` — the stealth layer
//! randomises the viewport per session, so a viewport-derived box paginates or
//! stalls depending on the draw.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Rows the fixture feed holds in total; the loader hands them out in batches.
const TOTAL_ROWS: usize = 120;

async fn serve_once() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = "<html><body><div id=\"feed\"><p>a</p><p>b</p></div></body></html>";
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

/// The probes below settle their scroll events through `setTimeout`, so every
/// evaluation is awaited.
async fn eval(ctx: &mut CdpContext, id: u64, expr: &str, session_id: &str) -> Value {
    let v = cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": expr, "returnByValue": true, "awaitPromise": true}),
        session_id,
    )
    .await;
    let raw = v["result"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("probe returned no value: {v}"));
    serde_json::from_str::<Value>(raw).unwrap()
}

async fn setup() -> (CdpContext, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_once().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());
    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": url, "waitUntil": "load"}),
        session_id,
    )
    .await;
    (ctx, session_id.to_string())
}

/// A container with many descendants must report a scroll range, not a 20px
/// stub: without a range there is nothing for a lazy loader to scroll through.
#[tokio::test(flavor = "current_thread")]
async fn scroll_box_is_sized_from_the_subtree() {
    let (mut ctx, sid) = setup().await;
    let r = eval(
        &mut ctx,
        2,
        r#"(() => {
            const el = document.getElementById('feed');
            const empty = el.scrollHeight;
            for (let i = 0; i < 100; i++) el.appendChild(document.createElement('p'));
            return Promise.resolve(JSON.stringify({
                empty,
                clientHeight: el.clientHeight,
                filled: el.scrollHeight,
            }));
        })()"#,
        &sid,
    )
    .await;
    assert_eq!(
        r["clientHeight"], 800,
        "the client box must be the synthetic viewport constant"
    );
    assert_eq!(
        r["empty"], 800,
        "an element with no content still reports at least one client box"
    );
    assert!(
        r["filled"].as_i64().unwrap() > r["empty"].as_i64().unwrap(),
        "adding 100 rows must grow the scroll box (got {})",
        r["filled"]
    );
}

/// The stealth layer randomises `innerHeight` per session. A scroll box that
/// tracked it paginated or stalled depending on the draw, so the box must be
/// invariant under it.
#[tokio::test(flavor = "current_thread")]
async fn scroll_box_does_not_track_the_randomised_viewport() {
    let (mut ctx, sid) = setup().await;
    let r = eval(
        &mut ctx,
        2,
        r#"(() => {
            const el = document.getElementById('feed');
            for (let i = 0; i < 100; i++) el.appendChild(document.createElement('p'));
            const read = () => ({ ch: el.clientHeight, sh: el.scrollHeight });
            globalThis.innerHeight = 640;
            const small = read();
            globalThis.innerHeight = 970;
            const large = read();
            return Promise.resolve(JSON.stringify({ small, large }));
        })()"#,
        &sid,
    )
    .await;
    assert_eq!(
        r["small"], r["large"],
        "the scroll box must be identical across viewport draws"
    );
}

/// The behaviour the geometry exists for. The fixture loader is the real
/// virtualized-feed contract: hand out the next batch once the driver has
/// scrolled a further client box down. With the stub box every pass landed on
/// the same offset, so this stopped at the first batch.
#[tokio::test(flavor = "current_thread")]
async fn virtualized_feed_pages_to_its_full_result_set() {
    let (mut ctx, sid) = setup().await;
    let r = eval(
        &mut ctx,
        2,
        &format!(
            r#"(async () => {{
            const el = document.getElementById('feed');
            const TOTAL = {TOTAL_ROWS};
            let served = 0;
            // A paginating feed hands out its next batch when it is scrolled.
            // Nothing here reads the geometry: what the geometry decides is
            // whether `scrollTop = scrollHeight` still MOVES on the next pass,
            // and a scroll that does not move is not a scroll event. With a
            // fixed-size box every pass after the first lands on the same
            // offset, so this listener is never reached again.
            el.addEventListener('scroll', () => {{
                for (let i = 0; i < 30 && served < TOTAL; i++, served++) {{
                    const row = document.createElement('p');
                    row.className = 'row';
                    el.appendChild(row);
                }}
            }});
            const sleep = ms => new Promise(r => setTimeout(r, ms));
            for (let pass = 0; pass < 40; pass++) {{
                el.scrollTop = el.scrollHeight;
                // The scroll event is dispatched from a timer, so yield to let
                // the batch it triggers land before measuring the next pass.
                await sleep(0);
            }}
            return JSON.stringify({{ rows: el.querySelectorAll('p.row').length }});
        }})()"#
        ),
        &sid,
    )
    .await;
    assert_eq!(
        r["rows"], TOTAL_ROWS as i64,
        "the feed must page in every row, not plateau on its first batch"
    );
}
