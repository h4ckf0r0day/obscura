//! SEC-007 / #583 — `Element::attribute()` must escape the attribute name
//! before interpolating it into page JS. A name containing a quote must not be
//! able to break out of the `getAttribute('{name}')` string literal and run
//! arbitrary JS in the page, the same guarantee `Page::query_selector` already
//! gives for selectors.

use obscura::Browser;

#[tokio::test]
async fn attribute_name_cannot_inject_js() {
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto("data:text/html,<div id=x data-safe=ok></div>")
        .await
        .unwrap();

    // Canary the injection would flip from 0 to 1.
    page.evaluate("globalThis.__pwned = 0");

    let el = page.query_selector("#x").expect("element #x should be found");

    // Payload breaks out of `getAttribute('{name}')` while staying a single
    // valid expression (the wrapper places it inside `el ? ... : null`, which
    // forbids a top-level comma). String concatenation carries the assignment:
    //   el.getAttribute('x' + (globalThis.__pwned = 1) + '')
    let _ = el.attribute("x' + (globalThis.__pwned = 1) + '");

    let pwned = page.evaluate("globalThis.__pwned");
    assert_ne!(
        pwned.as_f64(),
        Some(1.0),
        "attribute() must not execute JS injected via the attribute name (got {pwned:?})"
    );
}

#[tokio::test]
async fn attribute_reads_ordinary_names() {
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto("data:text/html,<div id=x data-safe=ok></div>")
        .await
        .unwrap();

    let el = page.query_selector("#x").expect("element #x should be found");
    assert_eq!(
        el.attribute("data-safe").as_deref(),
        Some("ok"),
        "escaping must not break reading a normal attribute name"
    );
}
