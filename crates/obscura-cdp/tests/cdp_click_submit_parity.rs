use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_once() -> String {
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
  <input type="hidden" name="vacancy_id" value="123">
  <textarea name="message">hello</textarea>
  <input type="checkbox" name="agree" value="yes" checked>
  <button id="submit" type="submit">Go</button>
</form>
<script>
function submitCompat() {
  const form = document.getElementById('f');
  const params = new URLSearchParams();
  form.querySelectorAll('input, textarea').forEach(function(field) {
    if (field.type === 'checkbox' && !field.checked) return;
    params.append(field.name, field.value);
  });
  location.href = form.action + '?' + params.toString();
}
document.querySelector('button').addEventListener('click', function(e) {
  e.preventDefault();
  submitCompat();
});
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

#[tokio::test(flavor = "current_thread")]
async fn runtime_click_submit_prevent_default_navigation_updates_page() {
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
        json!({"url": url}),
        session_id,
    )
    .await;

    let submit_compat_type = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": "typeof submitCompat", "returnByValue": true}),
        session_id,
    )
    .await;
    assert_eq!(submit_compat_type["result"]["value"], "function");

    let button = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({"expression": "document.getElementById('submit')"}),
        session_id,
    )
    .await;
    let object_id = button["result"]["objectId"].as_str().unwrap().to_string();

    cdp(
        &mut ctx,
        4,
        "Runtime.callFunctionOn",
        json!({"objectId": object_id, "functionDeclaration": "function() { this.click(); }"}),
        session_id,
    )
    .await;

    let page = ctx.get_page_mut(&page_id).unwrap();
    assert_eq!(page.url.as_ref().unwrap().path(), "/submitted");
    assert_eq!(
        page.url.as_ref().unwrap().query(),
        Some("vacancy_id=123&message=hello&agree=yes")
    );
    assert!(page
        .evaluate("document.body.textContent")
        .as_str()
        .unwrap()
        .contains("submitted"));
}
