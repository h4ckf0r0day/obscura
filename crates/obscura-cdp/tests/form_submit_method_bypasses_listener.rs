//! `HTMLFormElement.submit()` (the method) must submit WITHOUT firing a
//! cancelable `submit` event, so a page's `submit` listener that calls
//! `preventDefault()` cannot veto it. Only `requestSubmit()` and user-initiated
//! submits fire the cancelable event. Regression test for the invisible-reCAPTCHA
//! login pattern (listener preventDefaults; a success callback calls
//! `form.submit()` to actually send the form).

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// Serves a form whose `submit` listener always calls preventDefault().
async fn serve_form() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = socket.read(&mut buf).await.unwrap();
                let req = String::from_utf8_lossy(&buf[..n]);
                let (status, body) = if req.starts_with("GET /submitted") {
                    ("200 OK", "<html><body>submitted</body></html>")
                } else {
                    (
                        "200 OK",
                        r#"<html><body>
<form id="f" action="/submitted">
  <input type="hidden" name="q" value="1">
  <button id="b" type="submit">Go</button>
</form>
<script>
document.getElementById('f').addEventListener('submit', function(e) { e.preventDefault(); });
</script>
</body></html>"#,
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(resp.as_bytes()).await.unwrap();
            });
        }
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

async fn navigate(ctx: &mut CdpContext, url: &str, session_id: &str) {
    cdp(ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), session_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn submit_method_navigates_despite_prevent_default_listener() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_form().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    navigate(&mut ctx, &url, session_id).await;

    // The submit() METHOD must not fire the cancelable submit event, so the
    // page's preventDefault() listener cannot stop it: navigation proceeds.
    cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": "document.getElementById('f').submit()"}),
        session_id,
    )
    .await;

    let page = ctx.get_page_mut(&page_id).unwrap();
    assert_eq!(
        page.url.as_ref().unwrap().path(),
        "/submitted",
        "form.submit() should navigate even though a submit listener preventDefault()s"
    );
    assert_eq!(page.url.as_ref().unwrap().query(), Some("q=1"));
}

#[tokio::test(flavor = "current_thread")]
async fn request_submit_is_vetoed_by_prevent_default_listener() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_form().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-2";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    navigate(&mut ctx, &url, session_id).await;

    // requestSubmit() DOES fire the cancelable submit event, so the listener's
    // preventDefault() cancels it: the page must NOT navigate.
    let has = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": "typeof document.getElementById('f').requestSubmit", "returnByValue": true}),
        session_id,
    )
    .await;
    assert_eq!(has["result"]["value"], "function", "requestSubmit must exist");

    cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({"expression": "document.getElementById('f').requestSubmit()"}),
        session_id,
    )
    .await;

    let page = ctx.get_page_mut(&page_id).unwrap();
    assert_ne!(
        page.url.as_ref().unwrap().path(),
        "/submitted",
        "requestSubmit() must be cancelable by a preventDefault() submit listener"
    );
}

// A synthetic CDP click on a submit button is a user-initiated submit, so the
// cancelable `submit` event must fire and a preventDefault() listener must be
// able to veto navigation. Before the input.rs fix this path called
// `form.submit()` directly, bypassing the listener. Regression test for the
// CDP automation surface (Puppeteer/Playwright elementHandle.click()).
#[tokio::test(flavor = "current_thread")]
async fn cdp_click_submit_button_is_vetoed_by_prevent_default_listener() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_form().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-3";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    navigate(&mut ctx, &url, session_id).await;

    // Point the CDP click resolver at the submit button explicitly so the test
    // does not depend on layout coordinates.
    cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": "globalThis.__obscura_click_target = document.getElementById('b')"}),
        session_id,
    )
    .await;

    cdp(
        &mut ctx,
        3,
        "Input.dispatchMouseEvent",
        json!({"type": "mousePressed", "x": 0.0, "y": 0.0, "button": "left", "clickCount": 1}),
        session_id,
    )
    .await;

    let page = ctx.get_page_mut(&page_id).unwrap();
    assert_ne!(
        page.url.as_ref().unwrap().path(),
        "/submitted",
        "a CDP click on a submit button must fire the cancelable submit event and be vetoable"
    );
}

// requestSubmit(submitter) must validate its argument before doing anything
// else (issue #424): a TypeError if the submitter is not a submit button, and a
// NotFoundError DOMException if it is not owned by the form. obscura accepted
// anything and silently submitted. The form under test preventDefault()s its
// own submit event so the valid-submitter case cannot navigate away.
#[tokio::test(flavor = "current_thread")]
async fn request_submit_validates_its_submitter_argument() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_form().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-4";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    navigate(&mut ctx, &url, session_id).await;

    let v = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": r#"(() => {
                document.body.innerHTML =
                  '<form id="vf">' +
                    '<div id="d"></div>' +
                    '<button id="ok" type="submit">go</button>' +
                    '<button id="plain" type="button">x</button>' +
                    '<input id="inp" type="text">' +
                  '</form>' +
                  '<button id="outside" type="submit">y</button>';
                const f = document.getElementById('vf');
                f.addEventListener('submit', (e) => e.preventDefault());
                const probe = (fn) => {
                    try { fn(); return "no-throw"; }
                    catch (e) {
                        return (e instanceof DOMException) ? "DOMException:" + e.name
                                                           : (e && e.constructor && e.constructor.name) || String(e);
                    }
                };
                return JSON.stringify({
                    div:      probe(() => f.requestSubmit(document.getElementById('d'))),
                    plain:    probe(() => f.requestSubmit(document.getElementById('plain'))),
                    text:     probe(() => f.requestSubmit(document.getElementById('inp'))),
                    outside:  probe(() => f.requestSubmit(document.getElementById('outside'))),
                    valid:    probe(() => f.requestSubmit(document.getElementById('ok'))),
                    noArg:    probe(() => f.requestSubmit()),
                    nullArg:  probe(() => f.requestSubmit(null)),
                });
            })()"#,
            "returnByValue": true
        }),
        session_id,
    )
    .await;

    let val = serde_json::from_str::<Value>(v["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(val["div"], "TypeError", "a non-button submitter must throw TypeError");
    assert_eq!(val["plain"], "TypeError", "type=button is not a submit button");
    assert_eq!(val["text"], "TypeError", "input type=text is not a submit button");
    assert_eq!(
        val["outside"], "DOMException:NotFoundError",
        "a submit button not owned by the form must throw NotFoundError"
    );
    assert_eq!(val["valid"], "no-throw", "the form's own submit button must be accepted");
    assert_eq!(val["noArg"], "no-throw", "requestSubmit() with no submitter is valid");
    assert_eq!(val["nullArg"], "no-throw", "requestSubmit(null) means submit from the form itself");
}
