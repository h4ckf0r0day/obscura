use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn check_by_value(method: &str, await_promise: bool) {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "by-value".to_string();
    ctx.sessions.insert(session_id.clone(), page_id);

    for (expression, expected_type, expected_value) in [
        ("undefined", "undefined", None),
        ("null", "object", Some(Value::Null)),
        ("0", "number", Some(json!(0.0))),
        ("false", "boolean", Some(json!(false))),
        ("''", "string", Some(json!(""))),
        ("({answer: 42})", "object", Some(json!({"answer": 42}))),
        ("[null, 1]", "object", Some(json!([null, 1]))),
    ] {
        let expression = if await_promise {
            format!("Promise.resolve({expression})")
        } else {
            expression.to_string()
        };
        let mut params = json!({"returnByValue": true, "awaitPromise": await_promise});
        if method == "Runtime.evaluate" {
            params["expression"] = json!(expression);
        } else {
            params["functionDeclaration"] = json!(format!("function() {{ return {expression}; }}"));
        }
        let response = dispatch(
            &CdpRequest {
                id: 1,
                method: method.to_string(),
                params,
                session_id: Some(session_id.clone()),
            },
            &mut ctx,
        )
        .await;
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        let reply = response.result.unwrap();
        assert!(reply.get("exceptionDetails").is_none(), "{reply}");
        let result = &reply["result"];
        assert_eq!(
            result["type"], expected_type,
            "{method}, {expression}: {reply}"
        );
        assert_eq!(
            result.get("value"),
            expected_value.as_ref(),
            "{method}, {expression}: {reply}"
        );
        assert!(result.get("objectId").is_none(), "{reply}");
        if expected_type == "undefined" {
            assert!(result.get("subtype").is_none(), "{reply}");
        } else if expected_value == Some(Value::Null) {
            assert_eq!(result["subtype"], "null", "{reply}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_by_value_preserves_undefined() {
    check_by_value("Runtime.evaluate", false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn call_function_on_by_value_preserves_undefined() {
    check_by_value("Runtime.callFunctionOn", false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn evaluate_awaited_by_value_preserves_undefined() {
    check_by_value("Runtime.evaluate", true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn call_function_on_awaited_by_value_preserves_undefined() {
    check_by_value("Runtime.callFunctionOn", true).await;
}
