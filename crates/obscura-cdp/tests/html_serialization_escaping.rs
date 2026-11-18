//! Markup read back from the engine must parse to the same tree it was
//! written from. `<textarea>` and `<title>` text is escapable: the parser
//! decodes character references inside them, so a page that correctly sent
//! `&lt;/title&gt;&lt;script&gt;` used to come back out of `outerHTML`,
//! `DOM.getOuterHTML` and `--dump html` with a real `<script>` element.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

/// A page whose author escaped everything correctly.
const PAGE: &str = "<title>t &lt;/title&gt;&lt;script&gt;window.pwned=1&lt;/script&gt;</title>\
<textarea id=ta>u &lt;/textarea&gt;&lt;img src=x onerror=window.pwned=2&gt;</textarea>\
<svg><style id=st>&lt;img src=x onerror=window.pwned=3&gt;</style></svg>\
<a id=a title='&quot;&gt;&lt;b&gt; x&nbsp;y'>z&nbsp;w</a>\
<script>var raw = 1 < 2 && 3 > 2;</script><style>a > b { color: red }</style>";

async fn open(session_id: &str) -> CdpContext {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert(session_id.to_string(), page_id);
    let nav = dispatch(&CdpRequest {
        id: 1, method: "Page.navigate".to_string(),
        params: json!({"url": format!("data:text/html,{PAGE}"), "waitUntil": "load"}),
        session_id: Some(session_id.to_string()),
    }, &mut ctx).await;
    assert!(nav.error.is_none(), "{:?}", nav.error);
    ctx
}

async fn call(ctx: &mut CdpContext, session_id: &str, method: &str, params: Value) -> Value {
    let resp = dispatch(&CdpRequest {
        id: 2, method: method.to_string(), params, session_id: Some(session_id.to_string()),
    }, ctx).await;
    assert!(resp.error.is_none(), "{method}: {:?}", resp.error);
    resp.result.unwrap()
}

async fn evaluate(expression: &str) -> Value {
    let session = "serialize";
    let mut ctx = open(session).await;
    let result = call(&mut ctx, session, "Runtime.evaluate",
        json!({"expression": expression, "returnByValue": true})).await;
    assert!(result.get("exceptionDetails").is_none(), "{result}");
    result["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn escaped_page_text_survives_a_serialize_and_reparse_round_trip() {
    assert_eq!(evaluate(r#"(() => {
        const html = document.documentElement.outerHTML;
        const again = new DOMParser().parseFromString(html, 'text/html');
        return {
            title: html.includes('<title>t &lt;/title&gt;&lt;script&gt;window.pwned=1&lt;/script&gt;</title>'),
            textarea: document.getElementById('ta').outerHTML,
            foreignStyle: document.getElementById('st').outerHTML,
            scripts: again.scripts.length, images: again.images.length,
            sameTitle: again.title === document.title,
            sameValue: again.getElementById('ta').value === document.getElementById('ta').value,
        };
    })()"#).await, json!({
        "title": true,
        "textarea": "<textarea id=\"ta\">u &lt;/textarea&gt;&lt;img src=x onerror=window.pwned=2&gt;</textarea>",
        "foreignStyle": "<style id=\"st\">&lt;img src=x onerror=window.pwned=3&gt;</style>",
        "scripts": 1, "images": 0, "sameTitle": true, "sameValue": true,
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn inner_html_assignment_of_serialized_markup_creates_no_new_elements() {
    assert_eq!(evaluate(r#"(() => {
        const box = document.createElement('div');
        document.body.append(box);
        box.innerHTML = document.getElementById('ta').outerHTML
            + document.querySelector('svg').outerHTML + document.head.innerHTML;
        return [box.querySelectorAll('img').length, box.querySelectorAll('script').length,
            window.pwned === undefined];
    })()"#).await, json!([0, 0, true]));
}

#[tokio::test(flavor = "current_thread")]
async fn attributes_text_and_raw_text_follow_the_standard_escaping_rules() {
    assert_eq!(evaluate(r#"(() => [
        document.getElementById('a').outerHTML,
        document.querySelector('script').outerHTML,
        document.querySelector('head style, body > style').outerHTML,
    ])()"#).await, json!([
        "<a id=\"a\" title=\"&quot;&gt;&lt;b&gt; x&nbsp;y\">z&nbsp;w</a>",
        "<script>var raw = 1 < 2 && 3 > 2;</script>",
        "<style>a > b { color: red }</style>",
    ]));
}

#[tokio::test(flavor = "current_thread")]
async fn elements_that_serialize_as_void_have_no_inner_markup() {
    assert_eq!(evaluate(r#"(() => ['br', 'img', 'basefont', 'bgsound', 'frame', 'keygen', 'param'].map(tag => {
        const el = document.createElement(tag);
        el.append(document.createElement('b'), 'text');
        const span = document.createElement('span');
        span.append(el, 'after');
        return el.innerHTML === '' && el.outerHTML === `<${tag}>` && span.innerHTML === `<${tag}>after`;
    }))()"#).await, json!([true, true, true, true, true, true, true]));
}

#[tokio::test(flavor = "current_thread")]
async fn dom_get_outer_html_keeps_the_page_escaped() {
    let session = "serialize-cdp";
    let mut ctx = open(session).await;
    let document = call(&mut ctx, session, "DOM.getDocument", json!({"depth": 0})).await;
    let node_id = document["root"]["nodeId"].clone();
    let outer = call(&mut ctx, session, "DOM.getOuterHTML", json!({"nodeId": node_id})).await;
    let html = outer["outerHTML"].as_str().unwrap();
    assert!(html.contains("&lt;/title&gt;&lt;script&gt;window.pwned=1"), "{html}");
    assert!(html.contains("&lt;/textarea&gt;&lt;img src=x"), "{html}");
    assert!(!html.contains("</title><script>") && !html.contains("</textarea><img"), "{html}");
}
