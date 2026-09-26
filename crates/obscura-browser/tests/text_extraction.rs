use std::io::{Read, Write};
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use serde_json::json;

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
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{address}/")
}

async fn page_for(body: &'static str) -> Page {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let ctx = Arc::new(BrowserContext::with_storage_and_network("text".into(), None, false, None, None, true));
    let mut page = Page::new("text-page".into(), ctx);
    page.navigate(&serve(body)).await.unwrap();
    page
}

#[tokio::test(flavor = "current_thread")]
async fn inner_text_follows_rendered_text_rules() {
    let mut page = page_for(concat!(
        "<html><body>",
        "<h1>Title</h1><p>Para   with\n spaces</p><script>var secret = 1;</script>",
        "<div hidden>also hidden</div>",
        "<div>A <b>bold</b> <span style=\"display:none\">x</span>word</div>",
        "<ul><li>one</li><li>two</li></ul>",
        "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>",
        "<pre>  keep\n  this</pre>end<br>line</body></html>"
    ))
    .await;
    assert_eq!(
        page.evaluate("document.body.innerText"),
        json!("Title\n\nPara with spaces\n\nA bold word\none\ntwo\na\tb\nc\td\n  keep\n  this\nend\nline")
    );
    // An element that is not rendered reports its text content.
    assert_eq!(page.evaluate("document.querySelector('script').innerText"), json!("var secret = 1;"));
    assert_eq!(page.evaluate("document.querySelector('h1').outerText"), json!("Title"));
    // Setting innerText turns line breaks into <br>.
    assert_eq!(
        page.evaluate("(function(){ const d = document.createElement('div'); d.innerText = 'x\\ny'; return d.innerHTML; })()"),
        json!("x<br>y")
    );
}

/// Stylesheet rules need the render cascade; other builds see only markup.
#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn inner_text_skips_elements_hidden_by_stylesheets() {
    let mut page = page_for(concat!(
        "<html><head><style>.gone{display:none} .ghost{visibility:hidden}</style></head><body>",
        "<div>shown</div><div class=\"gone\">hidden</div><div class=\"ghost\">invisible</div>",
        "<div style=\"white-space:pre\">a  b</div></body></html>"
    ))
    .await;
    assert_eq!(page.evaluate("document.body.innerText"), json!("shown\na  b"));
}

#[tokio::test(flavor = "current_thread")]
async fn markdown_tables_are_valid_gfm() {
    let mut page = page_for(concat!(
        "<html><body><table>\n",
        "<caption style=\"caption-side:bottom\">Table 1: totals</caption>\n",
        "<thead><tr><th>Item</th><th>Qty</th></tr></thead>\n",
        "<tbody><tr><td>Apples | red</td><td>3</td></tr>\n",
        "<tr><td colspan=\"2\">wide</td></tr></tbody></table><p>after</p></body></html>"
    ))
    .await;
    let md = page.evaluate(obscura_browser::HTML_TO_MARKDOWN_JS);
    assert_eq!(
        md,
        json!("| Item | Qty |\n|---|---|\n| Apples \\| red | 3 |\n| wide |  |\n\nTable 1: totals\n\nafter")
    );

    let mut page = page_for("<html><body><table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table></body></html>").await;
    // No header row: the first row becomes the header, short rows are padded.
    assert_eq!(
        page.evaluate(obscura_browser::HTML_TO_MARKDOWN_JS),
        json!("| a | b |\n|---|---|\n| c |  |")
    );
}
