//! `Runtime.getProperties` mints a child objectId as `parentOid + '::' + key`
//! and interpolates it straight back into a generated
//! `__obscura_objects['<oid>']` lookup on the next call. The key comes from the
//! page, so the page - not the client - decides which characters land inside
//! that single-quoted JS literal (issue #709).
//!
//! Escaping only `\` and `'` leaves every C0 control alone, and a raw newline
//! ends a JS string literal exactly as a stray quote does. The generated
//! snippet is then a syntax error, `page.evaluate` yields no array, and the
//! handler falls through to `result: []`. That is the part worth a regression
//! test: the call still answers without an error and with an empty property
//! list, so a client walking a nested object sees an object that has no
//! properties rather than a failure, and nothing anywhere reports one.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session_id: &str) -> Value {
    let resp = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

/// The `objectId` of the property descriptor named `name`.
fn child_object_id(props: &Value, name: &str) -> Option<String> {
    props
        .get("result")?
        .as_array()?
        .iter()
        .find(|d| d.get("name").and_then(|v| v.as_str()) == Some(name))
        .and_then(|d| d.pointer("/value/objectId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// The string value of the property descriptor named `name`.
fn string_property(props: &Value, name: &str) -> Option<String> {
    props
        .get("result")?
        .as_array()?
        .iter()
        .find(|d| d.get("name").and_then(|v| v.as_str()) == Some(name))
        .and_then(|d| d.pointer("/value/value"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// The keys are assembled with String.fromCharCode so this expression carries no
// backslash of its own: the characters under test are exactly the four below
// and not an artefact of how the literal was written. A page is free to define
// every one of them, and a client cannot sanitise them away - it never sees the
// key until getProperties has already built an objectId out of it.
const SETUP: &str = r#"(function () {
    var LF = String.fromCharCode(10);
    var CR = String.fromCharCode(13);
    var NUL = String.fromCharCode(0);
    var BACKSLASH = String.fromCharCode(92);
    var o = {};
    o["lf" + LF + "x"] = { mark: "lf" };
    o["cr" + CR + "x"] = { mark: "cr" };
    o["nul" + NUL + "x"] = { mark: "nul" };
    o["quote'" + BACKSLASH + "x"] = { mark: "quote" };
    globalThis.__t = o;
    return o;
})()"#;

#[tokio::test(flavor = "current_thread")]
async fn get_properties_walks_into_a_child_whose_key_holds_a_control_character() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": "about:blank", "waitUntil": "load"}),
        session_id,
    )
    .await;

    // returnByValue omitted, so the result is a handle rather than a copy.
    let root = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": SETUP}),
        session_id,
    )
    .await;
    let root_oid = root
        .pointer("/result/objectId")
        .and_then(|v| v.as_str())
        .expect("Runtime.evaluate must hand back an objectId for an object result")
        .to_string();

    let top = cdp(
        &mut ctx,
        3,
        "Runtime.getProperties",
        json!({"objectId": root_oid}),
        session_id,
    )
    .await;

    // The last pair is the case the hand-rolled escaping already covered. It is
    // here so a regression in either direction shows up in one run: a fix that
    // traded the quote handling for control-character handling would pass the
    // first three and fail this one.
    let cases = [
        ("lf\nx", "lf"),
        ("cr\rx", "cr"),
        ("nul\0x", "nul"),
        ("quote'\\x", "quote"),
    ];

    for (id, (key, mark)) in cases.iter().enumerate() {
        let child_oid = child_object_id(&top, key).unwrap_or_else(|| {
            panic!("no child objectId for key {key:?}; descriptors were {top:#?}")
        });

        let child = cdp(
            &mut ctx,
            10 + id as u64,
            "Runtime.getProperties",
            json!({"objectId": child_oid}),
            session_id,
        )
        .await;

        assert_eq!(
            string_property(&child, "mark").as_deref(),
            Some(*mark),
            "walking into the child keyed {key:?} must reach its properties, \
             not fall through to an empty list; got {child:#?}"
        );
    }
}

// The same page-minted objectId does not stay inside Runtime. Puppeteer's `$$`
// flow is evaluate -> getProperties -> asElement, and asElement handles go on to
// DOM.describeNode / DOM.resolveNode, which interpolate the id into the same kind
// of lookup. dom.rs used to do that through its own `escape_object_id` - the
// helper #709 cites as already robust - but that was the same two `replace` calls
// and so had the same hole. Here the fallback was worse than an empty list:
// describeNode's `unwrap_or(0)` answers with node 0, so the client was handed a
// description of the wrong element rather than a failure.
//
// Both call sites now share `util::object_id_literal`, which is what keeps this
// test and the one above from drifting apart.
const SETUP_NODES: &str = r#"(function () {
    var LF = String.fromCharCode(10);
    var o = {};
    o["plain"] = document.getElementById("a");
    o["lf" + LF + "x"] = document.getElementById("b");
    globalThis.__n = o;
    return o;
})()"#;

#[tokio::test(flavor = "current_thread")]
async fn describe_node_resolves_a_child_handle_whose_key_holds_a_control_character() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(
        &mut ctx,
        1,
        "Page.navigate",
        json!({"url": "data:text/html,<div id=a></div><p id=b></p>", "waitUntil": "load"}),
        session_id,
    )
    .await;

    let root = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": SETUP_NODES}),
        session_id,
    )
    .await;
    let root_oid = root
        .pointer("/result/objectId")
        .and_then(|v| v.as_str())
        .expect("Runtime.evaluate must hand back an objectId for an object result")
        .to_string();

    let top = cdp(
        &mut ctx,
        3,
        "Runtime.getProperties",
        json!({"objectId": root_oid}),
        session_id,
    )
    .await;

    for (id, (key, node_name)) in [("plain", "DIV"), ("lf\nx", "P")].iter().enumerate() {
        let child_oid = child_object_id(&top, key).unwrap_or_else(|| {
            panic!("no child objectId for key {key:?}; descriptors were {top:#?}")
        });

        let described = cdp(
            &mut ctx,
            20 + id as u64,
            "DOM.describeNode",
            json!({"objectId": child_oid}),
            session_id,
        )
        .await;

        assert_eq!(
            described.pointer("/node/nodeName").and_then(|v| v.as_str()),
            Some(*node_name),
            "the handle keyed {key:?} must describe its own element; got {described:#?}"
        );
    }
}
