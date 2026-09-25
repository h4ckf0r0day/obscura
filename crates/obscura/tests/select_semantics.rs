// Select-element parity needed by jQuery and WooCommerce variation forms:
//
// - A single select with no explicit `selected` attribute implicitly selects
//   its first option, so selectedIndex is 0, not -1 (jQuery reads selects
//   through selectedIndex; -1 made $(select).val() return null).
// - select.type is the fixed IDL string "select-one"/"select-multiple"
//   (jQuery's valHook branches on it; "" made single selects read as arrays).
// - Programmatic value assignment must not fire change. Dispatching it fed
//   pages that assign inside a change handler back into that handler forever
//   (WooCommerce's variation form hit the stack limit).

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
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request);
                let body = r#"<!doctype html><html><head><title>fixture</title></head><body>
<select id="single"><option value="a">A</option><option value="b">B</option></select>
<select id="multi" multiple><option value="x">X</option></select>
<select id="empty"></select>
</body></html>"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

#[tokio::test(flavor = "current_thread")]
async fn select_defaults_match_browser_semantics() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            var single = document.getElementById('single');
            var multi = document.getElementById('multi');
            var empty = document.getElementById('empty');
            var changes = 0;
            single.addEventListener('change', function () { changes++; });
            single.value = 'b';
            return {
                single_index: single.selectedIndex,
                single_value_after_set: single.value,
                multi_index: multi.selectedIndex,
                empty_index: empty.selectedIndex,
                single_type: single.type,
                multi_type: multi.type,
                changes_after_assignment: changes,
            };
        })()"#,
    );

    assert_eq!(probes["single_value_after_set"], "b");
    assert_eq!(probes["single_index"], 1);
    assert_eq!(probes["multi_index"], -1);
    assert_eq!(probes["empty_index"], -1);
    assert_eq!(probes["single_type"], "select-one");
    assert_eq!(probes["multi_type"], "select-multiple");
    assert_eq!(probes["changes_after_assignment"], 0);
}

#[tokio::test(flavor = "current_thread")]
async fn select_without_explicit_selection_defaults_to_first_option() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            var single = document.getElementById('single');
            return { index: single.selectedIndex, value: single.value };
        })()"#,
    );

    assert_eq!(probes["index"], 0);
    assert_eq!(probes["value"], "a");
}
