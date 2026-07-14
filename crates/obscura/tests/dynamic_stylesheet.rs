use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use obscura::{Browser, CssMode};

fn spawn_stylesheet_server() -> (String, Arc<AtomicU32>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let stylesheet_requests = Arc::new(AtomicU32::new(0));
    let request_count = stylesheet_requests.clone();

    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            let mut buffer = [0u8; 4096];
            let bytes_read = stream.read(&mut buffer).unwrap_or(0);
            let request = std::str::from_utf8(&buffer[..bytes_read]).unwrap_or("");
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let (content_type, body) = if path == "/dynamic.css" {
                request_count.fetch_add(1, Ordering::SeqCst);
                ("text/css", "@import url('/import.css'); body { color: green; }")
            } else if path == "/import.css" {
                request_count.fetch_add(1, Ordering::SeqCst);
                ("text/css", "body { background-color: red; }")
            } else {
                (
                    "text/html",
                    r#"<script>
                        globalThis.stylesheetEvent = "pending";
                        const link = document.createElement("link");
                        link.rel = "stylesheet";
                        link.href = "/dynamic.css";
                        link.onload = () => globalThis.stylesheetEvent = "load";
                        link.addEventListener("error", () => globalThis.stylesheetEvent = "error");
                        document.head.appendChild(link);
                    </script>"#,
                )
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                content_type,
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    });

    (format!("http://{}", addr), stylesheet_requests)
}

#[tokio::test]
async fn dynamically_inserted_stylesheet_fetches_and_fires_load() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (base_url, stylesheet_requests) = spawn_stylesheet_server();
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();

    page.goto(&base_url).await.unwrap();
    for _ in 0..20 {
        page.settle(100).await;
        if page.evaluate("globalThis.stylesheetEvent") == serde_json::json!("load") {
            break;
        }
    }

    assert_eq!(
        page.evaluate("globalThis.stylesheetEvent"),
        serde_json::json!("load")
    );
    assert_eq!(
        page.evaluate("getComputedStyle(document.body).backgroundColor"),
        serde_json::json!("rgb(255, 0, 0)")
    );
    assert_eq!(
        page.evaluate("document.styleSheets.length").as_f64(),
        Some(1.0)
    );
    assert_eq!(stylesheet_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn drop_mode_dynamic_stylesheet_still_fetches_and_fires_load() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (base_url, stylesheet_requests) = spawn_stylesheet_server();
    let browser = Browser::builder()
        .css_mode(CssMode::Drop)
        .build()
        .unwrap();
    let mut page = browser.new_page().await.unwrap();

    page.goto(&base_url).await.unwrap();
    for _ in 0..20 {
        page.settle(100).await;
        if page.evaluate("globalThis.stylesheetEvent") == serde_json::json!("load") {
            break;
        }
    }

    assert_eq!(page.evaluate("globalThis.stylesheetEvent"), serde_json::json!("load"));
    assert_eq!(
        page.evaluate("document.styleSheets.length").as_f64(),
        Some(0.0)
    );
    assert_eq!(page.evaluate("document.querySelector('link').sheet"), serde_json::Value::Null);
    assert_eq!(stylesheet_requests.load(Ordering::SeqCst), 1);
}
