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

// A reference cycle through Error.cause must clone without crashing (issue
// #419). The Error branch recursed into `cause` before recording itself in
// `seen`, so a self-referential cause blew the stack. Chrome clones this and
// preserves identity (clone.cause === clone).
#[tokio::test(flavor = "current_thread")]
async fn structured_clone_handles_circular_error_cause() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(async () => {
            const e = new Error("boom");
            e.cause = e;
            const out = {};
            try {
                const clone = structuredClone(e);
                out.ok = true;
                out.message = clone.message;
                out.selfCycle = clone.cause === clone;
                out.isError = clone instanceof Error;
            } catch (err) {
                out.ok = false;
                out.err = String(err && err.message || err);
            }
            return JSON.stringify(out);
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["ok"], true, "circular Error.cause crashed structuredClone: {:?}", val["err"]);
    assert_eq!(val["message"], "boom");
    assert_eq!(val["isError"], true, "clone must remain an Error");
    assert_eq!(val["selfCycle"], true, "cyclic cause must resolve to the clone, not a duplicate");
}

// An own enumerable `__proto__` data property (what JSON.parse('{"__proto__":…}')
// produces) must clone as an own data property, not be routed through the
// inherited __proto__ setter (issue #420). Plain objects must also clone onto
// Object.prototype, matching Chrome, rather than inheriting the source proto.
#[tokio::test(flavor = "current_thread")]
async fn structured_clone_reproduces_own_proto_property() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(async () => {
            const src = JSON.parse('{"__proto__":{"polluted":true},"a":1}');
            const clone = structuredClone(src);
            return JSON.stringify({
                hasOwnProto: Object.prototype.hasOwnProperty.call(clone, "__proto__"),
                plainProto: Object.getPrototypeOf(clone) === Object.prototype,
                polluted: clone.polluted === true,
                a: clone.a,
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["hasOwnProto"], true, "own __proto__ data property was lost");
    assert_eq!(val["plainProto"], true, "plain object must clone onto Object.prototype");
    assert_eq!(val["polluted"], false, "clone prototype was reparented via the __proto__ setter");
    assert_eq!(val["a"], 1);
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

// structuredClone preserves reference identity within one graph, including for
// platform objects cloned through a hook (issue #423). The CryptoKey hook
// ignored the `seen` map _structuredClone hands it, so one key referenced twice
// came back as two distinct objects.
#[tokio::test(flavor = "current_thread")]
async fn structured_clone_preserves_cryptokey_identity() {
    let (mut ctx, sid) = setup().await;
    let v = eval(
        &mut ctx,
        2,
        r#"(async () => {
            const key = await crypto.subtle.importKey(
                "raw", new Uint8Array(32),
                { name: "HMAC", hash: "SHA-256" }, true, ["sign"]
            );
            const clone = structuredClone({ a: key, b: key });
            // The shared clone must still be usable by crypto.subtle.
            const sig = await crypto.subtle.sign("HMAC", clone.a, new TextEncoder().encode("abc"));
            return JSON.stringify({
                shared: clone.a === clone.b,
                distinctFromSource: clone.a !== key,
                tag: clone.a[Symbol.toStringTag],
                sigLen: new Uint8Array(sig).length,
            });
        })()"#,
        &sid,
    )
    .await;
    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["shared"], true, "one CryptoKey reached twice must clone to one shared object");
    assert_eq!(val["distinctFromSource"], true, "clone must not alias the source key");
    assert_eq!(val["tag"], "CryptoKey");
    assert_eq!(val["sigLen"], 32, "the shared clone must remain usable by crypto.subtle");
}
