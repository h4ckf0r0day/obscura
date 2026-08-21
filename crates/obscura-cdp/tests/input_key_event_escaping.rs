//! `Input.dispatchKeyEvent` interpolates the `key`/`code` params into a
//! generated `KeyboardEvent(...)` snippet. They must be escaped for BOTH
//! backslash and single-quote (issue #433): Chrome sends `key: "\\"` (U+005C)
//! when the backslash key is pressed, and quote-only escaping turns that into
//! `key:'\'` — the backslash escapes the closing quote, the literal runs on,
//! and the whole `page.evaluate` is a syntax error, so the `keydown` is
//! silently never dispatched. Regression test: the backslash key must arrive.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Serves a page that records the `key` of the last keydown event on the body.
async fn serve_page() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = r#"<html><body>
<input id="i">
<textarea id="a"></textarea>
<script>
window.__keys = [];
document.body.addEventListener('keydown', function (e) { window.__keys.push(e.key + '|' + e.code); });
</script>
</body></html>"#;
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

#[tokio::test(flavor = "current_thread")]
async fn dispatch_key_event_escapes_backslash_in_key_and_code() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_page().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), session_id).await;

    // The backslash key: Chrome sends key="\" (a single backslash) code="Backslash".
    cdp(
        &mut ctx,
        2,
        "Input.dispatchKeyEvent",
        json!({"type": "keyDown", "key": "\\", "code": "Backslash"}),
        session_id,
    )
    .await;

    // A key whose name itself contains a quote AND a backslash, to exercise the
    // ordering of the two replacements.
    cdp(
        &mut ctx,
        3,
        "Input.dispatchKeyEvent",
        json!({"type": "keyDown", "key": "a", "code": "KeyA"}),
        session_id,
    )
    .await;

    let v = cdp(
        &mut ctx,
        4,
        "Runtime.evaluate",
        json!({"expression": "JSON.stringify(window.__keys)", "returnByValue": true}),
        session_id,
    )
    .await;

    let keys: Vec<String> =
        serde_json::from_str(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        keys,
        vec!["\\|Backslash".to_string(), "a|KeyA".to_string()],
        "the backslash key must be dispatched, not dropped by a malformed snippet"
    );
}

// The same hazard one layer over: on the inserted *text* rather than on the key
// name. `insert_text_js` interpolates the text into the same kind of
// single-quoted literal. #433 escaped the backslash and the quote and stopped
// there, but a raw newline ends a JS string literal just as a stray quote does,
// so the snippet is a syntax error and the character is silently dropped.
//
// The `char` type is the path that reaches it with a newline: the `keyDown`
// branch skips insertion for "\r" and "\n" because `key == "Enter"` handles
// those, and `char` has no such guard, so a client entering a line break in a
// textarea this way loses it with no error reported anywhere.
#[tokio::test(flavor = "current_thread")]
async fn dispatch_key_event_char_carries_a_newline_into_a_textarea() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_page().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), session_id).await;
    cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": "(function () { document.getElementById('a').focus(); return 'ok'; })()",
            "returnByValue": true,
        }),
        session_id,
    )
    .await;

    for (id, ch) in [(3u64, "a"), (4, "\n"), (5, "b")] {
        cdp(
            &mut ctx,
            id,
            "Input.dispatchKeyEvent",
            json!({"type": "char", "text": ch}),
            session_id,
        )
        .await;
    }

    let v = cdp(
        &mut ctx,
        6,
        "Runtime.evaluate",
        json!({
            "expression": "JSON.stringify(document.getElementById('a').value)",
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    assert_eq!(
        v["result"]["value"].as_str().unwrap_or_default(),
        r#""a\nb""#,
        "a newline sent as a char must reach the field, not be dropped by a malformed snippet"
    );
}
