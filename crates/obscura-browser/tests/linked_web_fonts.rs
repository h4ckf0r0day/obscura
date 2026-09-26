#![cfg(feature = "render")]

use std::io::{Read, Write};
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};

/// Serves `routes` (path, content type, body). With `cors`, responses carry
/// `Access-Control-Allow-Origin: *`, as font CDNs do.
fn serve(routes: Vec<(&'static str, &'static str, Vec<u8>)>, cors: bool) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let Some((_, content_type, body)) = routes.iter().find(|(p, _, _)| *p == path) else {
                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            };
            let cors_header = if cors { "Access-Control-Allow-Origin: *\r\n" } else { "" };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{cors_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(body);
        }
    });
    format!("http://{address}")
}

const MONO: &[u8] = include_bytes!("../../obscura-render/assets/liberation-mono.ttf");

async fn probe_width(font_origin_cors: bool, cross_origin: bool) -> f64 {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let css = b"@font-face { font-family: 'Probe'; src: url(probe.ttf) format('truetype'); }".to_vec();
    let fonts = serve(
        vec![("/fonts.css", "text/css", css), ("/probe.ttf", "font/ttf", MONO.to_vec())],
        font_origin_cors,
    );
    let sheet = if cross_origin { format!("{fonts}/fonts.css") } else { "/fonts.css".to_string() };
    let html = format!(
        "<html><head><link rel=\"stylesheet\" href=\"{sheet}\"></head><body style=\"margin:0\">\
         <span id=\"p\" style=\"font:40px Probe, serif\">iiiiii</span></body></html>"
    );
    // The page's own server also carries the font files for the same-origin
    // case; the cross-origin case links the other server's sheet instead.
    let css = b"@font-face { font-family: 'Probe'; src: url(probe.ttf) format('truetype'); }".to_vec();
    let page_origin = serve(
        vec![
            ("/", "text/html", html.into_bytes()),
            ("/fonts.css", "text/css", css),
            ("/probe.ttf", "font/ttf", MONO.to_vec()),
        ],
        false,
    );
    let ctx = Arc::new(BrowserContext::with_storage_and_network("fonts".into(), None, false, None, None, true));
    let mut page = Page::new("fonts-page".into(), ctx);
    page.navigate(&format!("{page_origin}/")).await.unwrap();
    page.evaluate("document.getElementById('p').getBoundingClientRect().width")
        .as_f64()
        .unwrap()
}

async fn fallback_width() -> f64 {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let html = b"<html><body style=\"margin:0\"><span id=\"p\" style=\"font:40px serif\">iiiiii</span></body></html>".to_vec();
    let origin = serve(vec![("/", "text/html", html)], false);
    let ctx = Arc::new(BrowserContext::with_storage_and_network("fonts".into(), None, false, None, None, true));
    let mut page = Page::new("fallback-page".into(), ctx);
    page.navigate(&format!("{origin}/")).await.unwrap();
    page.evaluate("document.getElementById('p').getBoundingClientRect().width")
        .as_f64()
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn linked_stylesheet_web_fonts_are_applied() {
    let fallback = fallback_width().await;
    // Liberation Mono's "i" is far wider than the serif fallback's.
    let same_origin = probe_width(false, false).await;
    assert!(same_origin > fallback * 1.5, "same-origin <link> font: {same_origin} vs fallback {fallback}");
    let cross_origin = probe_width(true, true).await;
    assert!(cross_origin > fallback * 1.5, "cross-origin font with CORS: {cross_origin} vs fallback {fallback}");
    // Browsers refuse a cross-origin font without CORS; so do we.
    let no_cors = probe_width(false, true).await;
    assert_eq!(no_cors, fallback, "cross-origin font without CORS must not load");
}
