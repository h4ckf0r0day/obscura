//! Controls the parser associates with a form through its form element
//! pointer, though they are not inside it: `<table><form><tr><td><input>`
//! closes the `<form>` element at once, but the inputs that follow still
//! belong to it (HTML "insert a foreign/HTML element", step "associate").

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn page_for(path: &str) -> &'static str {
    match path {
        "/misnested" => r#"<html><body>
<table><form action="/submitted"><tr><td><input name="a" value="1"></td><td><select name="s"><option>x</option><option selected>y</option></select></td></tr></form></table>
<input name="outside" value="2">
</body></html>"#,
        "/misnested-id" => r#"<html><body>
<table><form id="f" action="/submitted"><tr><td><input id="a" name="a" value="1"></td></tr></form></table>
</body></html>"#,
        "/misnested-button" => r#"<html><body>
<table><form id="f" action="/submitted"><tr><td><input name="a" value="1"><input id="go" type="submit" name="go" value="Go"></td></tr></form></table>
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
async fn misnested_form_submits_the_controls_the_parser_gave_it() {
    let query = submitted_query("/misnested", "document.forms[0].submit()").await;
    assert_eq!(query.as_deref(), Some("a=1&s=y"));
}

#[tokio::test(flavor = "current_thread")]
async fn parser_owner_shows_in_form_and_elements_until_the_control_moves() {
    let value = evaluate(
        "/misnested-id",
        "(() => { const f = document.getElementById('f'), a = document.getElementById('a');
           const before = [f.children.length, a.form === f, f.elements.length].join(',');
           document.body.appendChild(a);
           return before + '|' + [a.form, f.elements.length].join(','); })()",
    )
    .await;
    // The <form> element itself stays empty; moving the input resets its owner.
    assert_eq!(value, json!("0,true,1|,0"));
}

#[tokio::test(flavor = "current_thread")]
async fn clicking_a_parser_associated_submit_button_submits_its_form() {
    let query = submitted_query("/misnested-button", "document.getElementById('go').click()").await;
    assert_eq!(query.as_deref(), Some("a=1&go=Go"));
}
