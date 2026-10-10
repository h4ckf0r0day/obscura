use std::io::{Read, Write};
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};

fn spawn_page() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1_024];
        let _ = stream.read(&mut request).unwrap();
        let body = r#"<!doctype html><html><body data-ready="false">
            <script>
                globalThis.__rejectionObserved = false;
                addEventListener('unhandledrejection', function (event) {
                    globalThis.__rejectionObserved = event.reason.message === 'background failure';
                });
                Promise.reject(new Error('background failure'));
                setTimeout(function () {
                    document.body.setAttribute('data-ready', 'true');
                }, 20);
            </script>
        </body></html>"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
        .unwrap();
    });

    format!("http://{address}")
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_background_promise_does_not_stop_the_page_event_loop() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let context = Arc::new(BrowserContext::with_storage_and_network(
        "unhandled-rejection".to_owned(),
        None,
        false,
        None,
        None,
        true,
    ));
    let mut page = Page::new("unhandled-rejection-page".to_owned(), context);
    page.navigate(&spawn_page()).await.unwrap();
    page.settle(200).await;

    assert_eq!(
        page.evaluate("globalThis.__rejectionObserved"),
        serde_json::json!(true),
    );
    assert_eq!(
        page.evaluate("document.body.getAttribute('data-ready')"),
        serde_json::json!("true"),
    );
}
