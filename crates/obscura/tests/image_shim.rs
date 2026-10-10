//! Regression test for issue #394 (crash half): `new Image()` must survive a
//! page that pre-defines a non-configurable own `src` on `<img>` elements, the
//! way Booking.com's anti-bot instrumentation does. The Image shim used to
//! unconditionally redefine `src` on the element it just created, throwing
//! `TypeError: Cannot redefine property: src`.

use std::io::{Read, Write};

use obscura::Browser;

/// Minimal HTTP/1.1 server returning the test page and a valid 1x1 PNG.
fn spawn_server(html: &'static str) -> String {
    const PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
        0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
        0xda, 0x63, 0xfc, 0xcf, 0xc0, 0x50, 0x0f, 0x00, 0x05, 0x83, 0x02, 0x7f, 0x94, 0xff,
        0x2f, 0x59, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut s = match incoming {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut buf = [0u8; 2048];
            let read = s.read(&mut buf).unwrap_or(0);
            let is_pixel = buf[..read].starts_with(b"GET /pixel.png ");
            let (content_type, body) = if is_pixel {
                ("image/png", PIXEL_PNG)
            } else {
                ("text/html", html.as_bytes())
            };
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(headers.as_bytes());
            let _ = s.write_all(body);
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

#[cfg(feature = "render")]
#[tokio::test]
async fn invalid_image_bytes_emit_error_like_chromium() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server(
        r#"<!doctype html><html><body><div id="r">waiting</div>
<script>
  var img = new Image();
  img.onerror = function () {
    document.getElementById('r').textContent =
      'error complete=' + img.complete +
      ' natural=' + img.naturalWidth + 'x' + img.naturalHeight;
  };
  img.src = '/broken.png';
</script>
</body></html>"#,
    );

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();
    for _ in 0..10 {
        page.settle(500).await;
        let text = page.evaluate("document.getElementById('r').textContent");
        if text.as_str().unwrap_or("").starts_with("error") {
            break;
        }
    }

    let text = page.evaluate("document.getElementById('r').textContent");
    assert_eq!(
        text.as_str().unwrap_or(""),
        "error complete=true natural=0x0",
        "invalid image bytes must fail instead of being treated as a decoded image"
    );
}
