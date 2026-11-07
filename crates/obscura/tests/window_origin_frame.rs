//! Regression coverage for the replaceable `window.origin` global, at the
//! frame-realm level. A child iframe is its own realm with its own document
//! URL, so bare `origin` inside it must resolve to the CHILD's origin, not the
//! parent's, and must stay independently replaceable per realm.
//!
//! The hermetic test uses two loopback ports: a different port is already a
//! different origin (origin = scheme + host + port), so it exercises genuine
//! cross-origin behaviour without any external DNS. The `#[ignore]`d variant
//! uses nip.io to exercise a genuinely different registrable domain
//! (cross-site, not just cross-origin); it needs outbound DNS, so it is opt-in
//! and never runs in the offline gate.

use std::io::{Read, Write};

use obscura::Browser;

/// Serve one fixed HTML body on an ephemeral loopback port. Returns the base
/// URL (no trailing slash), matching the sibling child_frame_scripts harness.
fn spawn_server(body: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf).unwrap_or(0);
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

/// Serve a child body on an ephemeral port, plus a parent body that embeds an
/// `<iframe>` pointing at a caller-supplied absolute child URL. Returns the
/// parent base URL. The parent HTML is built per-call (the child URL is only
/// known at runtime), so this cannot reuse the `&'static str` server above.
fn spawn_parent_embedding(child_url: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let parent_html = format!(
        "<!doctype html><html><head><title>parent</title></head><body>\
         <iframe src=\"{child_url}\"></iframe></body></html>"
    );
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf).unwrap_or(0);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                parent_html.len()
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(parent_html.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://{addr}")
}

/// The child document reads its own `origin` three ways (bare identifier,
/// window.origin, location.origin) and reports whether they agree, then proves
/// the property is replaceable in its own realm.
const CHILD_HTML: &str = r#"<!doctype html><html><head><title>child</title></head><body>
<script>
  (function () {
    'use strict';
    // Bare identifier must resolve inside the frame realm.
    window.__bareOrigin = origin;
    window.__matchesLocation = (origin === location.origin);
    window.__matchesWindow = (window.origin === location.origin);
    // Replaceable in the child's own realm: assignment must not throw.
    var threw = false;
    try { origin = 'child-override'; } catch (e) { threw = true; }
    window.__assignThrew = threw;
    window.__afterAssign = window.origin;
  })();
</script>
</body></html>"#;

/// Hermetic: parent on port A, child iframe served from port B. A different
/// port is a different origin, so the child's bare `origin` must be its OWN
/// origin (port B), distinct from the parent's (port A), and independently
/// replaceable. No external network: both listeners are loopback.
#[tokio::test]
async fn child_frame_origin_is_its_own_and_replaceable() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let child_base = spawn_server(CHILD_HTML);
    let child_url = format!("{child_base}/child.html");
    let parent_base = spawn_parent_embedding(child_url.clone());

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent_base).await.unwrap();
    page.settle(2000).await;

    assert_eq!(
        page.frame_urls(),
        vec![child_url.clone()],
        "the cross-origin child iframe never became a frame"
    );

    // The child's bare `origin` resolves to the child's own origin (its port),
    // not the parent's. child_base is "http://127.0.0.1:<portB>", which is
    // exactly the serialized origin.
    assert_eq!(
        page.evaluate_in_frame(0, "window.__bareOrigin").unwrap(),
        serde_json::json!(child_base),
        "bare `origin` in the child must be the child's own origin"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__matchesLocation").unwrap(),
        serde_json::json!(true),
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__matchesWindow").unwrap(),
        serde_json::json!(true),
    );

    // The parent's own origin is a different port, so it must differ from the
    // child's. This is what "cross-origin by port" buys us with no DNS.
    let parent_origin = page.evaluate("location.origin");
    assert_eq!(parent_origin, serde_json::json!(parent_base));
    assert_ne!(
        parent_origin,
        serde_json::json!(child_base),
        "parent and child must have distinct origins for this to be a real cross-origin test"
    );

    // Replaceable inside the child's realm: strict-mode assignment succeeded
    // and shadowed the accessor, independently of the parent.
    assert_eq!(
        page.evaluate_in_frame(0, "window.__assignThrew").unwrap(),
        serde_json::json!(false),
        "assigning `origin` in the child realm must not throw"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__afterAssign").unwrap(),
        serde_json::json!("child-override"),
    );

    // The parent's own `origin` is untouched by the child's reassignment.
    assert_eq!(page.evaluate("window.origin"), serde_json::json!(parent_base));
}

/// Opt-in cross-SITE variant using nip.io. `<id>.<dashed-ip>.nip.io` resolves
/// to the embedded IP, so this serves real HTTP on loopback while presenting a
/// genuinely different registrable domain than the parent -- useful for
/// exercising cross-site behaviour that cross-origin-by-port does not reach.
///
/// Ignored by default: it performs an outbound DNS lookup to nip.io, so it is
/// not hermetic and must not run in the offline/deterministic gate. Run it
/// explicitly with:
///   cargo nextest run --release --features render -p obscura \
///     -- --ignored child_frame_origin_cross_site_via_nip_io
///
/// It binds the servers to 127.0.0.1 but advertises the child via a
/// `127-0-0-1.nip.io` host on the child's actual port, so the fetch still lands
/// on the local listener while the child's origin host differs from the
/// parent's.
#[tokio::test]
#[ignore = "needs outbound DNS to nip.io; opt-in cross-site platform, not part of the offline gate"]
async fn child_frame_origin_cross_site_via_nip_io() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let child_base = spawn_server(CHILD_HTML);
    // Re-point the child URL's host at a nip.io name that resolves to loopback,
    // keeping the port the listener is actually bound to. The listener accepts
    // on 127.0.0.1 regardless of the Host header, so the request still lands.
    let child_port = child_base
        .rsplit(':')
        .next()
        .expect("child base has a port")
        .to_string();
    let child_host = format!("127-0-0-1.nip.io:{child_port}");
    let child_origin = format!("http://{child_host}");
    let child_url = format!("{child_origin}/child.html");
    let parent_base = spawn_parent_embedding(child_url.clone());

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent_base).await.unwrap();
    page.settle(3000).await;

    assert_eq!(
        page.frame_urls(),
        vec![child_url.clone()],
        "the cross-site child iframe never became a frame (DNS to nip.io available?)"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__bareOrigin").unwrap(),
        serde_json::json!(child_origin),
        "bare `origin` in the child must be the child's own cross-site origin"
    );
    assert_ne!(
        page.evaluate("location.origin"),
        serde_json::json!(child_origin),
        "parent and child must be cross-site"
    );
    assert_eq!(
        page.evaluate_in_frame(0, "window.__afterAssign").unwrap(),
        serde_json::json!("child-override"),
    );
}
