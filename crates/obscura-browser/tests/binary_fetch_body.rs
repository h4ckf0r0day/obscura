use std::io::{Read, Write};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use obscura_browser::{BrowserContext, Page};

const BINARY_BODY: [u8; 4] = [0, 128, 255, 16];

fn spawn_echo_server() -> (String, mpsc::Receiver<Vec<u8>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (body_tx, body_rx) = mpsc::channel();

    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else {
                continue;
            };
            let body_tx = body_tx.clone();
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                let header_end = loop {
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break end + 4;
                    }
                };

                let (path, content_length) = {
                    let headers = std::str::from_utf8(&request[..header_end]).unwrap();
                    let path = headers.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    (path, content_length)
                };
                while request.len() < header_end + content_length {
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }

                if path == "/binary" {
                    body_tx
                        .send(request[header_end..header_end + content_length].to_vec())
                        .unwrap();
                }

                let (content_type, body) = if path == "/binary" {
                    ("text/plain", "ok")
                } else {
                    (
                        "text/html",
                        "<!doctype html><html><body>binary fetch fixture</body></html>",
                    )
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
        }
    });

    (format!("http://{address}"), body_rx)
}

async fn assert_binary_fetch_body(stealth: bool) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (base_url, body_rx) = spawn_echo_server();
    let context = Arc::new(BrowserContext::with_storage_and_network(
        "binary-fetch".to_string(),
        None,
        stealth,
        None,
        None,
        true,
    ));
    let mut page = Page::new("binary-fetch-page".to_string(), context);
    page.navigate(&base_url).await.unwrap();

    page.evaluate(
        r#"
        (function() {
            document.body.setAttribute('data-binary-fetch', 'pending');
            fetch('/binary', {
                method: 'POST',
                headers: { 'content-type': 'application/octet-stream' },
                body: new Uint8Array([0, 128, 255, 16]),
            })
                .then(function(response) {
                    if (!response.ok) throw new Error('HTTP ' + response.status);
                    document.body.setAttribute('data-binary-fetch', 'done');
                })
                .catch(function(error) {
                    document.body.setAttribute('data-binary-fetch', 'error: ' + String(error));
                });
        })()
        "#,
    );

    for _ in 0..20 {
        page.settle(100).await;
        let status = page.evaluate("document.body.getAttribute('data-binary-fetch')");
        if status != serde_json::json!("pending") {
            break;
        }
    }

    assert_eq!(
        page.evaluate("document.body.getAttribute('data-binary-fetch')"),
        serde_json::json!("done")
    );
    assert_eq!(
        body_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        BINARY_BODY
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_preserves_binary_request_body() {
    assert_binary_fetch_body(false).await;
}

#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn stealth_fetch_preserves_binary_request_body() {
    assert_binary_fetch_body(true).await;
}
