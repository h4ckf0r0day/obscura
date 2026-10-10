//! #756: dropping a page while another one is alive aborted the process.
//!
//! Each `Page` owns its own `ObscuraJsRuntime`, so N pages means N isolates on
//! the thread. rusty_v8 enters an isolate when it is constructed and exits it
//! when it is dropped, which makes V8's entered-isolate stack the construction
//! order and means isolates have to be dropped in exactly the reverse of it --
//! a contract an embedder holding independent `Page` objects cannot keep. The
//! process died either at deno_core's context cleanup
//! (`Check failed: heap->isolate() == Isolate::TryGetCurrent()`) or, once that
//! was addressed naively, at `Isolate::Dispose` ("Disposing the isolate that is
//! entered by a thread").

use std::io::{Read, Write};

use obscura::Browser;

fn spawn_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = "<!doctype html><html><head><title>fixture</title></head><body>hi</body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body,
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    format!("http://{}", addr)
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_browser_can_be_dropped_without_aborting() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let first = Browser::new().unwrap();
    let mut page_one = first.new_page().await.unwrap();
    page_one.goto(&base).await.unwrap();

    let second = Browser::new().unwrap();
    let mut page_two = second.new_page().await.unwrap();
    page_two.goto(&base).await.unwrap();

    // Both isolates are live, and the first was entered first.
    drop(page_one);
    drop(first);

    assert_eq!(
        page_two.evaluate("document.title"),
        serde_json::json!("fixture"),
        "the surviving page should still work after its neighbour was dropped"
    );
}

/// The single-page case, which always worked: its isolate is the only one on
/// the stack, so it is necessarily dropped last.
#[tokio::test(flavor = "current_thread")]
async fn one_browser_one_page_drops_cleanly() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    assert_eq!(
        page.evaluate("document.title"),
        serde_json::json!("fixture")
    );
}

/// Two pages on ONE browser — one isolate each, same context.
#[tokio::test(flavor = "current_thread")]
async fn two_pages_on_one_browser_drop_cleanly() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();
    let browser = Browser::new().unwrap();
    let mut one = browser.new_page().await.unwrap();
    one.goto(&base).await.unwrap();
    let mut two = browser.new_page().await.unwrap();
    two.goto(&base).await.unwrap();
    drop(one);
    assert_eq!(two.evaluate("document.title"), serde_json::json!("fixture"));
}

/// Dropping from the *middle* of the stack, which needs the isolates entered
/// after it to be unwound and then put back, not just its own entered.
#[tokio::test(flavor = "current_thread")]
async fn a_page_in_the_middle_can_be_dropped_and_the_rest_keep_working() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();
    let browser = Browser::new().unwrap();

    let mut first = browser.new_page().await.unwrap();
    first.goto(&base).await.unwrap();
    let mut middle = browser.new_page().await.unwrap();
    middle.goto(&base).await.unwrap();
    let mut last = browser.new_page().await.unwrap();
    last.goto(&base).await.unwrap();

    drop(middle);

    // Both neighbours must still be able to run script: the one below the
    // hole on the entry stack and the one above it, which was exited and
    // re-entered to reach it.
    assert_eq!(
        first.evaluate("document.title"),
        serde_json::json!("fixture")
    );
    assert_eq!(
        last.evaluate("document.title"),
        serde_json::json!("fixture")
    );

    // And a page created after the hole works, so the stack was left in a
    // state a new isolate can be pushed onto.
    let mut fresh = browser.new_page().await.unwrap();
    fresh.goto(&base).await.unwrap();
    assert_eq!(
        fresh.evaluate("document.title"),
        serde_json::json!("fixture")
    );

    // Finally drop out of order again, oldest first, and keep using the rest.
    drop(first);
    assert_eq!(
        last.evaluate("document.title"),
        serde_json::json!("fixture")
    );
    assert_eq!(
        fresh.evaluate("document.title"),
        serde_json::json!("fixture")
    );
}

/// The half of this that has nothing to do with dropping: a second page put
/// the first one's isolate below the top of V8's entry stack, and running any
/// script on a non-current isolate aborts. A second page therefore disabled
/// the first one outright, with no teardown involved at all.
#[tokio::test(flavor = "current_thread")]
async fn an_older_page_still_runs_script_while_a_newer_one_is_alive() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();
    let browser = Browser::new().unwrap();

    let mut first = browser.new_page().await.unwrap();
    first.goto(&base).await.unwrap();
    let mut second = browser.new_page().await.unwrap();
    second.goto(&base).await.unwrap();

    // The newest page always worked; the older one is what aborted.
    assert_eq!(
        second.evaluate("document.title"),
        serde_json::json!("fixture")
    );
    assert_eq!(
        first.evaluate("document.title"),
        serde_json::json!("fixture")
    );

    // Interleaving the two must keep working in both directions.
    assert_eq!(first.evaluate("1 + 1"), serde_json::json!(2.0));
    assert_eq!(second.evaluate("2 + 2"), serde_json::json!(4.0));
    assert_eq!(first.evaluate("3 + 3"), serde_json::json!(6.0));
}
