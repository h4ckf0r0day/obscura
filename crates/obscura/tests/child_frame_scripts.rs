//! Regression test for issue #600: a child iframe's document was fetched and
//! parsed, but the frame never got a scripting context, so a `<script>` inside
//! it stayed an inert node. This is the reporter's loopback repro, reduced to
//! the part that needs no network: two documents on one local server.

use std::io::{Read, Write};

use obscura::Browser;

const PARENT_HTML: &str = r#"<!doctype html><html><head><title>parent</title></head><body>
<script>
  var f = document.createElement('iframe');
  f.src = '/child.html';
  document.body.appendChild(f);
</script>
</body></html>"#;

const ATTRIBUTE_PARENT_HTML: &str = r#"<!doctype html><html><head><title>parent</title></head><body>
<script>
  var f = document.createElement('iframe');
  f.setAttribute('src', '/child.html');
  document.body.appendChild(f);
</script>
</body></html>"#;

const SHADOW_PARENT_HTML: &str = r#"<!doctype html><html><head><title>parent</title></head><body>
<script>
  var host = document.createElement('div');
  var root = host.attachShadow({mode: 'open'});
  var f = document.createElement('iframe');
  f.setAttribute('src', '/child.html');
  root.appendChild(f);
  document.body.appendChild(host);
</script>
</body></html>"#;

const RESET_PARENT_HTML: &str = r#"<!doctype html><html><head><title>parent</title></head><body>
<script>
  var f = document.createElement('iframe');
  f.src = '/child.html';
  document.body.appendChild(f);
  setTimeout(function () { f.src = 'about:blank'; }, 150);
</script>
</body></html>"#;

const STATIC_PARENT_HTML: &str = r#"<!doctype html><html><head><title>parent</title></head><body>
<iframe src="/child.html"></iframe>
</body></html>"#;

const CHILD_HTML: &str = r#"<!doctype html><html><head><title>BEFORE</title></head><body>
<p>child</p>
<script>
  window.__ran = "YES";
  document.title = "RAN-IN-CHILD";
</script>
</body></html>"#;

/// The reporter's original pair: the child reports to its parent over
/// postMessage, which is how every embedded widget returns a result.
const MESSAGING_PARENT_HTML: &str = r#"<!doctype html><html><head><title>parent</title></head><body>
<script>
  window.__res = {parentGot: [], trusted: [], fromChildWindow: []};
  window.addEventListener('message', function (e) {
    window.__res.parentGot.push(String(e.data));
    window.__res.trusted.push(e.isTrusted === true);
    window.__res.fromChildWindow.push(e.source === document.querySelector('iframe').contentWindow);
  });
  var f = document.createElement('iframe');
  f.src = '/child-messaging.html';
  document.body.appendChild(f);
</script>
</body></html>"#;

const MESSAGING_CHILD_HTML: &str = r#"<!doctype html><html><body>
<script>
  try { parent.postMessage("FROM-CHILD", "*"); }
  catch (e) { document.title = "POST-THREW:" + e.message; }
  window.addEventListener('message', function (e) {
    window.__heard = String(e.data) + ':' + (e.isTrusted === true);
  });
</script>
</body></html>"#;

/// Minimal HTTP/1.1 server serving the parent document at `/` and the child at
/// `/child.html`.
fn spawn_server(parent_html: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let mut buf = [0u8; 2048];
            let read = stream.read(&mut buf).unwrap_or(0);
            let body = if buf[..read].starts_with(b"GET /child.html ") {
                CHILD_HTML
            } else if buf[..read].starts_with(b"GET /child-messaging.html ") {
                MESSAGING_CHILD_HTML
            } else {
                parent_html
            };
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_child_frame_runs_its_own_script() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;

    assert_eq!(
        page.frame_urls(),
        vec![format!("{base}/child.html")],
        "the child document never became a frame"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__ran").unwrap(),
        serde_json::json!("YES"),
        "the child frame's script did not run"
    );
    // The child's own script wrote this over the static <title>, so it proves
    // the script ran against the frame's document rather than anywhere else.
    assert_eq!(
        page.evaluate_in_frame(0, "document.title").unwrap(),
        serde_json::json!("RAN-IN-CHILD"),
    );
    // The frame's writes must not reach the parent's document.
    assert_eq!(
        page.evaluate("document.title").as_str().unwrap_or(""),
        "parent",
    );
    assert_eq!(page.evaluate("window.__ran"), serde_json::Value::Null);
}

#[tokio::test]
async fn a_child_frame_set_by_attribute_runs_its_own_script() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(ATTRIBUTE_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;

    assert_eq!(
        page.frame_urls(),
        vec![format!("{base}/child.html")],
        "setAttribute('src') did not start the child document"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__ran").unwrap(),
        serde_json::json!("YES"),
    );
}

#[tokio::test]
async fn a_shadow_dom_child_frame_stays_alive_and_runs_its_script() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(SHADOW_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;

    assert_eq!(
        page.frame_urls(),
        vec![format!("{base}/child.html")],
        "a shadow-root iframe realm was released as detached"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__ran").unwrap(),
        serde_json::json!("YES"),
    );
}

/// The whole point of a child frame having a scripting context: it can report
/// its result back out. This is the reporter's `parentGot` assertion.
#[tokio::test]
async fn a_child_frame_reaches_its_parent_with_post_message() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(MESSAGING_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;

    assert_eq!(
        page.evaluate("window.__res.parentGot"),
        serde_json::json!(["FROM-CHILD"]),
        "the child's message never reached the parent"
    );
    // A widget gates on isTrusted and drops anything else without a word.
    assert_eq!(
        page.evaluate("window.__res.trusted"),
        serde_json::json!([true]),
    );
    // And it replies through event.source, so that has to be the frame's window.
    assert_eq!(
        page.evaluate("window.__res.fromChildWindow"),
        serde_json::json!([true]),
    );
}

/// The other direction: a page talking into its frame.
#[tokio::test]
async fn a_parent_reaches_its_child_with_post_message() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(MESSAGING_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;

    page.evaluate("document.querySelector('iframe').contentWindow.postMessage('TO-CHILD', '*')");
    page.settle(1000).await;

    assert_eq!(
        page.evaluate_in_frame(0, "window.__heard").unwrap(),
        serde_json::json!("TO-CHILD:true"),
        "the parent's message never reached the child"
    );
}

/// `window.postMessage(x, '*')` targets the same window, and its listener has
/// to hear it. This was a no-op stub, so a page posting to itself waited
/// forever.
#[tokio::test]
async fn window_post_message_delivers_to_the_same_window() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(
        r#"<!doctype html><html><body><script>
  window.__got = [];
  window.addEventListener('message', (e) => window.__got.push([String(e.data), e.isTrusted === true]));
  window.postMessage('SELF', '*');
  window.__syncGot = window.__got.length;
</script></body></html>"#,
    );

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(1000).await;

    assert_eq!(
        page.evaluate("window.__got"),
        serde_json::json!([["SELF", true]]),
    );
    // postMessage never delivers synchronously.
    assert_eq!(page.evaluate("window.__syncGot").as_f64(), Some(0.0));
}

/// A parser-created `<iframe src>` never goes through the `src` setter, so
/// nothing used to start its load at all.
#[tokio::test]
async fn a_static_child_frame_runs_its_own_script() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(STATIC_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;

    assert_eq!(
        page.frame_urls(),
        vec![format!("{base}/child.html")],
        "a static iframe never started loading"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__ran").unwrap(),
        serde_json::json!("YES"),
    );
}

#[tokio::test]
async fn a_rejected_child_frame_does_not_leave_js_references() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    std::env::set_var("OBSCURA_MAX_LIVE_FRAMES", "0");
    let base = spawn_server(STATIC_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;

    assert!(page.frame_urls().is_empty(), "the frame cap was not applied");
    for registry in [
        "__obscura_frameObjects",
        "__obscura_frameWindows",
        "__obscura_frameElements",
    ] {
        assert_eq!(
            page.evaluate(&format!("Object.keys(globalThis.{registry}).length"))
                .as_f64(),
            Some(0.0),
            "rejected frame stayed in {registry}",
        );
    }
}

#[tokio::test]
async fn navigating_blank_releases_child_realms() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(STATIC_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(2000).await;
    assert_eq!(page.frame_urls().len(), 1);

    page.goto("about:blank").await.unwrap();
    assert!(page.frame_urls().is_empty(), "old frame realm survived navigation");
}

#[tokio::test]
async fn changing_iframe_src_releases_the_previous_realm() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(RESET_PARENT_HTML);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    page.settle(1000).await;

    assert!(page.frame_urls().is_empty(), "old src realm survived replacement");
    for registry in ["__obscura_frameWindows", "__obscura_frameElements"] {
        assert_eq!(
            page.evaluate(&format!("Object.keys(globalThis.{registry}).length"))
                .as_f64(),
            Some(0.0),
            "old iframe stayed in {registry}",
        );
    }
}
