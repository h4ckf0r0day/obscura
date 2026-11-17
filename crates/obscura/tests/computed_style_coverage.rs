// getComputedStyle coverage from #771. An audit of ~90 properties against
// Chromium 147 found 41 that returned the empty string and six that returned a
// wrong value. An empty string is indistinguishable from "not set" to a caller,
// so code that branches on one of these silently takes the wrong path instead
// of failing visibly.
//
// Everything the renderer already computes is now served from its snapshot;
// what it does not model at all falls back to the CSS initial value rather than
// ''. The expectations below are Chromium 147's own values, captured over CDP
// from the same markup.

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
                let body = r#"<!doctype html><html><head><title>fixture</title></head>
<body style="margin:0">
<div id="d">x</div>
<table id="t"><tr id="tr"><td id="td">c</td></tr></table>
<ul><li id="li">z</li></ul>
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

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn computed_style_never_returns_empty_for_supported_properties() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let base = spawn_server();

    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&base).await.unwrap();

    let probes = page.evaluate(
        r#"(function () {
            var cs = getComputedStyle(document.getElementById('d'));
            var out = {};
            var names = ['background-image','background-size','background-position',
              'background-repeat','font-style','word-spacing','text-decoration-line',
              'text-indent','list-style-position','border-spacing','caption-side',
              'align-self','flex-grow','flex-shrink','flex-basis','grid-column',
              'grid-row','transition-property','transition-duration','animation-name',
              'animation-duration','animation-iteration-count','animation-timing-function',
              'user-select','box-shadow','text-shadow','content','order','object-fit',
              'resize','appearance','filter','mix-blend-mode','writing-mode','direction',
              'unicode-bidi','isolation','contain','aspect-ratio','zoom','min-width',
              'min-height','outline-width','caret-color','object-position','justify-self',
              'list-style-image','empty-cells'];
            for (var i = 0; i < names.length; i++) out[names[i]] = cs.getPropertyValue(names[i]);
            out['__display_table'] = getComputedStyle(document.getElementById('t')).display;
            out['__display_row'] = getComputedStyle(document.getElementById('tr')).display;
            out['__display_cell'] = getComputedStyle(document.getElementById('td')).display;
            out['__display_li'] = getComputedStyle(document.getElementById('li')).display;
            out['__table_flex_dir'] = getComputedStyle(document.getElementById('t')).flexDirection;
            out['__table_align'] = getComputedStyle(document.getElementById('t')).alignItems;
            return out;
        })()"#,
    );

    // Chromium 147's value for each, on a plain <div> with nothing set.
    for (name, expected) in [
        ("background-image", "none"),
        ("background-size", "auto"),
        ("background-position", "0% 0%"),
        ("background-repeat", "repeat"),
        ("font-style", "normal"),
        ("word-spacing", "0px"),
        ("text-decoration-line", "none"),
        ("text-indent", "0px"),
        ("list-style-position", "outside"),
        ("caption-side", "top"),
        ("align-self", "auto"),
        ("flex-grow", "0"),
        ("flex-shrink", "1"),
        ("flex-basis", "auto"),
        ("grid-column", "auto"),
        ("grid-row", "auto"),
        ("transition-property", "all"),
        ("transition-duration", "0s"),
        ("animation-name", "none"),
        ("animation-duration", "0s"),
        ("animation-iteration-count", "1"),
        ("animation-timing-function", "ease"),
        ("user-select", "auto"),
        ("box-shadow", "none"),
        ("text-shadow", "none"),
        ("content", "normal"),
        ("order", "0"),
        ("object-fit", "fill"),
        ("object-position", "50% 50%"),
        ("resize", "none"),
        ("appearance", "none"),
        ("filter", "none"),
        ("mix-blend-mode", "normal"),
        ("writing-mode", "horizontal-tb"),
        ("direction", "ltr"),
        ("isolation", "auto"),
        ("contain", "none"),
        ("aspect-ratio", "auto"),
        ("zoom", "1"),
        ("justify-self", "auto"),
        ("list-style-image", "none"),
        ("empty-cells", "show"),
        ("caret-color", "rgb(0, 0, 0)"),
        // Previously wrong rather than empty.
        ("min-width", "0px"),
        ("min-height", "0px"),
        ("outline-width", "3px"),
    ] {
        assert_eq!(probes[name], expected, "getComputedStyle().{name}");
    }

    // `unicode-bidi` is the one property here whose Chromium value is
    // element-dependent (its UA sheet isolates block containers, so a <div>
    // reports `isolate` and a <span> `normal`). The CSS initial value is
    // reported for every element rather than an empty string.
    assert_eq!(probes["unicode-bidi"], "normal");

    // The layout model approximates tables with flex containers, so a table's
    // box type could not be read back at all and the internal column direction
    // leaked out as the computed value of a property nobody wrote.
    assert_eq!(probes["__display_table"], "table");
    assert_eq!(probes["__display_row"], "table-row");
    assert_eq!(probes["__display_cell"], "table-cell");
    assert_eq!(probes["__display_li"], "list-item");
    assert_eq!(probes["__table_flex_dir"], "row");
    assert_eq!(probes["__table_align"], "normal");
}
