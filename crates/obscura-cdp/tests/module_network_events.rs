use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::{CdpRequest, CdpResponse};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SESSION: &str = "module-session";

struct EnvOverride {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvOverride {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

async fn serve(routes: Vec<(&'static str, u16, &'static str, &'static [u8])>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = std::sync::Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let length = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..length]);
                let path = request.lines().next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1)).unwrap_or("/");
                let (_, status, headers, body) = routes.iter().find(|r| r.0 == path)
                    .unwrap_or_else(|| panic!("unexpected request: {path}"));
                let mime = if path.ends_with(".js") { "application/javascript" } else { "text/html" };
                let response = format!(
                    "HTTP/1.1 {status} Response\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nX-Module-Probe: observed\r\n{headers}Connection: close\r\n\r\n",
                    body.len(),
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.write_all(body).await.unwrap();
            });
        }
    });
    format!("http://{addr}")
}

async fn request(ctx: &mut CdpContext, method: &str, params: Value) -> CdpResponse {
    dispatch(&CdpRequest {
        id: 1, method: method.to_string(), params, session_id: Some(SESSION.to_string()),
    }, ctx).await
}

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = request(ctx, method, params).await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn navigate(base: &str, stealth: bool) -> CdpContext {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let mut ctx = CdpContext::new_with_options(None, stealth);
    let page = ctx.create_page();
    ctx.sessions.insert(SESSION.to_string(), page);
    cdp(&mut ctx, "Network.enable", json!({})).await;
    cdp(&mut ctx, "Page.navigate", json!({"url": format!("{base}/"), "waitUntil": "load"})).await;
    ctx
}

fn module_request_id(ctx: &CdpContext, url: &str, status: u16, size: usize) -> String {
    let responses = ctx.pending_events.iter().filter(|e| {
        e.method == "Network.responseReceived" && e.params["response"]["url"] == url
    }).collect::<Vec<_>>();
    assert_eq!(responses.len(), 1, "exactly one response for {url}");
    let response = &responses[0].params;
    assert_eq!(response["type"], "Script");
    assert_eq!(response["response"]["status"], status);
    assert_eq!(response["response"]["headers"]["x-module-probe"], "observed");
    let id = response["requestId"].as_str().unwrap().to_string();
    let starts = ctx.pending_events.iter().filter(|e| {
        e.method == "Network.requestWillBeSent" && e.params["requestId"] == id
    }).count();
    assert_eq!(starts, 1);
    let finishes = ctx.pending_events.iter().filter(|e| {
        e.method == "Network.loadingFinished" && e.params["requestId"] == id
    }).collect::<Vec<_>>();
    assert_eq!(finishes.len(), 1);
    assert_eq!(finishes[0].params["encodedDataLength"], size);
    id
}

async fn assert_module(ctx: &mut CdpContext, base: &str, path: &str, status: u16, body: &str) {
    let id = module_request_id(ctx, &format!("{base}{path}"), status, body.len());
    let stored = cdp(ctx, "Network.getResponseBody", json!({"requestId": id})).await;
    assert_eq!(stored, json!({"body": body, "base64Encoded": false}));
}

async fn static_graph(stealth: bool) {
    const ROOT: &str = "import '/child.js'; globalThis.rootRuns=(globalThis.rootRuns||0)+1;";
    const CHILD: &str = "globalThis.childRuns=(globalThis.childRuns||0)+1;";
    const ASYNC: &str = "globalThis.asyncRan=true;";
    let base = serve(vec![
        ("/", 200, "", b"<script type='module' src='/root.js'></script><script type='module' src='/child.js'></script><script type='module' src='/root.js'></script><script type='module' async src='/async.js'></script>"),
        ("/root.js", 201, "Content-Disposition: attachment\r\n", ROOT.as_bytes()),
        ("/child.js", 200, "", CHILD.as_bytes()),
        ("/async.js", 200, "", ASYNC.as_bytes()),
    ]).await;
    let mut ctx = navigate(&base, stealth).await;
    let result = cdp(&mut ctx, "Runtime.evaluate", json!({
        "expression": "[globalThis.rootRuns,globalThis.childRuns,globalThis.asyncRan]", "returnByValue": true,
    })).await;
    assert_eq!(result["result"]["value"], json!([1,1,true]));
    assert_module(&mut ctx, &base, "/root.js", 201, ROOT).await;
    assert_module(&mut ctx, &base, "/child.js", 200, CHILD).await;
    assert_module(&mut ctx, &base, "/async.js", 200, ASYNC).await;
}

#[tokio::test(flavor = "current_thread")]
async fn static_modules_record_real_responses_once() {
    static_graph(false).await;
}

#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn stealth_modules_record_real_responses_once() {
    static_graph(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn late_import_records_root_and_redirected_descendant() {
    const ROOT: &str = "import { value } from '/redirect.js'; export { value };";
    const CHILD: &str = "export const value='lazy-ready';";
    let base = serve(vec![
        ("/", 200, "", b"<!doctype html><body>lazy module</body>"),
        ("/lazy.js", 200, "", ROOT.as_bytes()),
        ("/redirect.js", 302, "Location: /child.js\r\n", b""),
        ("/child.js", 200, "", CHILD.as_bytes()),
    ]).await;
    let mut ctx = navigate(&base, false).await;
    ctx.pending_events.clear();
    let result = cdp(&mut ctx, "Runtime.evaluate", json!({
        "expression": "import('/lazy.js').then(m=>m.value)", "awaitPromise": true, "returnByValue": true,
    })).await;
    assert_eq!(result["result"]["value"], "lazy-ready");
    // The live server drains after commands and autonomous turns; dispatch
    // alone does not run that owner loop, so flush its page queue explicitly.
    ctx.pages[0].sync_js_network_events();
    for (path, body) in [("/lazy.js", ROOT), ("/child.js", CHILD)] {
        let events = ctx.pages[0].network_events.iter()
            .filter(|e| e.url == format!("{base}{path}")).collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
        let event = events[0];
        assert_eq!(event.resource_type, "Script");
        assert_eq!(event.status, 200);
        assert_eq!(event.body_size, body.len());
        assert_eq!(event.response_headers.get("x-module-probe").map(String::as_str), Some("observed"));
        let id = event.request_id.clone();
        let stored = cdp(&mut ctx, "Network.getResponseBody", json!({"requestId": id})).await;
        assert_eq!(stored, json!({"body": body, "base64Encoded": false}));
    }
    let recorded = ctx.pages[0].network_events.len();
    cdp(&mut ctx, "Runtime.evaluate", json!({
        "expression": "import('/lazy.js')", "awaitPromise": true,
    })).await;
    ctx.pages[0].sync_js_network_events();
    assert_eq!(ctx.pages[0].network_events.len(), recorded,
        "cached import must not invent another request");
}

#[tokio::test(flavor = "current_thread")]
async fn module_responses_survive_http_and_evaluation_errors() {
    const THROWS: &str = "throw new Error('module evaluation failed');";
    let base = serve(vec![
        ("/", 200, "", b"<script type='module' src='/missing.js'></script><script type='module' src='/throws.js'></script>"),
        ("/missing.js", 404, "", b"not found"),
        ("/throws.js", 200, "", THROWS.as_bytes()),
    ]).await;
    let mut ctx = navigate(&base, false).await;
    assert_module(&mut ctx, &base, "/missing.js", 404, "not found").await;
    assert_module(&mut ctx, &base, "/throws.js", 200, THROWS).await;
}

async fn body_not_retained() {
    const BODY: &str = "globalThis.moduleWithLargeBodyRan=true;";
    let base = serve(vec![
        ("/", 200, "", b"<script type='module' src='/large.js'></script>"),
        ("/large.js", 200, "", BODY.as_bytes()),
    ]).await;
    let mut ctx = navigate(&base, false).await;
    let id = module_request_id(&ctx, &format!("{base}/large.js"), 200, BODY.len());
    let response = request(&mut ctx, "Network.getResponseBody", json!({"requestId": id})).await;
    assert!(response.error.is_some(), "oversized response body must not be retained");
}

#[tokio::test(flavor = "current_thread")]
async fn module_body_buffer_limit_keeps_response_metadata() {
    let _env = EnvOverride::set("OBSCURA_NETWORK_BODY_BUFFER_BYTES", "16");
    body_not_retained().await;
}

#[tokio::test(flavor = "current_thread")]
async fn disabled_module_body_buffer_keeps_response_metadata() {
    let _env = EnvOverride::set("OBSCURA_NETWORK_BODY_BUFFER_ENTRIES", "0");
    body_not_retained().await;
}

#[tokio::test(flavor = "current_thread")]
async fn module_body_buffer_evicts_oldest_entry() {
    let _env = EnvOverride::set("OBSCURA_NETWORK_BODY_BUFFER_ENTRIES", "1");
    const ROOT: &str = "import '/child.js';";
    const CHILD: &str = "export const ready=true;";
    let base = serve(vec![
        ("/", 200, "", b"<script type='module' src='/root.js'></script>"),
        ("/root.js", 200, "", ROOT.as_bytes()),
        ("/child.js", 200, "", CHILD.as_bytes()),
    ]).await;
    let mut ctx = navigate(&base, false).await;
    let id = module_request_id(&ctx, &format!("{base}/root.js"), 200, ROOT.len());
    assert!(request(&mut ctx, "Network.getResponseBody", json!({"requestId": id})).await.error.is_some());
    assert_module(&mut ctx, &base, "/child.js", 200, CHILD).await;
}

#[tokio::test(flavor = "current_thread")]
async fn module_records_survive_page_runtime_suspension() {
    let base = serve(vec![
        ("/", 200, "", b"<script type='module' src='/root.js'></script>"),
        ("/root.js", 200, "", b"import '/child.js';"),
        ("/child.js", 200, "", b"globalThis.childRan=true;"),
        ("/lazy.js", 200, "", b"globalThis.lazyRan=true;"),
    ]).await;
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let mut ctx = CdpContext::new();
    ctx.create_page();
    let page = &mut ctx.pages[0];
    page.navigate(&format!("{base}/")).await.unwrap();
    assert!(page.network_events.iter().any(|e| e.url == format!("{base}/root.js") && e.body_size > 0),
        "direct Page navigation must expose parser module responses");
    page.evaluate("import('/lazy.js')");
    page.settle_for_duration(100).await;
    assert_eq!(page.evaluate("globalThis.lazyRan"), json!(true));
    page.suspend_js();
    for path in ["/root.js", "/child.js", "/lazy.js"] {
        assert_eq!(page.network_events.iter().filter(|e| e.url == format!("{base}{path}") && e.body_size > 0).count(), 1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_tracker_module_does_not_report_successful_response() {
    let mut client = obscura_net::ObscuraHttpClient::with_full_options(
        std::sync::Arc::new(obscura_net::CookieJar::new()), None, true,
    );
    client.block_trackers = true;
    let mut runtime = obscura_js::runtime::ObscuraJsRuntime::with_base_url("http://example.com/");
    runtime.set_http_client(std::sync::Arc::new(client));
    let error = runtime.load_module("http://doubleclick.net/blocked.js", 1_000).await.unwrap_err();
    assert!(error.contains("HTTP 0"), "{error}");
    assert!(runtime.take_js_network_events().is_empty(), "a blocked tracker was never fetched");
}

#[tokio::test(flavor = "current_thread")]
async fn module_body_buffer_preserves_non_utf8_bytes() {
    use base64::Engine;
    const BODY: &[u8] = b"// \xe9\nglobalThis.nonUtf8ModuleRan=true;";
    let base = serve(vec![
        ("/", 200, "", b"<script type='module' src='/raw.js'></script>"),
        ("/raw.js", 200, "", BODY),
    ]).await;
    let mut ctx = navigate(&base, false).await;
    let id = module_request_id(&ctx, &format!("{base}/raw.js"), 200, BODY.len());
    let stored = cdp(&mut ctx, "Network.getResponseBody", json!({"requestId": id})).await;
    assert_eq!(stored, json!({
        "body": base64::engine::general_purpose::STANDARD.encode(BODY), "base64Encoded": true,
    }));
}
