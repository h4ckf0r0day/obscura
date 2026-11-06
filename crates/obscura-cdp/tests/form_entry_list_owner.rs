//! Form submission builds its entry list from the controls the form owns:
//! descendants that do not name another form with `form=""`, plus controls
//! elsewhere whose `form=""` names it. A `multiple` select contributes every
//! selected, enabled option, selectedness is the live state (not the
//! `selected` attribute), and a `<button>` submitter adds its name and value.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn page_for(path: &str) -> &'static str {
    match path {
        "/form-attribute" => r#"<html><body>
<form id="f" action="/submitted">
  <input name="inside" value="1">
  <input name="other" value="x" form="g">
</form>
<form id="g" action="/elsewhere"></form>
<input name="outside" value="2" form="f">
<input name="unowned" value="3">
</body></html>"#,
        "/multiple" => r#"<html><body>
<form id="f" action="/submitted">
  <select name="m" multiple>
    <option value="a" selected>A</option>
    <option value="b" selected>B</option>
    <option value="c">C</option>
    <option value="d" selected disabled>D</option>
  </select>
</form>
</body></html>"#,
        "/button" => r#"<html><body>
<form id="f" action="/submitted">
  <input name="a" value="1">
  <button id="go" name="go" value="yes">Go</button>
  <button name="other" value="no">Other</button>
</form>
</body></html>"#,
        "/selected-index" => r#"<html><body>
<form id="f" action="/submitted">
  <select name="s"><option value="a" selected>A</option><option value="b">B</option></select>
  <select name="first"><option value="x">X</option><option value="y">Y</option></select>
</form>
</body></html>"#,
        _ => "<html><body>submitted</body></html>",
    }
}

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_string();
                let body = page_for(&path);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
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

/// Load `path`, run `script` (which submits the form), and return the query
/// string of the page the submission navigated to.
async fn submitted_query(path: &str, script: &str) -> Option<String> {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = serve().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": format!("{base}{path}"), "waitUntil": "load"}), session_id).await;
    cdp(&mut ctx, 2, "Runtime.evaluate", json!({"expression": script}), session_id).await;
    let page = ctx.get_page_mut(&page_id).unwrap();
    let url = page.url.as_ref().unwrap();
    assert_eq!(url.path(), "/submitted", "the form should have navigated to its action");
    url.query().map(str::to_string)
}

async fn evaluate(path: &str, expression: &str) -> Value {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = serve().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": format!("{base}{path}"), "waitUntil": "load"}), session_id).await;
    let result = cdp(&mut ctx, 2, "Runtime.evaluate", json!({"expression": expression, "returnByValue": true}), session_id).await;
    result["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn form_attribute_decides_which_form_owns_a_control() {
    let query = submitted_query("/form-attribute", "document.getElementById('f').submit()").await;
    assert_eq!(query.as_deref(), Some("inside=1&outside=2"));
}

#[tokio::test(flavor = "current_thread")]
async fn form_property_and_elements_follow_the_form_attribute() {
    let value = evaluate(
        "/form-attribute",
        "(() => { const f = document.getElementById('f');
           const named = (n) => document.querySelector('[name=' + n + ']');
           return [named('inside').form === f, named('other').form && named('other').form.id,
                   named('outside').form === f, named('unowned').form,
                   Array.from(f.elements).map((e) => e.name).join(',')].join('|'); })()",
    )
    .await;
    assert_eq!(value, json!("true|g|true||inside,outside"));
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_select_submits_every_selected_enabled_option() {
    let query = submitted_query("/multiple", "document.getElementById('f').submit()").await;
    assert_eq!(query.as_deref(), Some("m=a&m=b"));
}

#[tokio::test(flavor = "current_thread")]
async fn button_submitter_adds_its_name_and_value() {
    let query = submitted_query(
        "/button",
        "document.getElementById('f').requestSubmit(document.getElementById('go'))",
    )
    .await;
    assert_eq!(query.as_deref(), Some("a=1&go=yes"));
}

#[tokio::test(flavor = "current_thread")]
async fn submission_uses_live_selectedness_not_the_selected_attribute() {
    // selectedIndex updates the options' selectedness, not their attributes;
    // a single select with nothing chosen submits its first option.
    let query = submitted_query(
        "/selected-index",
        "(() => { const f = document.getElementById('f'); f.querySelector('[name=s]').selectedIndex = 1; f.submit(); })()",
    )
    .await;
    assert_eq!(query.as_deref(), Some("s=b&first=x"));
}
