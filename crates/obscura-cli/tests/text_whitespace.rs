use std::process::Command;

const INLINE_TEXT_URL: &str = "data:text/html,\
<html><body>\
<h1><span>H</span><span>e</span><span>l</span><span>l</span><span>o</span>%20\
<span>w</span><span>o</span><span>r</span><span>l</span><span>d</span><span>.</span></h1>\
<p><span>Hello</span><span>,</span>%20<span>world</span><span>!</span></p>\
</body></html>";

#[test]
fn dump_text_preserves_whitespace_between_inline_spans() {
    let output = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args([
            "fetch",
            INLINE_TEXT_URL,
            "--dump",
            "text",
            "--quiet",
        ])
        .output()
        .expect("run obscura fetch");

    assert!(
        output.status.success(),
        "obscura fetch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("UTF-8 text dump")
            .trim(),
        "Hello world.\n\nHello, world!",
    );
}
