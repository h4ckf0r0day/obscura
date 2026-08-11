//! Fetch interception must pause static subresources (scripts, stylesheets)
//! and obey the client's per-request verdict — matching an interception
//! pattern is not a block. The old behavior treated the patterns as a block
//! list, so a client that only wanted stylesheets gone (puppeteer
//! setRequestInterception + abort on *.css*) silently lost every script too:
//! the whole page's JS never ran.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use obscura::{Browser, InterceptResolution};

/// `/` is a page with one stylesheet, one external script that defines
/// `window.dep`, and an inline script that records whether it ran.
fn spawn_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut s = match incoming {
                Ok(s) => s,
                Err(_) => continue,
            };
            std::thread::spawn(move || {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let req = std::str::from_utf8(&buf).unwrap_or("");
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let (ct, body) = if path.starts_with("/style.css") {
                    ("text/css", "body { color: red; }".to_string())
                } else if path.starts_with("/dep.js") {
                    ("application/javascript", "window.dep = true;".to_string())
                } else {
                    (
                        "text/html",
                        concat!(
                            "<!doctype html><html><head><title>fixture</title>",
                            "<link rel=\"stylesheet\" href=\"/style.css\">",
                            "</head><body><div id=\"out\">marker-before</div>",
                            "<script src=\"/dep.js\"></script>",
                            "<script>document.getElementById('out').textContent = ",
                            "window.dep ? 'marker-dep-ran' : 'marker-dep-missing';</script>",
                            "</body></html>",
                        )
                        .to_string(),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    ct,
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    format!("http://{}", addr)
}

#[tokio::test(flavor = "current_thread")]
async fn interception_client_blocks_css_scripts_still_run() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();

    // The client's policy: abort stylesheets, continue everything else —
    // puppeteer's request handler with a *.css* blocklist does exactly this.
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_writer = seen.clone();
    let mut rx = page.enable_interception();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            seen_writer.lock().unwrap().push(req.url.clone());
            let resolution = if req.url.contains(".css") {
                InterceptResolution::Fail {
                    reason: "BlockedByClient".to_string(),
                }
            } else {
                InterceptResolution::Continue {
                    url: None,
                    method: None,
                    headers: None,
                    body: None,
                }
            };
            let _ = req.resolver.send(resolution);
        }
    });

    page.goto(&base).await.unwrap();

    let marker = page.evaluate("document.getElementById('out').textContent");
    assert_eq!(
        marker,
        serde_json::json!("marker-dep-ran"),
        "external script must execute when the client only blocked css"
    );

    // Both subresources were paused and asked about, not silently dropped.
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().any(|u| u.contains("/style.css")),
        "stylesheet request never reached the interception client: {seen:?}"
    );
    assert!(
        seen.iter().any(|u| u.contains("/dep.js")),
        "script request never reached the interception client: {seen:?}"
    );
}
