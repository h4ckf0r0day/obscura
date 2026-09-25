// Regression for issue #474: external scripts inserted after a timer must be
// fetched and execute before an explicit post-navigation settle completes.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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

async fn serve_dynamic_order_fixture(explicitly_in_order: bool) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let server_active = active.clone();
    let server_peak = peak.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let active = server_active.clone();
            let peak = server_peak.clone();
            tokio::spawn(async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let read = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..read]);
                let (content_type, body) = if request.starts_with("GET /slow.js") {
                    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                    (
                        "application/javascript",
                        "window.__dynamicOrder.push('slow');".to_string(),
                    )
                } else if request.starts_with("GET /fast.js") {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    (
                        "application/javascript",
                        "window.__dynamicOrder.push('fast');".to_string(),
                    )
                } else {
                    let ordered = if explicitly_in_order {
                        "slow.async=false;fast.async=false;"
                    } else {
                        ""
                    };
                    let body = format!(
                        r#"<script>
window.__dynamicOrder=[];
var slow=document.createElement('script');
var fast=document.createElement('script');
{ordered}
slow.src='/slow.js';
fast.src='/fast.js';
document.head.appendChild(slow);
document.head.appendChild(fast);
</script>"#
                    );
                    ("text/html", body)
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    (format!("http://{addr}/"), peak)
}

async fn navigate_dynamic_order_fixture(explicitly_in_order: bool) -> (Vec<String>, usize, u128) {
    let (url, peak) = serve_dynamic_order_fixture(explicitly_in_order).await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "script-order-session";
    ctx.sessions.insert(session_id.to_string(), page_id);

    let started = std::time::Instant::now();
    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": url, "waitUntil": "load"}),
        session_id,
    )
    .await;
    let elapsed_ms = started.elapsed().as_millis();
    let result = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": "window.__dynamicOrder",
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    let order = result["result"]["value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    (order, peak.load(Ordering::SeqCst), elapsed_ms)
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_classic_fetch_concurrency_matches_force_async_state() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");

    let (async_order, async_peak, async_elapsed_ms) =
        navigate_dynamic_order_fixture(false).await;
    assert_eq!(async_order, ["fast", "slow"]);
    assert_eq!(
        async_peak, 2,
        "default dynamic classics must fetch concurrently"
    );

    let (ordered_order, ordered_peak, ordered_elapsed_ms) =
        navigate_dynamic_order_fixture(true).await;
    assert_eq!(ordered_order, ["slow", "fast"]);
    assert_eq!(
        ordered_peak, 2,
        "explicit async=false scripts must overlap fetches while retaining their execution-order queue"
    );
    assert!(
        ordered_elapsed_ms < async_elapsed_ms + 300,
        "execution ordering must not serialize network fetches: async={async_elapsed_ms}ms ordered={ordered_elapsed_ms}ms"
    );
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

    let at_load = cdp(
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
        at_load["result"]["value"],
        r#"{"directExecuted":false,"directLoaded":false,"nestedExecuted":false,"nestedLoaded":false}"#,
        "waitUntil=load must not invent a post-load timer settle"
    );

    // The automation caller opts into post-load work. This must drive the
    // timer, both concurrently fetched scripts, their bodies, and their load
    // handlers without relying on navigation to exceed browser load semantics.
    ctx.pages[0].settle(1_500).await;

    let settled = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({
            "expression": "JSON.stringify({directExecuted: !!window.__directExecuted, directLoaded: !!window.__directLoaded, nestedExecuted: !!window.__nestedExecuted, nestedLoaded: !!window.__nestedLoaded})",
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    assert_eq!(
        settled["result"]["value"],
        r#"{"directExecuted":true,"directLoaded":true,"nestedExecuted":true,"nestedLoaded":true}"#,
        "dynamic scripts must execute and fire load before explicit settle completes"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dynamic_data_scripts_execute_before_chained_load_handlers() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({
            "url": "data:text/html,<html><head></head><body></body></html>",
            "waitUntil": "load"
        }),
        session_id,
    )
    .await;

    cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({
            "expression": r#"(function () {
                var state = {
                    aExec: false,
                    aLoad: false,
                    bExec: false,
                    bLoad: false,
                    cExec: false,
                    cLoad: false
                };
                window.__dataScriptState = state;

                var a = document.createElement('script');
                a.src = 'data:,window.__dataScriptState.aExec=true';
                a.onload = function () {
                    state.aLoad = true;
                    var b = document.createElement('script');
                    b.src = 'data:text/html,' + encodeURIComponent('window.__dataScriptState.bExec=true');
                    b.onload = function () {
                        state.bLoad = true;
                        var c = document.createElement('script');
                        c.src = 'data:text/javascript;base64,' +
                            btoa('window.__dataScriptState.cExec=true').replace(/=+$/, '') +
                            '#ignored-fragment';
                        c.onload = function () { state.cLoad = true; };
                        document.head.appendChild(c);
                    };
                    document.head.appendChild(b);
                };
                document.head.appendChild(a);
                return 'kicked';
            })()"#,
            "returnByValue": true,
        }),
        session_id,
    )
    .await;

    ctx.pages[0].settle(500).await;

    let result = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({
            "expression": "JSON.stringify(window.__dataScriptState)",
            "returnByValue": true,
        }),
        session_id,
    )
    .await;
    assert_eq!(
        result["result"]["value"],
        r#"{"aExec":true,"aLoad":true,"bExec":true,"bLoad":true,"cExec":true,"cLoad":true}"#,
        "data URL script bodies must execute before each chained load handler"
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
