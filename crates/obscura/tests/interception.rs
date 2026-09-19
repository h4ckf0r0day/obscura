//! End-to-end check for the request/response interception API on `obscura::Page`
//! (issue #306): preload scripts, the interception channel, and the passive
//! on_request/on_response callbacks all work for JS-initiated fetch()/XHR.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use obscura::{Browser, InterceptResolution, ResourceType};

/// Minimal HTTP/1.1 server: `/` returns HTML that fires `fetch('/api')`; `/api`
/// returns JSON. Enough to exercise JS fetch() interception + the callbacks.
fn spawn_echo_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut s = match incoming {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let req = std::str::from_utf8(&buf).unwrap_or("");
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let (ct, body) = if path.starts_with("/api") {
                ("application/json", "{\"hello\":\"world\"}".to_string())
            } else if path.starts_with("/modified") {
                ("text/plain", "REWRITTEN".to_string())
            } else {
                ("text/html", "<script>fetch('/api');</script>".to_string())
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
                ct,
                body.len(),
                body
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://{}", addr)
}

fn spawn_binary_server() -> (String, mpsc::Receiver<Vec<u8>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (body_tx, body_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 2048];
            let header_end = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break None;
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(pos) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break Some(pos + 4);
                }
            };
            let Some(header_end) = header_end else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]).into_owned();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while request.len() - header_end < content_length {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }

            let path = headers.split_whitespace().nth(1).unwrap_or("/");
            if path == "/binary" {
                let end = header_end + content_length.min(request.len() - header_end);
                let _ = body_tx.send(request[header_end..end].to_vec());
            }
            let body = if path == "/" {
                b"<!doctype html><title>binary interception</title>".as_slice()
            } else {
                b"ok".as_slice()
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    (format!("http://{}", addr), body_rx)
}

#[tokio::test]
async fn page_intercepts_and_observes_js_fetch() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_echo_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();

    // Passive on_request counter (fires for navigation + the JS fetch).
    let req_count = Arc::new(AtomicU32::new(0));
    let rc = req_count.clone();
    page.on_request(Arc::new(move |_info| {
        rc.fetch_add(1, Ordering::SeqCst);
    }));

    // Passive on_response: capture the /api (Fetch) body.
    let captured = Arc::new(Mutex::new(String::new()));
    let cap = captured.clone();
    page.on_response(Arc::new(move |info, resp| {
        if info.resource_type == ResourceType::Fetch {
            *cap.lock().unwrap() = String::from_utf8_lossy(&resp.body).into_owned();
        }
    }));

    // Channel-based interception: resolve every request Continue so the fetch
    // is not blocked.
    let mut rx = page.enable_interception();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            if req
                .resolver
                .send(InterceptResolution::Continue {
                    url: None,
                    method: None,
                    headers: None,
                    body: None,
                })
                .is_err()
            {
                break;
            }
        }
    });

    page.goto(&base).await.unwrap();
    // Pump the event loop so the inline fetch('/api') resolves.
    for _ in 0..20 {
        page.settle(500).await;
        if captured.lock().unwrap().contains("hello") {
            break;
        }
    }

    assert!(
        req_count.load(Ordering::SeqCst) >= 1,
        "on_request never fired for navigation or fetch"
    );
    let body = captured.lock().unwrap().clone();
    assert!(
        body.contains("hello"),
        "on_response did not capture the fetch response body: {:?}",
        body
    );
}

/// Issue #408 follow-up: callbacks are page-scoped. A callback registered on
/// page A must not fire for requests made by page B in the same browser
/// context, and must not survive page A being dropped.
#[tokio::test]
async fn callbacks_do_not_bleed_across_pages() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_echo_server();

    let browser = Browser::new().unwrap();
    let mut page_a = browser.new_page().await.unwrap();
    let mut page_b = browser.new_page().await.unwrap();

    let a_hits = Arc::new(AtomicU32::new(0));
    let a = a_hits.clone();
    page_a.on_request(Arc::new(move |_info| {
        a.fetch_add(1, Ordering::SeqCst);
    }));

    // Page B navigates (navigation + inline fetch('/api')); page A's callback
    // must stay silent.
    page_b.goto(&base).await.unwrap();
    page_b.settle(500).await;
    assert_eq!(
        a_hits.load(Ordering::SeqCst),
        0,
        "page A's on_request fired for page B's requests"
    );

    // Page A navigates; its own callback fires.
    page_a.goto(&base).await.unwrap();
    assert!(
        a_hits.load(Ordering::SeqCst) >= 1,
        "page A's on_request did not fire for its own navigation"
    );

    // Dropping page A must not leave its callback firing for page B.
    let before_drop = a_hits.load(Ordering::SeqCst);
    drop(page_a);
    page_b.goto(&base).await.unwrap();
    page_b.settle(500).await;
    assert_eq!(
        a_hits.load(Ordering::SeqCst),
        before_drop,
        "dropped page A's on_request still fired for page B's requests"
    );
}

#[tokio::test]
async fn page_rewrites_request_url_via_interception() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_echo_server();
    let modified = format!("{}/modified", base);

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();

    let captured = Arc::new(Mutex::new(String::new()));
    let cap = captured.clone();
    page.on_response(Arc::new(move |info, resp| {
        if info.resource_type == ResourceType::Fetch {
            *cap.lock().unwrap() = String::from_utf8_lossy(&resp.body).into_owned();
        }
    }));

    // Intercept and rewrite the /api request to /modified via Continue.
    let mut rx = page.enable_interception();
    let modified_for_task = modified.clone();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let new_url = if req.url.contains("/api") {
                Some(modified_for_task.clone())
            } else {
                None
            };
            if req
                .resolver
                .send(InterceptResolution::Continue {
                    url: new_url,
                    method: None,
                    headers: None,
                    body: None,
                })
                .is_err()
            {
                break;
            }
        }
    });

    page.goto(&base).await.unwrap();
    for _ in 0..20 {
        page.settle(500).await;
        if captured.lock().unwrap().contains("REWRITTEN") {
            break;
        }
    }

    let body = captured.lock().unwrap().clone();
    assert!(
        body.contains("REWRITTEN"),
        "interception Continue url-rewrite did not take effect; captured: {:?}",
        body
    );
}

#[tokio::test]
async fn page_continue_overrides_post_body_with_non_utf8_bytes() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (base, received_body) = spawn_binary_server();
    let expected: Vec<u8> = vec![0, 255, 128, 195, 40];

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let mut rx = page.enable_interception();
    let override_body = expected.clone();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let body = req.url.ends_with("/binary").then(|| override_body.clone());
            if req
                .resolver
                .send(InterceptResolution::Continue {
                    url: None,
                    method: None,
                    headers: None,
                    body,
                })
                .is_err()
            {
                break;
            }
        }
    });

    page.evaluate(
        "fetch('/binary', { method: 'POST', body: new Uint8Array([1, 2, 3]) })\
         .then(() => { globalThis.__binaryPostDone = true; })",
    );
    for _ in 0..20 {
        page.settle(100).await;
        if page.evaluate("globalThis.__binaryPostDone === true") == serde_json::json!(true) {
            break;
        }
    }

    assert_eq!(
        received_body
            .recv_timeout(Duration::from_secs(2))
            .expect("server should receive the overridden POST"),
        expected
    );
}

#[tokio::test]
async fn page_fulfill_preserves_non_utf8_response_bytes() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (base, _received_body) = spawn_binary_server();
    let expected: Vec<u8> = vec![0, 255, 128, 195, 40];

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let mut rx = page.enable_interception();
    let fulfilled_body = expected.clone();
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let resolution = if req.url.ends_with("/fulfilled") {
                InterceptResolution::Fulfill {
                    status: 200,
                    headers: std::collections::HashMap::from([(
                        "content-type".to_string(),
                        "application/octet-stream".to_string(),
                    )]),
                    body: fulfilled_body.clone(),
                }
            } else {
                InterceptResolution::Continue {
                    url: None,
                    method: None,
                    headers: None,
                    body: None,
                }
            };
            if req.resolver.send(resolution).is_err() {
                break;
            }
        }
    });

    page.evaluate(
        "fetch('/fulfilled')\
         .then(response => response.arrayBuffer())\
         .then(body => { globalThis.__fulfilledBytes = Array.from(new Uint8Array(body)); })",
    );
    for _ in 0..20 {
        page.settle(100).await;
        if page.evaluate("Array.isArray(globalThis.__fulfilledBytes)") == serde_json::json!(true) {
            break;
        }
    }

    assert_eq!(
        page.evaluate("globalThis.__fulfilledBytes"),
        serde_json::json!(expected)
    );
}

// Issue #408: a callback registered with on_response must be detachable via the
// returned id, so a crawler can stop capturing after a phase. Before the fix the
// vecs were append-only and a callback fired for the client's whole lifetime.
#[tokio::test]
async fn on_response_callback_can_be_detached() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_echo_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();

    let hits = Arc::new(AtomicU32::new(0));
    let h = hits.clone();
    let id = page.on_response(Arc::new(move |info, _resp| {
        if info.resource_type == ResourceType::Fetch {
            h.fetch_add(1, Ordering::SeqCst);
        }
    }));

    // First navigation: the callback fires for the JS fetch('/api').
    page.goto(&base).await.unwrap();
    for _ in 0..20 {
        page.settle(200).await;
        if hits.load(Ordering::SeqCst) >= 1 {
            break;
        }
    }
    let after_first = hits.load(Ordering::SeqCst);
    assert!(after_first >= 1, "on_response should fire while attached");

    // Detach it; off_response must report success and the id must be gone.
    assert!(page.off_response(id), "off_response must remove the callback");
    assert!(!page.off_response(id), "removing an already-removed id returns false");

    // Second navigation: the detached callback must not fire again.
    page.goto(&base).await.unwrap();
    for _ in 0..10 {
        page.settle(200).await;
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        after_first,
        "detached on_response callback must not fire after off_response"
    );
}
