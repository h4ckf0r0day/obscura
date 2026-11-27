use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

#[tokio::test(flavor = "current_thread")]
async fn primitive_results_keep_their_types_without_retaining_handles() {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = Some("primitive-results".to_string());
    ctx.sessions.insert(session_id.clone().unwrap(), page_id);
    for method in ["Runtime.evaluate", "Runtime.callFunctionOn"] {
        for await_promise in [false, true] {
            for return_by_value in [false, true] {
                for (source, kind, value, unserializable) in [
                    ("false", "boolean", Some(json!(false)), None),
                    ("42", "number", Some(json!(42.0)), None),
                    ("'<header>Header</header>'", "string", Some(json!("<header>Header</header>")), None),
                    ("null", "object", Some(Value::Null), None),
                    ("undefined", "undefined", None, None),
                    ("-0", "number", None, Some("-0")),
                    ("NaN", "number", None, Some("NaN")),
                    ("Infinity", "number", None, Some("Infinity")),
                    ("-Infinity", "number", None, Some("-Infinity")),
                    ("9007199254740993n", "bigint", None, Some("9007199254740993n")),
                ] {
                    let expression = if await_promise {
                        format!("Promise.resolve({source})")
                    } else {
                        source.to_string()
                    };
                    let mut params = json!({"returnByValue": return_by_value, "awaitPromise": await_promise});
                    if method == "Runtime.evaluate" {
                        params["expression"] = json!(expression);
                    } else {
                        params["functionDeclaration"] = json!(format!("function() {{ return {expression}; }}"));
                    }
                    let response = dispatch(&CdpRequest {
                        id: 1, method: method.to_string(), params, session_id: session_id.clone(),
                    }, &mut ctx).await;
                    assert!(response.error.is_none(), "{method}: {:?}", response.error);
                    let reply = response.result.unwrap();
                    assert!(reply.get("exceptionDetails").is_none(), "{reply}");
                    let result = &reply["result"];
                    assert!(result.get("objectId").is_none(), "{method} {source}: {reply}");
                    assert_eq!(result["type"], kind, "{method} {source}: {reply}");
                    assert_eq!(result.get("value"), value.as_ref(), "{method} {source}: {reply}");
                    assert_eq!(result.get("unserializableValue").and_then(Value::as_str),
                        unserializable, "{method} {source}: {reply}");
                    if source == "null" {
                        assert_eq!(result["subtype"], "null", "{reply}");
                    }
                }
            }
        }
    }
    let page = ctx.get_session_page_mut(&session_id).unwrap();
    assert_eq!(page.evaluate("Object.keys(globalThis.__obscura_objects).length"), json!(0.0),
        "primitive results have no client handle to release and must not stay rooted");
}
