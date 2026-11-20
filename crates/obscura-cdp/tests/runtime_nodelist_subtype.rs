use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value, session: Option<&str>) -> Value {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: session.map(str::to_string),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "CDP {method} failed: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

fn assert_node_list(remote: &Value, length: usize) {
    assert_eq!(remote["type"], "object", "{remote}");
    assert_eq!(remote["subtype"], "array", "{remote}");
    assert_eq!(remote["className"], "NodeList", "{remote}");
    assert_eq!(remote["description"], format!("NodeList({length})"), "{remote}");
    assert!(remote["objectId"].as_str().is_some(), "{remote}");
}

// Chrome reports a NodeList handle with subtype "array". go-rod's
// Page.Elements requires that subtype before it lists the items with
// Runtime.getProperties, so without it every element list fails.
#[tokio::test(flavor = "current_thread")]
async fn node_list_handles_report_the_array_subtype() {
    let mut ctx = CdpContext::new();
    let created = cdp(&mut ctx, "Target.createTarget", json!({"url": "about:blank"}), None).await;
    let attached = cdp(
        &mut ctx,
        "Target.attachToTarget",
        json!({"targetId": created["targetId"], "flatten": true}),
        None,
    )
    .await;
    let session = attached["sessionId"].as_str().unwrap().to_string();
    let session = Some(session.as_str());
    cdp(&mut ctx, "Runtime.enable", json!({}), session).await;
    ctx.pending_events.clear();

    let url = concat!(
        "data:text/html,<a href='/f'>one</a><a href='/g'>two</a>",
        "<script>console.log(document.querySelectorAll('a'));</script>"
    );
    cdp(&mut ctx, "Page.navigate", json!({"url": url, "waitUntil": "load"}), session).await;

    let logged = ctx
        .pending_events
        .iter()
        .find(|event| event.method == "Runtime.consoleAPICalled")
        .expect("missing console.log event")
        .params["args"][0]
        .clone();
    assert_node_list(&logged, 2);

    let evaluated = cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "document.querySelectorAll('a')"}),
        session,
    )
    .await;
    assert_node_list(&evaluated["result"], 2);

    let empty = cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "document.querySelectorAll('missing')"}),
        session,
    )
    .await;
    assert_node_list(&empty["result"], 0);

    let body = cdp(&mut ctx, "Runtime.evaluate", json!({"expression": "document.body"}), session).await;
    let called = cdp(
        &mut ctx,
        "Runtime.callFunctionOn",
        json!({
            "functionDeclaration": "function() { return this.childNodes; }",
            "objectId": body["result"]["objectId"],
        }),
        session,
    )
    .await;
    assert_eq!(called["result"]["subtype"], "array", "{called}");
    assert_eq!(called["result"]["className"], "NodeList", "{called}");

    // The go-rod flow: every indexed property of the list is a node handle.
    let properties = cdp(
        &mut ctx,
        "Runtime.getProperties",
        json!({"objectId": evaluated["result"]["objectId"], "ownProperties": true}),
        session,
    )
    .await;
    let items: Vec<&Value> = properties["result"]
        .as_array()
        .expect("getProperties returned no result")
        .iter()
        .filter(|property| property["name"] != "length")
        .collect();
    assert_eq!(items.len(), 2, "{properties}");
    for item in items {
        assert_eq!(item["value"]["subtype"], "node", "{item}");
    }

    // A NodeList reached as a property of another handle is an array too.
    let holder = cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "({ links: document.querySelectorAll('a') })"}),
        session,
    )
    .await;
    let properties = cdp(
        &mut ctx,
        "Runtime.getProperties",
        json!({"objectId": holder["result"]["objectId"], "ownProperties": true}),
        session,
    )
    .await;
    assert_node_list(&properties["result"][0]["value"], 2);

    // Other objects keep their shape: a plain array-like is not an array.
    let array_like = cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "({ 0: 'a', length: 1 })"}),
        session,
    )
    .await;
    assert!(array_like["result"].get("subtype").is_none(), "{array_like}");
}
