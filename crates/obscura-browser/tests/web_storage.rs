use std::io::{Read, Write};
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use serde_json::json;

fn spawn_html_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else {
                continue;
            };
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|b| b == b"\r\n\r\n") {
                        break;
                    }
                }
                let body = "<!doctype html><html><body>storage fixture</body></html>";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    format!("http://{address}")
}

fn context(id: &str, storage_dir: Option<std::path::PathBuf>) -> Arc<BrowserContext> {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    Arc::new(BrowserContext::with_storage_and_network(
        id.to_string(),
        None,
        false,
        None,
        storage_dir,
        true,
    ))
}

#[tokio::test(flavor = "current_thread")]
async fn local_storage_is_shared_per_origin_and_survives_navigation() {
    let origin_a = spawn_html_server();
    let origin_b = spawn_html_server();
    let ctx = context("ls-shared", None);

    let mut page = Page::new("ls-page-1".to_string(), ctx.clone());
    page.navigate(&format!("{origin_a}/one")).await.unwrap();
    page.evaluate("(localStorage.setItem('token', 'abc'), localStorage.b = '2', 1)");

    // Same tab, same origin, new document.
    page.navigate(&format!("{origin_a}/two")).await.unwrap();
    assert_eq!(page.evaluate("localStorage.getItem('token')"), json!("abc"));
    assert_eq!(page.evaluate("String(localStorage.length)"), json!("2"));
    assert_eq!(page.evaluate("JSON.stringify(Object.keys(localStorage))"), json!("[\"b\",\"token\"]"));
    assert_eq!(page.evaluate("'token' in localStorage"), json!(true));

    // Another tab of the same origin sees the same area.
    let mut other = Page::new("ls-page-2".to_string(), ctx.clone());
    other.navigate(&format!("{origin_a}/three")).await.unwrap();
    assert_eq!(other.evaluate("localStorage.token"), json!("abc"));

    // A different origin does not.
    other.navigate(&format!("{origin_b}/")).await.unwrap();
    assert_eq!(other.evaluate("localStorage.getItem('token')"), json!(null));
    assert_eq!(other.evaluate("String(localStorage.length)"), json!("0"));

    page.evaluate("(localStorage.removeItem('b'), delete localStorage.token, 1)");
    assert_eq!(page.evaluate("String(localStorage.length)"), json!("0"));
}

#[tokio::test(flavor = "current_thread")]
async fn session_storage_is_per_tab() {
    let origin = spawn_html_server();
    let ctx = context("ss-tab", None);

    let mut page = Page::new("ss-page-1".to_string(), ctx.clone());
    page.navigate(&format!("{origin}/a")).await.unwrap();
    page.evaluate("(sessionStorage.setItem('step', '1'), 1)");
    page.navigate(&format!("{origin}/b")).await.unwrap();
    assert_eq!(page.evaluate("sessionStorage.getItem('step')"), json!("1"));

    let mut other = Page::new("ss-page-2".to_string(), ctx);
    other.navigate(&format!("{origin}/a")).await.unwrap();
    assert_eq!(other.evaluate("sessionStorage.getItem('step')"), json!(null));
}

#[tokio::test(flavor = "current_thread")]
async fn local_storage_persists_with_storage_dir() {
    let origin = spawn_html_server();
    let dir = std::env::temp_dir().join(format!("obscura-ls-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    {
        let ctx = context("ls-persist-1", Some(dir.clone()));
        let mut page = Page::new("ls-persist-page-1".to_string(), ctx.clone());
        page.navigate(&format!("{origin}/")).await.unwrap();
        page.evaluate("(localStorage.setItem('session', 'kept'), 1)");
        ctx.save_cookies();
    }

    let ctx = context("ls-persist-2", Some(dir.clone()));
    let mut page = Page::new("ls-persist-page-2".to_string(), ctx);
    page.navigate(&format!("{origin}/")).await.unwrap();
    assert_eq!(page.evaluate("localStorage.getItem('session')"), json!("kept"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "current_thread")]
async fn set_item_over_quota_throws_quota_exceeded_error() {
    let origin = spawn_html_server();
    let ctx = context("ls-quota", None);
    let mut page = Page::new("ls-quota-page".to_string(), ctx);
    page.navigate(&format!("{origin}/")).await.unwrap();
    let result = page.evaluate(
        "(function() { try { localStorage.setItem('big', 'x'.repeat(6 * 1024 * 1024)); return 'stored'; } catch (e) { return e.name; } })()",
    );
    assert_eq!(result, json!("QuotaExceededError"));
    assert_eq!(page.evaluate("localStorage.getItem('big')"), json!(null));
}
