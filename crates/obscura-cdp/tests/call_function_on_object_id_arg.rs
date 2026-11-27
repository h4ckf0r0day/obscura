use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn call_with_arguments(session_id: &str, arguments: Value, declaration: &str) -> Value {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert(session_id.to_string(), page_id);

    let evaluated = dispatch(
        &CdpRequest {
            id: 1,
            method: "Runtime.evaluate".to_string(),
            params: json!({"expression": "window"}),
            session_id: Some(session_id.to_string()),
        },
        &mut ctx,
    )
    .await;
    let window_id = evaluated.result.unwrap()["result"]["objectId"]
        .as_str()
        .expect("window must be returned by reference")
        .to_string();

    let response = dispatch(
        &CdpRequest {
            id: 2,
            method: "Runtime.callFunctionOn".to_string(),
            params: json!({
                "functionDeclaration": declaration,
                "arguments": arguments,
                "objectId": window_id,
            }),
            session_id: Some(session_id.to_string()),
        },
        &mut ctx,
    )
    .await;

    assert!(response.error.is_none(), "{:?}", response.error);
    let reply = response.result.unwrap();
    assert!(reply.get("exceptionDetails").is_none(), "{reply}");
    reply["result"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_object_id_argument_does_not_fall_back_to_a_null_value() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("rod-null-shadow".to_string(), page_id);
    let response = dispatch(&CdpRequest {
        id: 1, method: "Runtime.callFunctionOn".to_string(),
        params: json!({
            "functionDeclaration": "function(o) { globalThis.__called = true; return o; }",
            "arguments": [{"value":null,"objectId":"__never_resolves__"}],
        }),
        session_id: Some("rod-null-shadow".to_string()),
    }, &mut ctx).await;
    assert_eq!(response.error.unwrap().message, "Could not find object with given id");
}

#[tokio::test(flavor = "current_thread")]
async fn object_id_argument_passes_the_real_object() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "rod-arg".to_string();
    ctx.sessions.insert(session_id.clone(), page_id);

    let evaluated = dispatch(
        &CdpRequest {
            id: 1,
            method: "Runtime.evaluate".to_string(),
            params: json!({"expression": "({answer: 42})"}),
            session_id: Some(session_id.clone()),
        },
        &mut ctx,
    )
    .await;
    let object_id = evaluated.result.unwrap()["result"]["objectId"]
        .as_str()
        .expect("object must be returned by reference")
        .to_string();

    // The exact shape go-rod sends (page_eval.go ensureJSHelper).
    let response = dispatch(
        &CdpRequest {
            id: 2,
            method: "Runtime.callFunctionOn".to_string(),
            params: json!({
                "functionDeclaration": "function(o) { return o.answer; }",
                "arguments": [{"value": null, "objectId": object_id}],
            }),
            session_id: Some(session_id),
        },
        &mut ctx,
    )
    .await;

    assert!(response.error.is_none(), "{:?}", response.error);
    let reply = response.result.unwrap();
    assert!(reply.get("exceptionDetails").is_none(), "{reply}");
    assert_eq!(reply["result"]["type"], "number", "{reply}");
    assert_eq!(reply["result"]["value"], json!(42.0), "{reply}");
}

// Without an objectId a null `value` is a genuine null argument and must stay one.
#[tokio::test(flavor = "current_thread")]
async fn null_value_without_object_id_stays_null() {
    let result = call_with_arguments(
        "plain-null",
        json!([{"value": null}]),
        "function(o) { return o === null; }",
    )
    .await;
    assert_eq!(result["type"], "boolean", "{result}");
    assert_eq!(result["value"], json!(true), "{result}");
}
