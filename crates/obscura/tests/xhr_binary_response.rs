//! XMLHttpRequest with `responseType = 'arraybuffer'` must return the response
//! bytes unchanged.
//!
//! XHR is implemented over fetch and used to read every body with
//! `resp.text()`, then rebuild the binary response types from that string.
//! `text()` -> `TextEncoder().encode()` is not a round-trip: a lenient UTF-8
//! decode treats any high byte as a lead byte, so `82 83` becomes U+0083 and
//! re-encodes as `c2 83`. Every byte >= 0x80 was rewritten and the length
//! changed with the content, while `fetch()` — which never took that detour —
//! was correct.
//!
//! The fixture is 256 bytes holding every byte value, so a lossy path cannot
//! coincidentally survive it, and the assertion is on the bytes rather than
//! only the length: a same-length corruption would otherwise pass.

use std::io::{Read, Write};

use obscura::Browser;

/// Serve `/bytes.bin` as the 256 values 0x00..=0xFF, and anything else as a
/// page to run the probe from.
fn spawn_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let target = request.split_whitespace().nth(1).unwrap_or("").to_string();

                let (content_type, body): (&str, Vec<u8>) = if target.starts_with("/bytes.bin") {
                    ("application/octet-stream", (0u8..=255).collect())
                } else {
                    (
                        "text/html",
                        b"<!doctype html><html><head><title>fixture</title></head><body></body></html>"
                            .to_vec(),
                    )
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
                    content_type,
                    body.len(),
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    format!("http://{}", addr)
}

#[tokio::test(flavor = "current_thread")]
async fn xhr_arraybuffer_returns_the_response_bytes_unchanged() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&format!("{base}/page")).await.unwrap();

    page.evaluate(
        r#"(function () {
            var out = document.createElement('pre');
            out.id = 'probe-results';
            document.body.appendChild(out);
            function done(v) {
                out.textContent = JSON.stringify(v);
                document.body.setAttribute('data-done', '1');
            }
            var x = new XMLHttpRequest();
            x.open('GET', 'bytes.bin', true);
            x.responseType = 'arraybuffer';
            x.onload = function () {
                var u8 = new Uint8Array(x.response);
                done({ length: u8.length, bytes: Array.prototype.slice.call(u8) });
            };
            x.onerror = function () { done({ error: 'xhr failed' }); };
            x.send();
        })()"#,
    );

    for _ in 0..40 {
        page.settle(250).await;
        if page.evaluate("document.body.getAttribute('data-done')") == serde_json::json!("1") {
            break;
        }
    }
    assert_eq!(
        page.evaluate("document.body.getAttribute('data-done')"),
        serde_json::json!("1"),
        "the XHR never completed"
    );

    let raw = page.evaluate("document.getElementById('probe-results').textContent");
    let result: serde_json::Value =
        serde_json::from_str(raw.as_str().unwrap_or("")).unwrap_or(serde_json::Value::Null);

    assert!(result["error"].is_null(), "probe reported {:?}", result["error"]);
    assert_eq!(
        result["length"].as_u64(),
        Some(256),
        "expected all 256 bytes, got {:?} — a lossy text round-trip changes the length",
        result["length"]
    );

    let got: Vec<u64> = result["bytes"]
        .as_array()
        .expect("probe returned no bytes")
        .iter()
        .map(|b| b.as_u64().unwrap_or(u64::MAX))
        .collect();
    let expected: Vec<u64> = (0..=255).collect();
    let first_diff = got.iter().zip(&expected).position(|(a, b)| a != b);
    assert!(
        first_diff.is_none(),
        "byte {} differs: expected {:?}, got {:?}",
        first_diff.unwrap(),
        expected.get(first_diff.unwrap()),
        got.get(first_diff.unwrap()),
    );
}
