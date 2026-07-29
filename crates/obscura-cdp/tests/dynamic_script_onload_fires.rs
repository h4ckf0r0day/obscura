// Regression for issue #474: external scripts inserted after a timer must be
// fetched and execute before the post-navigation event-loop settle completes.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..read]);
                let (content_type, body) = if request.starts_with("GET /direct.js") {
                    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    ("application/javascript", "window.__directExecuted = true;")
                } else if request.starts_with("GET /nested.js") {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    ("application/javascript", "window.__nestedExecuted = true;")
                } else {
                    (
                        "text/html",
                        r#"<html><body>
<div id="r">stage1</div>
<script>
setTimeout(function () {
  var direct = document.createElement("script");
  direct.src = "/direct.js";
  direct.onload = function () { window.__directLoaded = true; };
  document.body.appendChild(direct);

  var box = document.createElement("div");
  var nested = document.createElement("script");
  nested.src = "/nested.js";
  nested.onload = function () { window.__nestedLoaded = true; };
  box.appendChild(nested);
  document.body.appendChild(box);
}, 100);
</script>
</body></html>"#,
                    )
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
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
    let response = dispatch(
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
        response.error.is_none(),
        "CDP {method} failed: {:?}",
        response.error
    );
    response.result.unwrap_or_else(|| json!({}))
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_external_scripts_execute_and_fire_load() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": url, "waitUntil": "load"}),
        session_id,
    )
    .await;

    let result = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": "JSON.stringify({directExecuted: !!window.__directExecuted, directLoaded: !!window.__directLoaded, nestedExecuted: !!window.__nestedExecuted, nestedLoaded: !!window.__nestedLoaded})",
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    assert_eq!(
        result["result"]["value"],
        r#"{"directExecuted":true,"directLoaded":true,"nestedExecuted":true,"nestedLoaded":true}"#,
        "dynamic scripts must execute and fire load before navigation settles"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_data_script_onload_chain_executes_in_order() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": "data:text/html,<html><head></head><body></body></html>", "waitUntil": "load"}),
        session_id,
    )
    .await;

    let kick = cdp(
        &mut ctx,
        2,
        "Runtime.callFunctionOn",
        json!({
            "functionDeclaration": r#"function () {
                var state = {
                    aExec: false, aLoad: false,
                    bExec: false, bLoad: false,
                    cExec: false, cLoad: false,
                    errors: [], order: []
                };
                window.__dataScriptChain = state;

                var a = document.createElement('script');
                a.src = "data:text/javascript,window.__dataScriptChain.aExec%3Dtrue%3Bwindow.__dataScriptChain.order.push(%27aExec%27)";
                a.onerror = function () { state.errors.push('aError'); };
                a.onload = function () {
                    state.aLoad = true;
                    state.order.push('aLoad');

                    var b = document.createElement('script');
                    b.src = 'data:text/javascript;base64,' + btoa("window.__dataScriptChain.bExec=true;window.__dataScriptChain.order.push('bExec')");
                    b.onerror = function () { state.errors.push('bError'); };
                    b.onload = function () {
                        state.bLoad = true;
                        state.order.push('bLoad');

                        var c = document.createElement('script');
                        c.src = "data:text/javascript,window.__dataScriptChain.cExec%3Dtrue%3Bwindow.__dataScriptChain.order.push(%27cExec%27)";
                        c.onerror = function () { state.errors.push('cError'); };
                        c.onload = function () {
                            state.cLoad = true;
                            state.order.push('cLoad');
                        };
                        document.head.appendChild(c);
                    };
                    document.head.appendChild(b);
                };
                document.head.appendChild(a);
                return 'kicked';
            }"#,
            "awaitPromise": true,
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    assert_eq!(kick["result"]["value"], "kicked");

    // Match a Puppeteer-side wait: no page promise is awaited between the
    // synchronous kick and read commands.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let result = cdp(
        &mut ctx,
        3,
        "Runtime.callFunctionOn",
        json!({
            "functionDeclaration": "function () { return JSON.stringify(window.__dataScriptChain); }",
            "awaitPromise": true,
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    let actual: Value = serde_json::from_str(result["result"]["value"].as_str().unwrap()).unwrap();
    assert_eq!(
        actual,
        json!({
            "aExec": true, "aLoad": true,
            "bExec": true, "bLoad": true,
            "cExec": true, "cLoad": true,
            "errors": [],
            "order": ["aExec", "aLoad", "bExec", "bLoad", "cExec", "cLoad"]
        }),
        "data: script bodies must execute before load and chained insertions must drain"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_dynamic_data_script_fires_error_not_load() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": "data:text/html,<html><head></head><body></body></html>", "waitUntil": "load"}),
        session_id,
    )
    .await;

    cdp(
        &mut ctx,
        2,
        "Runtime.callFunctionOn",
        json!({
            "functionDeclaration": r#"function () {
                window.__invalidDataScript = { error: false, load: false };
                var script = document.createElement('script');
                script.src = 'data:text/javascript;base64,!';
                script.onerror = function () { window.__invalidDataScript.error = true; };
                script.onload = function () { window.__invalidDataScript.load = true; };
                document.head.appendChild(script);
            }"#,
            "awaitPromise": true,
        }),
        session_id,
    )
    .await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let result = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({
            "expression": "JSON.stringify(window.__invalidDataScript)",
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    assert_eq!(result["result"]["value"], r#"{"error":true,"load":false}"#);
}
