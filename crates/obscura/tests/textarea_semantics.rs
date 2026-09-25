// Textarea parity from #685: the element kept its parser handling and value
// semantics but had no interface of its own and no control geometry, so it
// laid out as a plain block (full containing-block width, zero height when
// empty). Playwright resolves that zero-height box to `hidden`, and
// wait_for_selector/click/fill then burn their full timeout. Both halves are
// covered here:
//
// - DOM: rows/cols reflect their attributes with the HTML defaults (2/20),
//   type is the fixed string "textarea", and the wrapper is a real
//   HTMLTextAreaElement (constructor.name, instanceof) instead of Element.
// - Layout: an empty textarea keeps an intrinsic box from rows/cols
//   (Chromium: cols=20 rows=2 -> 168x36, rows=8 -> 126 tall), is
//   inline-block, and an authored height still wins.

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
<textarea id="t"></textarea>
<textarea id="r" rows="8"></textarea>
<textarea id="c" style="height:36px"></textarea>
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
async fn textarea_idl_matches_browser_semantics() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            var t = document.getElementById('t');
            return {
                rows_default: t.rows,
                cols_default: t.cols,
                type: t.type,
                ctor: t.constructor.name,
                is_textarea: t instanceof HTMLTextAreaElement,
                is_element: t instanceof Element,
                rows_reflect: (t.rows = 5, t.getAttribute('rows')),
                rows_after_set: t.rows,
                rows_invalid: (t.setAttribute('rows', '0'), t.rows),
            };
        })()"#,
    );

    assert_eq!(probes["rows_default"], 2);
    assert_eq!(probes["cols_default"], 20);
    assert_eq!(probes["type"], "textarea");
    assert_eq!(probes["ctor"], "HTMLTextAreaElement");
    assert_eq!(probes["is_textarea"], true);
    assert_eq!(probes["is_element"], true);
    assert_eq!(probes["rows_reflect"], "5");
    assert_eq!(probes["rows_after_set"], 5);
    // rows/cols are limited to positive numbers; anything else reads back
    // as the default.
    assert_eq!(probes["rows_invalid"], 2);
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn textarea_gets_intrinsic_control_box() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            var box = function (id) {
                var r = document.getElementById(id).getBoundingClientRect();
                return r.width.toFixed(0) + 'x' + r.height.toFixed(0);
            };
            var t = document.getElementById('t');
            return {
                empty: box('t'),
                rows8: box('r'),
                css_height: box('c'),
                display: getComputedStyle(t).display,
                value_roundtrip: (t.value = 'hi', t.value),
            };
        })()"#,
    );

    // Chromium reference: an empty cols=20 rows=2 control is 168x36; each
    // extra row adds one 15px control line; authored height wins.
    assert_eq!(probes["empty"], "168x36");
    assert_eq!(probes["rows8"], "168x126");
    assert_eq!(probes["css_height"], "168x36");
    assert_eq!(probes["display"], "inline-block");
    assert_eq!(probes["value_roundtrip"], "hi");
}
