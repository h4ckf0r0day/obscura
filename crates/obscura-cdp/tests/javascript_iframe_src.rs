//! Regression tests for issue #1245. An `<iframe src="javascript:...">` was
//! fetched like any other URL, refused, and left empty, so its script never
//! ran. A javascript: URL assigned to a frame's location was queued as a
//! navigation of the page itself, and following it failed Page.navigate for a
//! page that had loaded fine.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves `body` at every path, so the page has the http origin that
/// example.com gives the issue's repro and its initial frames inherit.
async fn serve(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = socket.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session: &str) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session.to_string()),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

async fn attached_session(ctx: &mut CdpContext) -> String {
    let created = dispatch(
        &CdpRequest {
            id: 900,
            method: "Target.createTarget".to_string(),
            params: json!({"url": "about:blank"}),
            session_id: None,
        },
        ctx,
    )
    .await
    .result
    .expect("Target.createTarget produced no result");
    let target_id = created["targetId"].as_str().expect("no targetId").to_string();

    let attached = dispatch(
        &CdpRequest {
            id: 901,
            method: "Target.attachToTarget".to_string(),
            params: json!({"targetId": target_id, "flatten": true}),
            session_id: None,
        },
        ctx,
    )
    .await
    .result
    .expect("Target.attachToTarget produced no result");
    attached["sessionId"].as_str().expect("no sessionId").to_string()
}

/// Navigates a fresh page to `body`, then awaits `expression` by value. The
/// result carries the page's URL as `pageUrl`.
async fn navigate_and_evaluate(body: &'static str, expression: &str) -> Value {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve(body).await;
    let mut ctx = CdpContext::new();
    let session = &attached_session(&mut ctx).await;
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url}), session).await;
    let evaluated = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
        session,
    )
    .await;
    assert!(evaluated.get("exceptionDetails").is_none(), "{evaluated}");
    let mut value = evaluated["result"]["value"].clone();
    value["pageUrl"] = json!(url);
    value
}

/// The issue's repro, verbatim, against the result headless Chrome gives.
#[tokio::test(flavor = "current_thread")]
async fn javascript_src_runs_in_the_frame_realm() {
    let out = navigate_and_evaluate(
        "<!doctype html><html><body><p>page</p></body></html>",
        r#"new Promise((resolve) => {
  const out = {};
  window.__cb = (w) => { out.callback = w && w.document ? "called with the frame window" : String(w); };
  const a = document.createElement("iframe");
  a.src = 'javascript:"<p id=x>hello</p>"';
  const b = document.createElement("iframe");
  b.src = "javascript:(parent.__cb(window), '')";
  document.body.append(a, b);
  setTimeout(() => {
    try { out.html = a.contentDocument && a.contentDocument.body ? a.contentDocument.body.innerHTML : null; }
    catch (e) { out.html = "error: " + e.message; }
    out.callback = out.callback || "not called";
    resolve(out);
  }, 1500);
})"#,
    )
    .await;
    assert_eq!(out["callback"], "called with the frame window", "{out}");
    assert_eq!(out["html"], "<p id=\"x\">hello</p>", "{out}");
}

/// A string result becomes the frame's document; anything else leaves the
/// initial about:blank document in place. Either way the script ran in the
/// frame and the parent can read the document afterwards. A page that writes
/// into the frame before the URL runs aborts it and keeps what it wrote.
#[tokio::test(flavor = "current_thread")]
async fn javascript_src_result_decides_the_frame_document() {
    let out = navigate_and_evaluate(
        "<!doctype html><html><body></body></html>",
        r#"new Promise((resolve) => {
  const loads = {};
  const frame = (name, src, sandbox) => {
    const f = document.createElement("iframe");
    if (sandbox !== undefined) f.setAttribute("sandbox", sandbox);
    f.addEventListener("load", () => { loads[name] = (loads[name] || 0) + 1; });
    f.src = src;
    document.body.append(f);
    return f;
  };
  const replaced = frame("replaced", 'javascript:"<p id=x>hello</p>"');
  const kept = frame("kept",
    "javascript:void(document.body.setAttribute('data-realm', window === parent ? 'page' : 'frame'))");
  const encoded = frame("encoded", "javascript:%22%3Cb%3Ecaf%C3%A9%3C%2Fb%3E%22");
  const sandboxed = frame("sandboxed",
    "javascript:(parent.__sandboxRan = true, '<p>ran</p>')", "allow-same-origin");
  const written = frame("written", 'javascript:"<p>from the url</p>"');
  const writtenDoc = written.contentDocument;
  writtenDoc.open();
  writtenDoc.write("<p>written</p>");
  writtenDoc.close();
  const appended = frame("appended", 'javascript:"<p>from the url</p>"');
  appended.contentDocument.write("<p>appended</p>");
  setTimeout(() => {
    const doc = (f) => [f.contentDocument.URL, f.contentDocument.body.outerHTML];
    resolve({
      replaced: doc(replaced),
      kept: doc(kept),
      encoded: doc(encoded),
      sandboxed: doc(sandboxed),
      sandboxRan: window.__sandboxRan === true,
      written: doc(written),
      appended: doc(appended),
      loads,
    });
  }, 500);
})"#,
    )
    .await;
    assert_eq!(out["replaced"], json!(["about:blank", "<body><p id=\"x\">hello</p></body>"]), "{out}");
    assert_eq!(out["kept"], json!(["about:blank", "<body data-realm=\"frame\"></body>"]), "{out}");
    assert_eq!(out["encoded"], json!(["about:blank", "<body><b>café</b></body>"]), "{out}");
    // Without allow-scripts a sandboxed frame runs nothing.
    assert_eq!(out["sandboxed"], json!(["about:blank", "<body></body>"]), "{out}");
    assert_eq!(out["sandboxRan"], false, "{out}");
    assert_eq!(out["written"], json!(["about:blank", "<body><p>written</p></body>"]), "{out}");
    assert_eq!(out["appended"], json!(["about:blank", "<body><p>appended</p></body>"]), "{out}");
    assert_eq!(out["loads"]["replaced"], 1, "{out}");
}

/// A page that, while loading, sends javascript: URLs through every location
/// route must still load: Page.navigate succeeds, the page keeps its URL, and
/// each script ran against the document it was aimed at.
#[tokio::test(flavor = "current_thread")]
async fn javascript_navigations_during_load_keep_the_page() {
    let out = navigate_and_evaluate(
        r#"<!doctype html><html><body>
<form id="f" action="javascript:void(document.body.setAttribute('data-form', 'ran'))"></form>
<script>
  function frame() {
    var f = document.createElement('iframe');
    document.body.appendChild(f);
    return f;
  }
  frame().contentWindow.location.replace("javascript:void(document.body.setAttribute('data-ran', 'replace'))");
  frame().contentWindow.location.href = "javascript:void(document.body.setAttribute('data-ran', 'href'))";
  frame().contentDocument.location = "javascript:void(document.body.setAttribute('data-ran', 'document'))";
  location.href = "javascript:void(document.body.setAttribute('data-top', 'ran'))";
  document.getElementById('f').submit();
</script>
</body></html>"#,
        r#"new Promise((resolve) => setTimeout(() => resolve({
  href: location.href,
  frames: Array.from(document.querySelectorAll("iframe"), (f) => f.contentDocument.body.getAttribute("data-ran")),
  top: document.body.getAttribute("data-top"),
  form: document.body.getAttribute("data-form"),
}), 200))"#,
    )
    .await;
    assert_eq!(out["href"], out["pageUrl"], "the page lost its URL: {out}");
    assert_eq!(out["frames"], json!(["replace", "href", "document"]), "{out}");
    assert_eq!(out["top"], "ran", "{out}");
    assert_eq!(out["form"], "ran", "{out}");
}
