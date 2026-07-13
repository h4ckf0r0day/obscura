// Regression parity for issue #389: Cloudflare managed challenges hang because
// bootstrap.js stubbed two structured-clone primitives the turnstile
// orchestrate VM depends on. Each case below fails on main and must pass after
// the fix:
//
//   1. `structuredClone` must preserve ArrayBuffer / TypedArray bytes (the
//      JSON fallback on line 5123 serializes them to `{}`).
//   2. A `CryptoKey` must survive `structuredClone` and remain usable by
//      `crypto.subtle` (the WeakMap on line 6898 is keyed by object identity,
//      so a clone has no key material and throws "not a valid CryptoKey").
//
// These mirror the cdp_click_submit_parity helpers (`serve_once` / `cdp`),
// copied per the Testing-and-debugging.md guidance to reuse the pattern.

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
            let body = "<html><body><script>window.__boot = true;</script></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });
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
    assert!(
        resp.error.is_none(),
        "CDP {method} failed: {:?}",
        resp.error
    );
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

#[tokio::test(flavor = "current_thread")]
async fn structured_clone_preserves_arraybuffer_bytes() {
    let (mut ctx, sid) = setup().await;
    // A 4-byte view into a 4-byte buffer. The JSON fallback loses the buffer
    // entirely (Uint8Array serializes to {}), so byteLength reads back as 0.
    let v = eval(
        &mut ctx,
        2,
        r#"(async () => {
            const src = new Uint8Array([10, 20, 30, 40]);
            const clone = structuredClone(src);
            return JSON.stringify({
                srcLen: src.byteLength,
                cloneLen: clone.byteLength,
                same: src.buffer === clone.buffer,
                bytes: Array.from(clone),
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["srcLen"], 4);
    assert_eq!(val["cloneLen"], 4, "structuredClone dropped the ArrayBuffer");
    assert_eq!(val["same"], false, "clone must be independent, not the same buffer");
    assert_eq!(val["bytes"], json!([10, 20, 30, 40]));
}

#[tokio::test(flavor = "current_thread")]
async fn cryptokey_survives_structured_clone_and_still_signs() {
    let (mut ctx, sid) = setup().await;
    // importKey -> structuredClone -> sign with the clone. On main the clone
    // has no WeakMap entry, so sign throws "Argument is not a valid CryptoKey".
    let v = eval(
        &mut ctx,
        2,
        r#"(async () => {
            const key = await crypto.subtle.importKey(
                "raw", new Uint8Array(32),
                { name: "HMAC", hash: "SHA-256" }, true, ["sign"]
            );
            const clone = structuredClone(key);
            const sig = await crypto.subtle.sign("HMAC", clone, new TextEncoder().encode("abc"));
            const b = new Uint8Array(sig);
            return JSON.stringify({
                cloneType: clone.type,
                cloneTag: clone[Symbol.toStringTag],
                sigLen: b.length,
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["cloneType"], "secret");
    assert_eq!(val["cloneTag"], "CryptoKey");
    assert_eq!(val["sigLen"], 32, "cloned CryptoKey must remain usable by crypto.subtle");
}

// DataView has no .slice() method, so the original TypedArray branch
// (`new Ctor(value.slice())`) threw `TypeError: value.slice is not a function`
// on every DataView clone. That is a new crash in the very buffers category
// this feature targets, so it must clone cleanly.
#[tokio::test(flavor = "current_thread")]
async fn structured_clone_preserves_dataview() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(async () => {
            const buf = new ArrayBuffer(8);
            const view = new DataView(buf);
            view.setUint32(0, 0x12345678);
            view.setUint32(4, 0x9abcdef0);
            const clone = structuredClone(view);
            return JSON.stringify({
                len: clone.byteLength,
                a: clone.getUint32(0),
                b: clone.getUint32(4),
                independent: clone.buffer !== view.buffer,
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["len"], 8, "DataView clone must keep its length");
    assert_eq!(val["a"], 0x12345678);
    assert_eq!(val["b"], 0x9abcdef0u64 as i64);
    assert_eq!(val["independent"], true, "clone must own its buffer, not alias the source");
}

// Functions and symbols are not structured-cloneable. The original early
// `typeof !== "object"` return passed them through by reference instead of
// throwing DataCloneError, so this guards both the throw and the name.
#[tokio::test(flavor = "current_thread")]
async fn structured_clone_rejects_functions_and_symbols() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(async () => {
            const out = {};
            try { structuredClone(function f(){}); out.fn = "cloned"; }
            catch (e) { out.fn = e instanceof DOMException ? e.name : "TypeError:" + e.message; }
            try { structuredClone(Symbol("s")); out.sym = "cloned"; }
            catch (e) { out.sym = e instanceof DOMException ? e.name : "TypeError:" + e.message; }
            return JSON.stringify(out);
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["fn"], "DataCloneError", "functions must not clone");
    assert_eq!(val["sym"], "DataCloneError", "symbols must not clone");
}
