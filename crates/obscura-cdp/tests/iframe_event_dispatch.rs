// Regression tests for iframe event dispatch. `_IframeDocument`'s
// addEventListener/removeEventListener/dispatchEvent were no-ops, so listeners
// registered on an iframe document never ran. Separately, iframe load invoked
// the `onload` property directly instead of dispatching through the element,
// which skipped any addEventListener('load', ...) listener. Both fail on main,
// pass after the fix.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Serve every request (not just the first): Page.navigate consumes one, and the
// iframe load test needs its own successful response so it exercises the real
// load path rather than the fetch-failure fallback.
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let body = "<html><body><div id=a></div></body></html>";
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

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: &str,
) -> Value {
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
    let url = serve().await;
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

#[tokio::test(flavor = "current_thread")]
async fn iframe_document_dispatches_registered_listeners() {
    let (mut ctx, sid) = setup().await;
    // Accessing contentDocument on a src-less iframe lazily creates an
    // about:blank _IframeDocument synchronously, so this exercises the event
    // target without waiting on an async load.
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            const iframe = document.createElement('iframe');
            document.body.appendChild(iframe);
            const doc = iframe.contentDocument;
            const out = { hasDoc: !!doc };
            let calls = 0;
            const listener = () => { calls++; };
            // A registered listener runs on dispatch.
            doc.addEventListener('probe', listener);
            doc.dispatchEvent(new Event('probe'));
            out.afterRegister = calls;
            // Registering the same listener again is a no-op (deduped).
            doc.addEventListener('probe', listener);
            doc.addEventListener('probe', listener);
            doc.dispatchEvent(new Event('probe'));
            out.afterDuplicate = calls;
            // After removal the listener no longer runs.
            doc.removeEventListener('probe', listener);
            doc.dispatchEvent(new Event('probe'));
            out.afterRemove = calls;
            // dispatchEvent reports cancellation via its return value.
            doc.addEventListener('cancelme', (e) => e.preventDefault());
            out.cancelReturn = doc.dispatchEvent(new Event('cancelme', { cancelable: true }));
            out.plainReturn = doc.dispatchEvent(new Event('nolisteners'));
            return JSON.stringify(out);
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["hasDoc"], true);
    assert_eq!(val["afterRegister"].as_u64(), Some(1), "listener runs once on dispatch");
    assert_eq!(val["afterDuplicate"].as_u64(), Some(2), "duplicate registration deduped");
    assert_eq!(val["afterRemove"].as_u64(), Some(2), "removed listener does not run");
    assert_eq!(val["cancelReturn"], false, "preventDefault -> dispatchEvent returns false");
    assert_eq!(val["plainReturn"], true, "no cancellation -> dispatchEvent returns true");
}

#[tokio::test(flavor = "current_thread")]
async fn iframe_load_reaches_onload_and_addeventlistener() {
    let (mut ctx, sid) = setup().await;
    // Both the onload property and an addEventListener('load', ...) listener
    // must run exactly once. On main, setting onload made the direct-call path
    // skip the addEventListener listener entirely.
    let v = eval(
        &mut ctx,
        2,
        r#"(function () {
            return new Promise((resolve) => {
                const iframe = document.createElement('iframe');
                const events = [];
                // Resolve from the onload property (which fires on both old and
                // new code) one microtask later, so the synchronous
                // addEventListener('load') handler has already run on the fixed
                // build. On main the listener never fires, so this reports
                // ["property"] (a clean assertion failure) instead of hanging.
                iframe.onload = () => {
                    events.push('property');
                    Promise.resolve().then(() => resolve(JSON.stringify({ events })));
                };
                iframe.addEventListener('load', () => events.push('listener'));
                document.body.appendChild(iframe);
                iframe.src = location.href;
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        val["events"],
        json!(["property", "listener"]),
        "both onload property and addEventListener('load') run exactly once, in order"
    );
}
