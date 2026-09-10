use obscura_browser::lifecycle::WaitUntil;
use obscura_js::runtime::RemoteObjectInfo;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub(crate) fn execution_context_created_event(
    context: &crate::dispatch::ExecutionContextRecord,
    session_id: Option<String>,
) -> crate::types::CdpEvent {
    crate::types::CdpEvent {
        method: "Runtime.executionContextCreated".to_string(),
        params: json!({
            "context": {
                "id": context.id,
                "origin": context.origin,
                "name": context.world_name,
                "uniqueId": context.unique_id,
                "auxData": {
                    "isDefault": context.is_default,
                    "type": if context.is_default { "default" } else { "isolated" },
                    "frameId": context.frame_id,
                }
            }
        }),
        session_id,
    }
}

/// Whether a binding name is a plain JS identifier and therefore safe to
/// interpolate into the generated shim / teardown scripts. Chromium bindings
/// are identifiers; anything else (quotes, brackets, spaces, operators) could
/// break out of the surrounding string literal and inject arbitrary JS into the
/// page. `Runtime.addBinding` always enforced this, but `Runtime.removeBinding`
/// did not, so a crafted name escaped `delete globalThis['{name}']` and ran in
/// the page context. Both handlers now share this guard.
fn is_valid_binding_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !name.chars().next().unwrap_or('0').is_ascii_digit()
}

/// Drain pending JS-initiated navigation (form.submit, location.assign, etc),
/// then emit the same CDP nav-event sequence Page.navigate emits so
/// Puppeteer's waitForNavigation / Playwright's wait_for_url resolves.
/// Without this, in-page navigations look like Runtime.evaluate finishing
/// to clients and they hang waiting for a frameNavigated that never fires.
async fn emit_post_eval_nav(
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<(), String> {
    let page = ctx
        .get_session_page_mut(session_id)
        .ok_or("No page")?;
    let did_navigate = page.process_pending_navigation().await.map_err(|e| e.to_string())?;
    if !did_navigate {
        return Ok(());
    }
    let (frame_id, page_url, page_id, network_events, reached_idle) = {
        let p = ctx.get_session_page_mut(session_id).ok_or("No page")?;
        (
            p.frame_id.clone(),
            p.url_string(),
            p.id.clone(),
            p.network_events.drain(..).collect::<Vec<_>>(),
            p.lifecycle.is_network_idle(),
        )
    };
    let loader_id = format!("loader-{}", uuid::Uuid::new_v4());
    super::page::emit_navigation_events(
        ctx,
        session_id,
        &frame_id,
        &loader_id,
        &page_url,
        &page_id,
        &network_events,
        WaitUntil::Load,
        reached_idle,
    );
    Ok(())
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => {
            // puppeteer-extra's FrameManager.initialize calls Runtime.enable on
            // the browser-level connection BEFORE any page target exists. Real
            // Chrome replies with `{}` and emits executionContextCreated when
            // a context appears. Returning "No page" here breaks the standard
            // puppeteer connect/newPage flow. If there's no session, succeed
            // silently — the next Target.attachToTarget will set things up.
            if let Some(page_id) = session_id.as_ref()
                .and_then(|session| ctx.sessions.get(session)).cloned()
            {
                let newly_enabled = session_id.as_ref().is_some_and(|session| {
                    ctx.runtime_enabled_sessions.insert(session.clone())
                });
                ctx.refresh_runtime_event_collection(&page_id);
                ctx.ensure_default_context(&page_id);
                if newly_enabled {
                    let events = ctx.contexts_for_page(&page_id)
                        .map(|context| execution_context_created_event(
                            context, session_id.clone(),
                        ))
                        .collect::<Vec<_>>();
                    ctx.pending_events.extend(events);
                }
            }
            Ok(json!({}))
        }
        "disable" => {
            if let Some(session_id) = session_id {
                let page_id = ctx.sessions.get(session_id).cloned();
                ctx.runtime_enabled_sessions.remove(session_id);
                if let Some(page_id) = page_id {
                    ctx.refresh_runtime_event_collection(&page_id);
                }
            }
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

            validate_context(params, "contextId", ctx, session_id, "evaluate")?;

            let await_promise = params
                .get("awaitPromise")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // CDP `timeout` field (milliseconds). Default to Chrome's
            // protocolTimeout (30s) so long evaluations don't pin the V8 lock
            // indefinitely and starve every other CDP command on the same
            // session.
            let timeout_ms = params
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(30_000);

            let page = ctx
                .get_session_page_mut(session_id)
                .ok_or("No page")?;
            let info = match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                page.evaluate_for_cdp_with_timeout(
                    expression,
                    return_by_value,
                    await_promise,
                    timeout_ms,
                ),
            )
            .await
            {
                Ok(Ok(info)) => info,
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(format!(
                        "Runtime.evaluate exceeded {timeout_ms}ms timeout"
                    ));
                }
            };
            emit_post_eval_nav(ctx, session_id).await?;

            Ok(evaluation_reply(&info))
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

            // #51: validate executionContextId the same way Runtime.evaluate
            // does. CDP names this field `executionContextId` on
            // callFunctionOn (not `contextId`); a request may omit it when
            // `objectId` is supplied — in that case context validation is a
            // no-op and the default context is used.
            validate_context(params, "executionContextId", ctx, session_id, "callFunctionOn")?;

            // Keep awaitPromise alive for the same command budget as evaluate.
            // Playwright implements waits with callFunctionOn on some utility
            // paths, so a shorter hidden cap makes the client return before the
            // requested browser timer fires.
            let timeout_ms = params
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(30_000);

            let page = ctx
                .get_session_page_mut(session_id)
                .ok_or("No page")?;
            let info = match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                page.call_function_on_for_cdp_with_timeout(
                    function_declaration,
                    object_id,
                    &arguments,
                    return_by_value,
                    await_promise,
                    timeout_ms,
                ),
            )
            .await
            {
                Ok(Ok(info)) => info,
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    return Err(format!(
                        "Runtime.callFunctionOn exceeded {timeout_ms}ms timeout"
                    ));
                }
            };
            emit_post_eval_nav(ctx, session_id).await?;

            Ok(evaluation_reply(&info))
        }
        "getProperties" => {
            // Puppeteer's $$() flow:
            //   1. evaluate querySelectorAll → handle for the NodeList
            //   2. getProperties on that handle → indexed items
            //   3. For each item, JSHandle.asElement() checks subtype === 'node';
            //      if true, wraps as ElementHandle (with click/type/etc).
            //
            // Older impl returned the raw value via JSON, dropping the node
            // identity. Items came back as `{type:'object'}` with no objectId
            // and no subtype, so asElement returned null and the caller got
            // plain JSHandles back from page.$$ -- breaking checkboxes[0].click().
            //
            // We now:
            //   1. Walk the underlying object in JS, allocating a stable child
            //      oid per (parent_oid + index) and stashing each value in
            //      __obscura_objects so later callFunctionOn can resolve it.
            //   2. Annotate each item with subtype:'node' + className when the
            //      value has a numeric nodeType, so Puppeteer wraps it as
            //      ElementHandle.
            let object_id = params.get("objectId").and_then(|v| v.as_str());
            if let Some(oid) = object_id {
                let page = ctx
                    .get_session_page_mut(session_id)
                    .ok_or("No page")?;
                // The child ids minted below are `<parent>::<key>` and the key
                // is a property name off a page object, so the page decides
                // what ends up inside this literal. A JSON literal covers the
                // C0 controls a manual quote/backslash pair leaves alone; see
                // `util::object_id_literal`.
                let oid_literal = crate::util::object_id_literal(oid);
                let code = format!(
                    "(function() {{\
                        var obj = globalThis.__obscura_objects[{oid}];\
                        if (!obj || typeof obj !== 'object') return [];\
                        var keys = Object.keys(obj);\
                        return keys.map(function(k) {{\
                            var v = obj[k];\
                            var t = typeof v;\
                            var item = {{ name: k, type: t }};\
                            if (v === null) {{ item.value = null; return item; }}\
                            if (t !== 'object' && t !== 'function') {{ item.value = v; return item; }}\
                            var childOid = {oid} + '::' + k;\
                            globalThis.__obscura_objects[childOid] = v;\
                            item.childOid = childOid;\
                            if (typeof v.nodeType === 'number') {{\
                                item.subtype = 'node';\
                                item.className = v.constructor && v.constructor.name ? v.constructor.name : (v.tagName ? 'HTML' + v.tagName.charAt(0) + v.tagName.slice(1).toLowerCase() + 'Element' : 'Node');\
                                item.description = v.tagName ? v.tagName.toLowerCase() : (v.nodeName || 'node');\
                            }} else if (Array.isArray(v)) {{\
                                item.subtype = 'array';\
                                item.className = 'Array';\
                                item.description = 'Array(' + v.length + ')';\
                            }} else {{\
                                item.className = (v.constructor && v.constructor.name) || 'Object';\
                                item.description = item.className;\
                            }}\
                            return item;\
                        }});\
                    }})()",
                    oid = oid_literal,
                );
                let result = page.evaluate(&code);
                if let serde_json::Value::Array(props) = result {
                    let descriptors: Vec<Value> = props
                        .iter()
                        .map(|p| {
                            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let prop_type =
                                p.get("type").and_then(|v| v.as_str()).unwrap_or("undefined");
                            let mut remote = json!({ "type": prop_type });
                            if let Some(child_oid) = p.get("childOid").and_then(|v| v.as_str()) {
                                remote["type"] = json!("object");
                                if let Some(sub) = p.get("subtype").and_then(|v| v.as_str()) {
                                    remote["subtype"] = json!(sub);
                                }
                                if let Some(cls) = p.get("className").and_then(|v| v.as_str()) {
                                    remote["className"] = json!(cls);
                                }
                                if let Some(desc) = p.get("description").and_then(|v| v.as_str()) {
                                    remote["description"] = json!(desc);
                                }
                                remote["objectId"] = json!(child_oid);
                            } else if let Some(val) = p.get("value") {
                                match val {
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
                                        remote["value"] = val.clone();
                                    }
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
            if is_valid_binding_name(name) {
                // The shim forwards every call back to Rust through
                // op_binding_called; the CDP dispatcher then drains the
                // queue and emits Runtime.bindingCalled events the same
                // way Chromium does. Chromium's V8InspectorImpl rejects
                // calls without exactly one argument and ToString-coerces
                // that argument before emitting it as the payload — we
                // match the coercion (`String(arg)`) and silently drop
                // calls with wrong arity, which is what Chrome does.
                let shim = format!(
                    "globalThis['{name}'] = function (arg) {{\
                        if (arguments.length !== 1) return;\
                        try {{\
                            const payload = typeof arg === 'string' ? arg : String(arg);\
                            Deno.core.ops.op_binding_called('{name}', payload);\
                        }} catch (e) {{ /* swallow: binding must not throw into page */ }}\
                    }};",
                    name = name,
                );
                // Re-install on every navigation: globalThis is wiped on
                // each new document, and puppeteer registers bindings
                // once-per-page rather than once-per-document.
                let key = format!("__obscura_binding__{}", name);
                ctx.preload_scripts.retain(|(k, _)| k != &key);
                ctx.preload_scripts.push((key, shim.clone()));
                // Remember who subscribed, so the call goes back to this
                // session rather than to whichever session of the page a
                // HashMap happens to yield first. A client discards an event
                // addressed to a session it does not hold, and the session
                // Target.createTarget leaves behind is not the one a client
                // ends up using.
                if let Some(session_id) = session_id {
                    let owners = ctx.binding_sessions.entry(name.to_string()).or_default();
                    if !owners.contains(session_id) {
                        owners.push(session_id.clone());
                    }
                }
                // Install on the current page so the binding is usable
                // immediately, without waiting for the next navigation.
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    page.evaluate(&shim);
                }
            }
            Ok(json!({}))
        }
        "removeBinding" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if is_valid_binding_name(name) {
                let key = format!("__obscura_binding__{}", name);
                ctx.preload_scripts.retain(|(k, _)| k != &key);
                if let Some(session_id) = session_id {
                    if let Some(owners) = ctx.binding_sessions.get_mut(name) {
                        owners.retain(|owner| owner != session_id);
                        if owners.is_empty() {
                            ctx.binding_sessions.remove(name);
                        }
                    }
                }
                if let Some(page) = ctx.get_session_page_mut(session_id) {
                    page.evaluate(&format!("delete globalThis['{}'];", name));
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

/// Reject `Runtime.{evaluate,callFunctionOn}` calls that target an execution
/// context Obscura has not advertised for the attached page. An absent identity
/// uses the page's default context. Direct embedders retain the compatibility
/// path for ids reserved through `next_isolated_context`.
fn validate_context(
    params: &Value,
    field: &str,
    ctx: &crate::dispatch::CdpContext,
    session_id: &Option<String>,
    method: &str,
) -> Result<(), String> {
    let id = params.get(field).and_then(|value| value.as_i64());
    let unique_id = params.get("uniqueContextId").and_then(|value| value.as_str());
    if id.is_some() && unique_id.is_some() {
        return Err(format!(
            "Runtime.{method} cannot specify both {field} and uniqueContextId"
        ));
    }
    if id.is_none() && unique_id.is_none() {
        return Ok(());
    }
    let record = id.and_then(|id| ctx.context_by_id(id))
        .or_else(|| unique_id.and_then(|id| ctx.context_by_unique_id(id)));
    if let Some(record) = record {
        let owner = session_id.as_ref().and_then(|session| ctx.sessions.get(session));
        if owner == Some(&record.page_id) {
            // This registry currently validates ownership/routing only. A
            // named isolated context still executes in the owning page's
            // current V8 runtime/global; it is not a separate V8 realm yet.
            return Ok(());
        }
    } else if session_id.is_none() && unique_id.is_none()
        && id.is_some_and(|id| ctx.valid_context_ids.contains(&id))
    {
        // Direct embedders can still reserve an id through the existing public
        // next_isolated_context API. Attached sessions require page ownership.
        return Ok(());
    }
    let identity = id.map(|id| id.to_string())
        .or_else(|| unique_id.map(str::to_string))
        .unwrap_or_default();
    if record.is_none() || session_id.is_some() {
        return Err(format!(
            "Cannot find context with specified id: {}",
            identity
        ));
    }
    Ok(())
}

/// Shape one `Runtime.evaluate` or `Runtime.callFunctionOn` reply.
///
/// A thrown value, or the value a promise rejected with, is not a protocol
/// failure: the command succeeds and reports it through `exceptionDetails`,
/// which is what a client rebuilds the page error from. The value is repeated
/// in `result` the way Chrome does, so a client that reads only `result` sees
/// the error object rather than a success that never happened.
///
/// `exceptionId` is a fixed 1 because nothing here correlates exceptions
/// across commands; clients key off the presence of the field, not its id.
fn evaluation_reply(info: &RemoteObjectInfo) -> Value {
    let remote = remote_object_from_info(info);
    if !info.thrown {
        return json!({ "result": remote });
    }
    json!({
        "result": remote.clone(),
        "exceptionDetails": {
            "exceptionId": 1,
            "text": "Uncaught",
            "lineNumber": 0,
            "columnNumber": 0,
            "exception": remote,
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::CdpContext;

    // Issue #51 — Runtime.evaluate / callFunctionOn must read and validate
    // contextId. Pre-fix the parameter was silently dropped, so Playwright's
    // locator (which targets the utility world created by
    // Page.createIsolatedWorld) ran in the wrong context and timed out.
    //
    // Phase 5.5 (RED-then-GREEN) verification:
    //   - Without the prod fix, `valid_context_ids` does not exist on
    //     CdpContext → these tests fail to compile.
    //   - With the prod fix, all four tests pass.

    #[tokio::test]
    async fn evaluate_rejects_unknown_context_id() {
        let mut ctx = CdpContext::new();
        let err = handle(
            "evaluate",
            &json!({ "expression": "1 + 1", "contextId": 9999 }),
            &mut ctx,
            &None,
        )
        .await
        .expect_err("unknown contextId must error per CDP spec");
        assert!(
            err.contains("Cannot find context with specified id"),
            "error must match real Chrome's wording: {err}"
        );
        assert!(err.contains("9999"), "error must include the bad id: {err}");
    }

    #[tokio::test]
    async fn call_function_on_rejects_unknown_execution_context_id() {
        let mut ctx = CdpContext::new();
        let err = handle(
            "callFunctionOn",
            &json!({
                "functionDeclaration": "() => 42",
                "executionContextId": 9999,
            }),
            &mut ctx,
            &None,
        )
        .await
        .expect_err("unknown executionContextId must error per CDP spec");
        assert!(
            err.contains("Cannot find context with specified id"),
            "error must match Chrome wording: {err}"
        );
    }

    #[tokio::test]
    async fn evaluate_rejects_unadvertised_compatibility_context_ids() {
        for context_id in [1, 2] {
            let mut ctx = CdpContext::new();
            let error = handle(
                "evaluate",
                &json!({ "expression": "1 + 1", "contextId": context_id }),
                &mut ctx,
                &None,
            )
            .await
            .expect_err("an unadvertised compatibility id must not route");
            assert!(error.contains("Cannot find context"));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn evaluate_reports_a_rejection_through_exception_details() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = "evaluate-rejection".to_string();
        ctx.sessions.insert(session_id.clone(), page_id);

        let reply = handle(
            "evaluate",
            &json!({
                "expression": "Promise.reject(new Error('boom'))",
                "returnByValue": true,
                "awaitPromise": true,
                "timeout": 3000,
            }),
            &mut ctx,
            &Some(session_id),
        )
        .await
        .expect("a rejection is answered, not failed at the protocol level");

        let details = reply
            .get("exceptionDetails")
            .expect("a thrown value is reported through exceptionDetails");
        assert_eq!(details["text"], json!("Uncaught"));
        assert_eq!(details["exception"]["subtype"], json!("error"));
        assert_eq!(details["exception"]["className"], json!("Error"));
        assert!(
            details["exception"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("boom"),
            "the description carries the page message: {details}"
        );
        // Chrome repeats the exception in `result`, so a client that reads
        // only that field still sees the error rather than a stale success.
        assert_eq!(reply["result"], details["exception"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn call_function_on_reports_a_rejection_through_exception_details() {
        // This is the path Puppeteer's page.evaluate(fn) takes. It used to
        // answer successfully with `{}` for a rejected Error, so the caller
        // could not tell a failure from an empty object.
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = "call-rejection".to_string();
        ctx.sessions.insert(session_id.clone(), page_id);

        let reply = handle(
            "callFunctionOn",
            &json!({
                "functionDeclaration": "() => Promise.reject(new Error('boom'))",
                "returnByValue": true,
                "awaitPromise": true,
                "timeout": 3000,
            }),
            &mut ctx,
            &Some(session_id),
        )
        .await
        .expect("a rejection is answered, not failed at the protocol level");

        let details = reply
            .get("exceptionDetails")
            .expect("a rejected call is reported through exceptionDetails");
        assert_eq!(details["exception"]["subtype"], json!("error"));
        assert!(reply["result"]["value"].is_null(), "the error must not be serialized by value: {reply}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_resolved_evaluation_carries_no_exception_details() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = "evaluate-resolved".to_string();
        ctx.sessions.insert(session_id.clone(), page_id);

        let reply = handle(
            "evaluate",
            &json!({
                "expression": "Promise.resolve(2)",
                "returnByValue": true,
                "awaitPromise": true,
                "timeout": 3000,
            }),
            &mut ctx,
            &Some(session_id),
        )
        .await
        .unwrap();
        assert_eq!(reply["result"]["value"], json!(2.0));
        assert!(reply.get("exceptionDetails").is_none());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn evaluate_await_promise_reports_the_requested_timeout() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = "await-timeout-session".to_string();
        ctx.sessions.insert(session_id.clone(), page_id);

        let error = handle(
            "evaluate",
            &json!({
                "expression": "new Promise(() => {})",
                "returnByValue": true,
                "awaitPromise": true,
                "timeout": 25,
            }),
            &mut ctx,
            &Some(session_id),
        )
        .await
        .expect_err("an unsettled promise must not return stale result metadata");
        assert!(
            error.contains("25ms timeout") || error.contains("within 25ms"),
            "unexpected timeout error: {error}"
        );
    }

    #[tokio::test]
    async fn create_isolated_world_registers_id_for_evaluate() {
        // Round-trip: Page.createIsolatedWorld returns contextId N, and a
        // subsequent Runtime.evaluate targeting that contextId must NOT be
        // rejected.
        let mut ctx = CdpContext::new();
        // Bypass the page-attached path of createIsolatedWorld by direct
        // insert — mirrors the same effect as calling the page handler with
        // a real session.
        ctx.valid_context_ids.insert(100);

        let result = handle(
            "evaluate",
            &json!({ "expression": "1 + 1", "contextId": 100 }),
            &mut ctx,
            &None,
        )
        .await;
        if let Err(e) = result {
            assert!(
                !e.contains("Cannot find context"),
                "registered isolated-world contextId=100 must be accepted, got: {e}"
            );
        }
    }

    /// Regression for #122 item 7: puppeteer-extra's FrameManager.initialize
    /// fires Runtime.enable on the browser-level WebSocket BEFORE any page
    /// target exists. Real Chrome replies with `{}`; before the fix Obscura
    /// returned `{"error":{"code":-32601,"message":"No page"}}` and the
    /// puppeteer connect flow died.
    #[tokio::test]
    async fn enable_succeeds_when_no_session_attached() {
        let mut ctx = CdpContext::new();
        let result = handle("enable", &json!({}), &mut ctx, &None)
            .await
            .expect("Runtime.enable must succeed even with no session");
        assert_eq!(result, json!({}));
    }

    /// SEC-002 / #578 — Runtime.removeBinding must validate the binding name the
    /// same way addBinding does. Before the fix the name was interpolated
    /// straight into `delete globalThis['{name}']`, so a CDP client could break
    /// out of the string delimiter and run arbitrary JS in the page. This drives
    /// the real handler against a live page and asserts the injected statement
    /// never executes.
    #[tokio::test(flavor = "current_thread")]
    async fn remove_binding_rejects_injection_in_name() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session = Some(format!("{page_id}-session"));
        ctx.sessions.insert(session.clone().unwrap(), page_id);

        crate::domains::page::handle(
            "navigate",
            &json!({ "url": "data:text/html,<p>hi</p>", "waitUntil": "load" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate should succeed");

        // Canary the injection would flip from 0 to 1.
        ctx.get_session_page_mut(&session)
            .unwrap()
            .evaluate("globalThis.__pwned = 0");

        // The generated code is `delete globalThis['{name}']`, which the runtime
        // wraps as `return ( ... )`. A comma-expression payload stays a single
        // valid expression through that wrapper and runs the assignment:
        //   delete globalThis['x'] , (globalThis.__pwned = 1) , globalThis['y']
        handle(
            "removeBinding",
            &json!({ "name": "x'] , (globalThis.__pwned = 1) , globalThis['y" }),
            &mut ctx,
            &session,
        )
        .await
        .expect("removeBinding must return Ok regardless of the name");

        let pwned = ctx
            .get_session_page_mut(&session)
            .unwrap()
            .evaluate("globalThis.__pwned");
        assert_ne!(
            pwned.as_f64(),
            Some(1.0),
            "removeBinding must not execute JS injected via the binding name (got {pwned:?})"
        );
    }
}
