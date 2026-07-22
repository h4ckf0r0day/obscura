use std::io::{Read, Write};

use obscura::Browser;

fn spawn_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = match incoming {
                Ok(stream) => stream,
                Err(_) => continue,
            };
            std::thread::spawn(move || {
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request);
                let request = std::str::from_utf8(&request).unwrap_or("");
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let (content_type, body) = if path.starts_with("/api") {
                    ("application/json", r#"{"ok":true}"#)
                } else {
                    ("text/html", "<!doctype html><html><head><title>fixture</title></head><body></body></html>")
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

async fn run_browser(base: &str) {
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(base).await.unwrap();

    let script = format!(
        r#"(function() {{
            var done = 0;
            function mark() {{
                done += 1;
                document.body.setAttribute('data-done', String(done));
                document.querySelector('title').textContent = 'done-' + done;
            }}
            for (var i = 0; i < 8; i++) {{
                fetch('{base}/api?fetch=' + i)
                    .then(function(r) {{ return r.json(); }})
                    .then(function() {{ return fetch('{base}/api?nested=' + i); }})
                    .then(mark)
                    .catch(function() {{}});
            }}
            for (var j = 0; j < 4; j++) {{
                var xhr = new XMLHttpRequest();
                xhr.open('GET', '{base}/api?xhr=' + j);
                xhr.addEventListener('load', mark);
                xhr.send();
            }}
        }})()"#,
    );
    page.evaluate(&script);

    for _ in 0..20 {
        page.settle(250).await;
        if page.evaluate("document.body.getAttribute('data-done')") == serde_json::json!("12") {
            break;
        }
    }
    assert_eq!(
        page.evaluate("document.body.getAttribute('data-done')"),
        serde_json::json!("12")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sequential_browsers_complete_concurrent_fetch_and_xhr() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    run_browser(&base).await;
    run_browser(&base).await;
}
