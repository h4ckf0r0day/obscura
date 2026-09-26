#![cfg(feature = "render")]

use std::io::{Read, Write};
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page, RasterPdfOptions};

fn serve(body: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{address}/")
}

fn utf16_hex(text: &str) -> String {
    text.encode_utf16().map(|unit| format!("{unit:04X}")).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn printed_pages_carry_text_headers_and_an_outline() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve(concat!(
        "<!doctype html><html><head><title>Report</title></head>",
        "<body style=\"margin:0;font:16px sans-serif\">",
        "<h1>Summary</h1><p>Quarterly r\u{e9}sum\u{e9} for the team.</p>",
        "<h2>Numbers</h2><p style=\"height:2000px\">Growth</p>",
        "</body></html>"
    ));
    let ctx = Arc::new(BrowserContext::with_storage_and_network("pdf".into(), None, false, None, None, true));
    let mut page = Page::new("pdf-page".into(), ctx);
    page.set_viewport((1280.0, 720.0));
    page.navigate(&url).await.unwrap();

    let pdf = page
        .raster_pdf(RasterPdfOptions {
            display_header_footer: true,
            footer_template: "<div style=\"font-size:10px\"><span class=\"pageNumber\"></span>/<span class=\"totalPages\"></span></div>".into(),
            generate_document_outline: true,
            ..RasterPdfOptions::default()
        })
        .expect("PDF export");
    let raw = String::from_utf8_lossy(&pdf);

    // Letter with 1cm margins: the print layout is ~740px wide, so the 2000px
    // paragraph runs onto a third page.
    let pages = raw.matches("/Type /Page ").count();
    assert!(pages >= 3, "{pages} pages");
    assert!(raw.contains(&utf16_hex("Quarterly")), "body text is in the text layer");
    assert!(raw.contains(&utf16_hex("r\u{e9}sum\u{e9}")), "accented text keeps its code points");
    assert!(raw.contains(&format!("(1/{pages}) Tj")), "footer carries page values");
    assert!(raw.contains("/Outlines") && raw.contains("/PageMode /UseOutlines"));
}
