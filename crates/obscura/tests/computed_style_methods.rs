// Regression for issue #635: methods on the object returned by
// getComputedStyle() must not throw. The computed proxy used to hand back
// CSSStyleDeclaration methods unbound, so they ran _pull() with `this` bound
// to the proxy, where the declaration's own fields (_loaded, _owner) miss and
// fall through to the CSS-name lookup as "". The result was
// "TypeError: this._owner.getAttribute is not a function" on every method
// call. jQuery's curCSS reads computed.getPropertyValue(name), so every
// $().css() read and .show() failed on real sites.

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
<div id="inline" style="color: red">inline-styled</div>
<div id="plain">plain</div>
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
async fn computed_style_methods_do_not_throw() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            function attempt(fn) {
                try { return String(fn()); } catch (e) { return 'threw: ' + e.message; }
            }
            var inline = document.getElementById('inline');
            var plain = document.getElementById('plain');
            return {
                inline_color: attempt(function () { return getComputedStyle(inline).getPropertyValue('color'); }),
                default_display: attempt(function () { return getComputedStyle(plain).getPropertyValue('display'); }),
                item: attempt(function () { return getComputedStyle(inline).item(0); }),
                length: attempt(function () { return getComputedStyle(inline).length; }),
                css_text: attempt(function () { return getComputedStyle(inline).cssText; }),
                set_property: attempt(function () { getComputedStyle(inline).setProperty('color', 'blue'); return 'ok'; }),
                remove_property: attempt(function () { return getComputedStyle(inline).removeProperty('color'); }),
            };
        })()"#,
    );

    let get = |key: &str| {
        probes[key]
            .as_str()
            .unwrap_or_else(|| panic!("probe {key} missing in {probes}"))
            .to_string()
    };

    // The jQuery curCSS path: inline value first, engine default as fallback.
    assert_eq!(get("inline_color"), "red");
    assert_eq!(get("default_display"), "block");

    // No method reachable through the computed proxy may hit the unbound-this
    // crash, whatever else it returns.
    for key in [
        "inline_color",
        "default_display",
        "item",
        "length",
        "css_text",
        "set_property",
        "remove_property",
    ] {
        let value = get(key);
        assert!(
            !value.contains("_owner") && !value.contains("is not a function"),
            "computed style {key} hit the unbound-this crash: {value}"
        );
    }
}
