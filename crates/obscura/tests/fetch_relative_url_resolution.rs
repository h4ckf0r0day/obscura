//! Issue #663: fetch and XHR skipped URL resolution as soon as the relative
//! URL contained "://" anywhere, so a query carrying an absolute URL (e.g.
//! `api/proxy?target=https://cdn/x.json`) fell through unresolved. The Fetch
//! spec lets the URL parser decide absoluteness; resolution must always run.
//!
//! The local server echoes the request target line back in the JSON body, so
//! the test asserts on the exact path the engine actually requested.

use std::io::{Read, Write};

use obscura::Browser;

fn spawn_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else {
                continue;
            };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let target = request
                    .split_whitespace()
                    .nth(1)
                    .map(str::to_string)
                    .unwrap_or_default();
                let (content_type, body) = if target.starts_with("/api") || target.starts_with("/deep/api") {
                    ("application/json", format!("{{\"path\":\"{}\"}}", target))
                } else {
                    (
                        "text/html",
                        "<!doctype html><html><head><title>fixture</title></head><body></body></html>".to_string(),
                    )
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
                    content_type,
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    format!("http://{}", addr)
}

/// Parse the echoed server JSON out of one probe slot and return the requested path.
fn requested_path(results: &serde_json::Value, key: &str) -> String {
    let raw = match results[key].as_str() {
        Some(s) => s.to_string(),
        None => format!("MISSING_SLOT_{}", key),
    };
    let inner: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    inner["path"].as_str().unwrap_or(&raw).to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_and_xhr_resolve_relative_urls_that_contain_double_slashes() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&format!("{base}/deep/page")).await.unwrap();

    let script = format!(
        r#"(function() {{
            var out = document.createElement('pre');
            out.id = 'probe-results';
            document.body.appendChild(out);
            var results = {{}};
            var pending = 5;
            function record(name, val) {{
                results[name] = val;
                pending -= 1;
                if (pending === 0) {{
                    out.textContent = JSON.stringify(results);
                    document.body.setAttribute('data-done', '1');
                }}
            }}
            // The issue repro: relative URL whose query carries an absolute one.
            fetch('api/proxy?target=https://cdn.example.com/x.json')
                .then(function(r) {{ return r.text(); }})
                .then(function(t) {{ record('f_query', t); }}, function(e) {{ record('f_query', 'REJECTED'); }});
            var x1 = new XMLHttpRequest();
            x1.open('GET', 'api/proxy?target=https://cdn.example.com/x.json');
            x1.onload = function() {{ record('x_query', x1.responseText); }};
            x1.onerror = function() {{ record('x_query', 'REJECTED'); }};
            x1.send();
            // Regression: plain relative URLs still resolve.
            fetch('api/ok')
                .then(function(r) {{ return r.text(); }})
                .then(function(t) {{ record('f_plain', t); }}, function(e) {{ record('f_plain', 'REJECTED'); }});
            var x2 = new XMLHttpRequest();
            x2.open('GET', 'api/ok?b=2');
            x2.onload = function() {{ record('x_plain', x2.responseText); }};
            x2.onerror = function() {{ record('x_plain', 'REJECTED'); }};
            x2.send();
            // Regression: absolute URLs stay intact.
            fetch('{base}/api/ok?abs=1')
                .then(function(r) {{ return r.text(); }})
                .then(function(t) {{ record('f_abs', t); }}, function(e) {{ record('f_abs', 'REJECTED'); }});
        }})()"#,
    );
    page.evaluate(&script);

    for _ in 0..40 {
        page.settle(250).await;
        if page.evaluate("document.body.getAttribute('data-done')") == serde_json::json!("1") {
            break;
        }
    }
    assert_eq!(
        page.evaluate("document.body.getAttribute('data-done')"),
        serde_json::json!("1"),
        "probes did not finish"
    );
    let raw = page.evaluate("document.getElementById('probe-results').textContent");
    let results: serde_json::Value =
        serde_json::from_str(raw.as_str().unwrap_or("")).unwrap_or(serde_json::Value::Null);

    let path = requested_path(&results, "f_query");
    assert!(
        path.starts_with("/deep/api/proxy?target=https"),
        "fetch must resolve against the page URL, got {path:?}"
    );
    let path = requested_path(&results, "x_query");
    assert!(
        path.starts_with("/deep/api/proxy?target=https"),
        "XHR must resolve against the page URL, got {path:?}"
    );
    assert_eq!(
        requested_path(&results, "f_plain"),
        "/deep/api/ok",
        "plain relative fetch resolution regressed"
    );
    assert_eq!(
        requested_path(&results, "x_plain"),
        "/deep/api/ok?b=2",
        "plain relative XHR resolution regressed"
    );
    assert_eq!(
        requested_path(&results, "f_abs"),
        "/api/ok?abs=1",
        "absolute fetch URL was altered"
    );
}
