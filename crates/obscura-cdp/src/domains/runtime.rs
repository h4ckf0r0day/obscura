use obscura_js::runtime::RemoteObjectInfo;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let event = crate::types::CdpEvent {
                method: "Runtime.executionContextCreated".to_string(),
                params: json!({
                    "context": {
                        "id": 1,
                        "origin": page.url_string(),
                        "name": "",
                        "uniqueId": format!("ctx-{}", page.id),
                        "auxData": {
                            "isDefault": true,
                            "type": "default",
                            "frameId": page.frame_id,
                        }
                    }
                }),
                session_id: session_id.clone(),
            };
            ctx.pending_events.push(event);
            Ok(json!({}))
        }
        "evaluate" => {
            let expression = params
                .get("expression")
                .and_then(|v| v.as_str())
                .ok_or("expression required")?;
            let return_by_value = params
                .get("returnByValue")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let info = page.evaluate_for_cdp(expression, return_by_value);

            Ok(json!({ "result": remote_object_from_info(&info) }))
        }
        "callFunctionOn" => {
            let function_declaration = params
                .get("functionDeclaration")
                .and_then(|v| v.as_str())
                .unwrap_or("() => undefined");
            let return_by_value = params
                .get("returnByValue")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let await_promise = params
                .get("awaitPromise")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let object_id = params.get("objectId").and_then(|v| v.as_str());
            let arguments = params
                .get("arguments")
                .and_then(|v| v.as_array())
                .map(|a| a.to_vec())
                .unwrap_or_default();

            let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
            let info = page
                .call_function_on_for_cdp(
                    function_declaration,
                    object_id,
                    &arguments,
                    return_by_value,
                    await_promise,
                )
                .await;

            Ok(json!({ "result": remote_object_from_info(&info) }))
        }
        "getProperties" => {
            let object_id = params.get("objectId").and_then(|v| v.as_str());
            if let Some(oid) = object_id {
                let page = ctx.get_session_page_mut(session_id).ok_or("No page")?;
                let escaped_oid = oid.replace('\\', "\\\\").replace('\'', "\\'");
                let code = format!(
                    "(function() {{\
                        var obj = globalThis.__obscura_objects['{oid}'];\
                        if (!obj || typeof obj !== 'object') return [];\
                        return Object.keys(obj).map(function(k) {{\
                            var v = obj[k];\
                            return {{ name: k, value: v, type: typeof v }};\
                        }});\
                    }})()",
                    oid = escaped_oid,
                );
                let result = page.evaluate(&code);
                if let serde_json::Value::Array(props) = result {
                    let descriptors: Vec<Value> = props
                        .iter()
                        .map(|p| {
                            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let value = p.get("value").unwrap_or(&Value::Null);
                            let prop_type = p
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("undefined");
                            let mut remote = json!({
                                "type": prop_type,
                            });
                            match value {
                                Value::Null => {
                                    remote["type"] = json!("object");
                                    remote["subtype"] = json!("null");
                                    remote["value"] = json!(null);
                                }
                                Value::String(s) => {
                                    remote["type"] = json!("string");
                                    remote["value"] = json!(s);
                                }
                                Value::Number(n) => {
                                    remote["type"] = json!("number");
                                    remote["value"] = json!(n);
                                }
                                Value::Bool(b) => {
                                    remote["type"] = json!("boolean");
                                    remote["value"] = json!(b);
                                }
                                _ => {
                                    remote["value"] = value.clone();
                                }
                            }
                            json!({
                                "name": name,
                                "value": remote,
                                "configurable": true,
                                "enumerable": true,
                                "writable": true,
                                "isOwn": true,
                            })
                        })
                        .collect();
                    Ok(json!({ "result": descriptors, "internalProperties": [] }))
                } else {
                    Ok(json!({ "result": [], "internalProperties": [] }))
                }
            } else {
                Ok(json!({ "result": [], "internalProperties": [] }))
            }
        }
        "releaseObject" => {
            if let Some(oid) = params.get("objectId").and_then(|v| v.as_str()) {
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    page.release_object(oid);
                }
            }
            Ok(json!({}))
        }
        "releaseObjectGroup" => {
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.release_object_group();
            }
            Ok(json!({}))
        }
        "addBinding" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !name.is_empty() {
                if name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                    && !name.chars().next().unwrap_or('0').is_ascii_digit()
                {
                    if let Some(page) = ctx.get_session_page_mut(session_id) {
                        let code = format!(
                            "if (typeof globalThis.{name} === 'undefined') {{\
                                globalThis.{name} = function() {{ return null; }};\
                            }}",
                            name = name,
                        );
                        page.evaluate(&code);
                    }
                }
            }
            Ok(json!({}))
        }
        "runIfWaitingForDebugger" => Ok(json!({})),
        "getExceptionDetails" => Ok(json!({ "exceptionDetails": null })),
        "discardConsoleEntries" => Ok(json!({})),
        _ => Err(format!("Unknown Runtime method: {}", method)),
    }
}

fn remote_object_from_info(info: &RemoteObjectInfo) -> Value {
    let mut obj = json!({ "type": info.js_type });

    if let Some(ref subtype) = info.subtype {
        obj["subtype"] = json!(subtype);
    }

    if !info.class_name.is_empty() {
        obj["className"] = json!(info.class_name);
    }

    if !info.description.is_empty() {
        obj["description"] = json!(info.description);
    }

    if let Some(ref oid) = info.object_id {
        obj["objectId"] = json!(oid);
    }

    if let Some(ref value) = info.value {
        obj["value"] = value.clone();
    }

    obj
}
