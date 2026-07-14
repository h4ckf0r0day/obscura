//! Regression test for issue #394 (crash half): `new Image()` must survive a
//! page that pre-defines a non-configurable own `src` on `<img>` elements, the
//! way Booking.com's anti-bot instrumentation does. The Image shim used to
//! unconditionally redefine `src` on the element it just created, throwing
//! `TypeError: Cannot redefine property: src`.

use std::io::{Read, Write};

use obscura::Browser;

/// Minimal HTTP/1.1 server returning HTML that wraps document.createElement to
/// plant a non-configurable `src` on every <img>, then calls `new Image()`.
fn spawn_server(html: &'static str) -> String {
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
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn new_image_survives_non_configurable_src() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(
        r#"<!doctype html><html><body><div id="r">waiting</div>
<script>
  var origCreate = document.createElement.bind(document);
  document.createElement = function (tag) {
    var el = origCreate(tag);
    if (String(tag).toLowerCase() === 'img') {
      Object.defineProperty(el, 'src', { value: '', writable: true, configurable: false });
    }
    return el;
  };
  var img = new Image(10, 20);
  document.getElementById('r').textContent =
    'survived w=' + img.width + ' h=' + img.height;
</script>
</body></html>"#,
    );

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let text = page.evaluate("document.getElementById('r').textContent");
    assert_eq!(
        text.as_str().unwrap_or(""),
        "survived w=10 h=20",
        "new Image() threw instead of degrading when src is non-configurable"
    );
}

#[tokio::test]
async fn new_image_still_emulates_load_when_src_is_configurable() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(
        r#"<!doctype html><html><body><div id="r">waiting</div>
<script>
  var img = new Image();
  img.onload = function () {
    document.getElementById('r').textContent = 'loaded complete=' + img.complete;
  };
  img.src = '/pixel.png';
</script>
</body></html>"#,
    );

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    // The shim fires `load` on a setTimeout(0); pump the event loop.
    for _ in 0..10 {
        page.settle(500).await;
        let text = page.evaluate("document.getElementById('r').textContent");
        if text.as_str().unwrap_or("").starts_with("loaded") {
            break;
        }
    }

    let text = page.evaluate("document.getElementById('r').textContent");
    assert_eq!(
        text.as_str().unwrap_or(""),
        "loaded complete=true",
        "load emulation regressed for the normal (configurable src) path"
    );
}
