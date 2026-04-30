use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::{JsRuntime, RuntimeOptions};
use obscura_dom::DomTree;

use crate::module_loader::ObscuraModuleLoader;
use crate::ops::{build_extension, ObscuraState};

static SNAPSHOT: &[u8] = include_bytes!(env!("OBSCURA_SNAPSHOT_PATH"));

const BOOTSTRAP_JS: &str = include_str!("../js/bootstrap.js");

#[derive(Debug, Clone)]
pub struct RemoteObjectInfo {
    pub js_type: String,
    pub subtype: Option<String>,
    pub class_name: String,
    pub description: String,
    pub object_id: Option<String>,
    pub value: Option<serde_json::Value>,
}

pub struct ObscuraJsRuntime {
    runtime: JsRuntime,
    state: Rc<RefCell<ObscuraState>>,
    object_store: HashMap<String, String>,
    object_counter: u64,
}

impl ObscuraJsRuntime {
    pub fn new() -> Self {
        Self::with_base_url("about:blank")
    }

    pub fn with_base_url(base_url: &str) -> Self {
        let state = Rc::new(RefCell::new(ObscuraState::new()));
        let state_clone = state.clone();

        let module_loader = Rc::new(ObscuraModuleLoader::new(base_url));

        let mut runtime = JsRuntime::new(RuntimeOptions {
            extensions: vec![build_extension()],
            module_loader: Some(module_loader),
            startup_snapshot: Some(SNAPSHOT),
            ..Default::default()
        });

        runtime.op_state().borrow_mut().put(state_clone);

        runtime
            .execute_script(
                "<obscura:init>",
                "globalThis.__obscura_objects = {}; globalThis.__obscura_oid = 0; globalThis.__obscura_init();".to_string(),
            )
            .expect("init should not fail");

        ObscuraJsRuntime {
            runtime,
            state,
            object_store: HashMap::new(),
            object_counter: 0,
        }
    }

    pub fn set_cookie_jar(&self, jar: std::sync::Arc<obscura_net::CookieJar>) {
        self.state.borrow_mut().cookie_jar = Some(jar);
    }

    pub fn set_http_client(&self, client: std::sync::Arc<obscura_net::ObscuraHttpClient>) {
        self.state.borrow_mut().http_client = Some(client);
    }

    pub fn set_dom(&self, dom: DomTree) {
        self.state.borrow_mut().dom = Some(dom);
    }

    pub fn set_url(&self, url: &str) {
        self.state.borrow_mut().url = url.to_string();
    }

    pub fn set_title(&self, title: &str) {
        self.state.borrow_mut().title = title.to_string();
    }

    pub fn set_blocked_urls(&self, patterns: Vec<String>) {
        self.state.borrow_mut().blocked_urls = patterns;
    }

    pub fn take_pending_navigation(&self) -> Option<(String, String, String)> {
        self.state.borrow_mut().pending_navigation.take()
    }

    pub fn set_intercept_tx(&self, tx: tokio::sync::mpsc::UnboundedSender<crate::ops::InterceptedRequest>) {
        let mut state = self.state.borrow_mut();
        state.intercept_tx = Some(tx);
        state.intercept_enabled = true;
    }

    pub fn set_user_agent(&mut self, ua: &str) {
        let escaped = ua.replace('\\', "\\\\").replace('\'', "\\'");
        let _ = self.runtime.execute_script(
            "<set-ua>",
            format!("globalThis.__obscura_ua = '{}';", escaped),
        );
    }
    pub fn evaluate(&mut self, expression: &str) -> Result<serde_json::Value, String> {
        let wrapped = Self::wrap_expression(expression);
        let result = self
            .runtime
            .execute_script("<eval>", wrapped)
            .map_err(|e| format!("JS error: {}", e))?;
        self.v8_to_json(result)
    }

    pub fn evaluate_for_cdp(
        &mut self,
        expression: &str,
        return_by_value: bool,
    ) -> Result<RemoteObjectInfo, String> {
        if return_by_value {
            let val = self.evaluate(expression)?;
            return Ok(Self::info_from_json(&val));
        }

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        // Same trailing-semicolon trim as wrap_expression — Playwright's
        // utility-script eval ends with `})();`, and `({expr})` would
        // otherwise become `(...;)` which is a parse-time SyntaxError.
        let cleaned_expr = expression
            .trim()
            .trim_end_matches(|c: char| c == ';' || c.is_whitespace());

        let meta_code = format!(
            "(function() {{\n\
                var __result;\n\
                try {{ __result = ({expr}); }} catch(e) {{ __result = undefined; }}\n\
                globalThis.__obscura_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            expr = cleaned_expr,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );

        let result = self
            .runtime
            .execute_script("<eval-remote>", meta_code)
            .map_err(|e| format!("JS error: {}", e))?;

        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str)
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );

        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    pub async fn call_function_on_for_cdp(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
    ) -> Result<RemoteObjectInfo, String> {
        let this_expr = self.resolve_this(object_id);
        let (setup, args_list) = self.build_args(arguments);

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        if await_promise {
            let code = format!(
                "(async function() {{\n\
                    {setup}\n\
                    var __fn = ({fn_decl});\n\
                    var __this = ({this_expr});\n\
                    var __result = await __fn.call(__this, {args});\n\
                    globalThis.__obscura_objects['{oid}'] = __result;\n\
                    globalThis.__obscura_await_meta = {meta_fn};\n\
                }})()",
                setup = setup,
                fn_decl = function_declaration,
                this_expr = this_expr,
                args = args_list,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
            );

            self.runtime
                .execute_script("<callFnAsync>", code)
                .map_err(|e| format!("JS error: {}", e))?;

            self.resolve_promises().await;

            if return_by_value {
                let read = self.runtime.execute_script(
                    "<readResult>",
                    format!("globalThis.__obscura_objects['{}']", oid),
                ).map_err(|e| format!("JS error: {}", e))?;
                let json_val = self.v8_to_json(read)?;
                return Ok(Self::info_from_json(&json_val));
            }

            let meta_result = self.runtime.execute_script(
                "<readMeta>",
                "globalThis.__obscura_await_meta".to_string(),
            ).map_err(|e| format!("JS error: {}", e))?;
            let meta_str = self.v8_to_json(meta_result)?;
            let meta_json = if let serde_json::Value::String(s) = &meta_str {
                serde_json::from_str(s).unwrap_or(meta_str.clone())
            } else {
                meta_str
            };
            self.object_store.insert(
                oid.clone(),
                format!("globalThis.__obscura_objects['{}']", oid),
            );
            return Ok(Self::info_from_meta(&meta_json, Some(oid)));
        }

        if return_by_value {
            let code = format!(
                "(function() {{\n\
                    {setup}\n\
                    var __fn = ({fn_decl});\n\
                    var __this = ({this_expr});\n\
                    return __fn.call(__this, {args});\n\
                }})()",
                setup = setup,
                fn_decl = function_declaration,
                this_expr = this_expr,
                args = args_list,
            );
            let result = self.runtime
                .execute_script("<callFnByValue>", code)
                .map_err(|e| format!("JS error: {}", e))?;
            let json_val = self.v8_to_json(result)?;
            return Ok(Self::info_from_json(&json_val));
        }

        let code = format!(
            "(function() {{\n\
                {setup}\n\
                var __fn = ({fn_decl});\n\
                var __this = ({this_expr});\n\
                var __result = __fn.call(__this, {args});\n\
                globalThis.__obscura_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            setup = setup,
            fn_decl = function_declaration,
            this_expr = this_expr,
            args = args_list,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self.runtime
            .execute_script("<callFnRemote>", code)
            .map_err(|e| format!("JS error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }
    pub async fn call_function_on(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.call_function_on_for_cdp(function_declaration, object_id, arguments, return_by_value, false).await
    }
    pub fn store_object(&mut self, js_expression: &str) -> Result<String, String> {
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!(
            "globalThis.__obscura_objects['{}'] = ({});",
            oid, js_expression,
        );
        self.runtime
            .execute_script("<store>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(oid)
    }

    pub fn store_object_with_meta(
        &mut self,
        js_expression: &str,
    ) -> Result<RemoteObjectInfo, String> {
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!(
            "(function() {{\n\
                var __result = ({expr});\n\
                globalThis.__obscura_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            expr = js_expression,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self
            .runtime
            .execute_script("<store-meta>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    pub fn release_object(&mut self, object_id: &str) {
        if self.object_store.remove(object_id).is_some() {
            let code = format!(
                "delete globalThis.__obscura_objects['{}'];",
                object_id,
            );
            let _ = self.runtime.execute_script("<release>", code);
        }
    }

    pub fn release_object_group(&mut self) {
        let _ = self.runtime.execute_script(
            "<releaseGroup>",
            "globalThis.__obscura_objects = {};".to_string(),
        );
        self.object_store.clear();
    }
    pub async fn load_module(&mut self, url: &str) -> Result<(), String> {
        let specifier = deno_core::ModuleSpecifier::parse(url)
            .map_err(|e| format!("Invalid module URL {}: {}", url, e))?;

        let module_id = self
            .runtime
            .load_side_es_module_from_code(&specifier, deno_core::ModuleCodeString::from_static(""))
            .await
            .map_err(|e| format!("Module load error: {}", e))?;

        let result = self.runtime.mod_evaluate(module_id);

        let timeout = tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            self.runtime.run_event_loop(deno_core::PollEventLoopOptions::default()),
        ).await;

        match timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("Module event loop error: {}", e)),
            Err(_) => {
                tracing::warn!("Module evaluation timed out after 10s: {}", url);
                return Ok(());
            }
        }

        match result.await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("Module eval error: {}", e);
                Ok(())
            }
        }
    }

    pub async fn load_inline_module(&mut self, code: &str, base_url: &str) -> Result<(), String> {
        let specifier = deno_core::ModuleSpecifier::parse(
            &format!("{}#inline-module-{}", base_url, self.object_counter),
        )
        .unwrap_or_else(|_| deno_core::ModuleSpecifier::parse("about:blank").unwrap());

        self.object_counter += 1;

        let module_id = self
            .runtime
            .load_side_es_module_from_code(
                &specifier,
                deno_core::ModuleCodeString::from(code.to_string()),
            )
            .await
            .map_err(|e| format!("Inline module load error: {}", e))?;

        let result = self.runtime.mod_evaluate(module_id);

        let timeout = tokio::time::timeout(
            tokio::time::Duration::from_secs(10),
            self.runtime.run_event_loop(deno_core::PollEventLoopOptions::default()),
        ).await;

        match timeout {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("Module event loop error: {}", e)),
            Err(_) => {
                tracing::warn!("Inline module timed out after 10s");
                return Ok(());
            }
        }

        match result.await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("Inline module eval error: {}", e);
                Ok(())
            }
        }
    }

    pub fn execute_script(&mut self, _name: &str, source: &str) -> Result<(), String> {
        self.runtime
            .execute_script("<script>", source.to_string())
            .map_err(|e| format!("JS error: {}", e))?;
        Ok(())
    }

    pub fn execute_script_guarded(&mut self, _name: &str, source: &str) -> Result<(), String> {
        if source.len() < 10_000 {
            self.execute_script(_name, source)
        } else {
            self.execute_script_with_timeout(source, std::time::Duration::from_secs(5))
        }
    }

    pub fn execute_script_with_timeout(
        &mut self,
        source: &str,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        if timeout.is_zero() {
            self.runtime
                .execute_script("<script>", source.to_string())
                .map_err(|e| format!("JS error: {}", e))?;
            return Ok(());
        }

        let isolate_handle = self.runtime.v8_isolate().thread_safe_handle();

        let pair = std::sync::Arc::new((
            std::sync::Mutex::new(false),
            std::sync::Condvar::new(),
        ));
        let pair_clone = pair.clone();

        let watchdog = std::thread::spawn(move || {
            let (lock, cvar) = &*pair_clone;
            let mut cancelled = lock.lock().unwrap();
            let deadline = std::time::Instant::now() + timeout;

            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    isolate_handle.terminate_execution();
                    return;
                }

                let result = cvar.wait_timeout(cancelled, remaining).unwrap();
                cancelled = result.0;
                if *cancelled {
                    return;
                }
            }
        });

        let result = self
            .runtime
            .execute_script("<script>", source.to_string());

        {
            let (lock, cvar) = &*pair;
            let mut cancelled = lock.lock().unwrap();
            *cancelled = true;
            cvar.notify_one();
        }
        let _ = watchdog.join();

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("Uncaught Error: execution terminated") {
                    tracing::warn!("Script killed after {}s timeout", timeout.as_secs());
                    self.runtime.execute_script("<reset>", "undefined".to_string()).ok();
                    Ok(())
                } else {
                    Err(format!("JS error: {}", msg))
                }
            }
        }
    }

    pub async fn run_event_loop(&mut self) -> Result<(), String> {
        self.runtime
            .run_event_loop(deno_core::PollEventLoopOptions::default())
            .await
            .map_err(|e| format!("Event loop error: {}", e))
    }

    pub async fn resolve_promises(&mut self) {
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            self.runtime.run_event_loop(deno_core::PollEventLoopOptions::default()),
        ).await;
    }
    pub fn take_dom(&self) -> Option<DomTree> {
        self.state.borrow_mut().dom.take()
    }

    pub fn with_dom<R>(&self, f: impl FnOnce(&DomTree) -> R) -> Option<R> {
        let state = self.state.borrow();
        state.dom.as_ref().map(f)
    }

    pub fn dom_ref(&self) -> Option<std::cell::Ref<'_, Option<DomTree>>> {
        let r = self.state.borrow();
        if r.dom.is_some() {
            Some(std::cell::Ref::map(r, |s| &s.dom))
        } else {
            None
        }
    }
    fn make_oid(&self, counter: u64) -> String {
        format!("{{\"injectedScriptId\":1,\"id\":{}}}", counter)
    }

    fn wrap_expression(expression: &str) -> String {
        let trimmed = expression.trim();

        let is_multi_statement = trimmed.starts_with("var ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("if ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("return ");

        if is_multi_statement {
            format!(
                "(function() {{ try {{ {} }} catch(e) {{ return null; }} }})()",
                expression
            )
        } else {
            // Strip trailing semicolons + whitespace before wrapping in
            // `return (...);`. Playwright's utility-script expression is
            // an IIFE that ends with `})();` — leaving the `;` in place
            // produces `return (...;);`, a SyntaxError. The script fails
            // to parse, the catch never fires (parse errors are not
            // catchable), and the function silently returns `undefined`.
            // Stripping makes the wrapped expression syntactically valid.
            let cleaned = trimmed.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
            format!(
                "(function() {{ try {{ return ({}); }} catch(e) {{ return null; }} }})()",
                cleaned
            )
        }
    }

    fn meta_extract_js(var_name: &str) -> String {
        format!(
            r#"(function(v) {{
                var t = typeof v;
                var st = null, cn = '', desc = '';
                if (v === null) {{ t = 'object'; st = 'null'; }}
                else if (v === undefined) {{ t = 'undefined'; }}
                else if (Array.isArray(v)) {{
                    st = 'array'; cn = 'Array';
                    desc = 'Array(' + v.length + ')';
                }}
                else if (t === 'object' && typeof v._nid === 'number') {{
                    st = 'node';
                    cn = v.constructor ? v.constructor.name : 'Node';
                    if (v.nodeType === 9) cn = 'HTMLDocument';
                    else if (v.nodeType === 1) cn = 'HTML' + (v.tagName || 'Element').charAt(0) + (v.tagName || 'Element').slice(1).toLowerCase() + 'Element';
                    desc = v.tagName ? v.tagName.toLowerCase() : (v.nodeName || 'node');
                }}
                else if (t === 'function') {{
                    cn = 'Function';
                    desc = v.name ? 'function ' + v.name + '()' : 'function()';
                }}
                else if (t === 'object') {{
                    cn = (v.constructor && v.constructor.name) || 'Object';
                    desc = cn;
                }}
                else {{ desc = String(v); }}
                return JSON.stringify({{type:t,subtype:st,className:cn,description:desc}});
            }})({var_name})"#,
            var_name = var_name,
        )
    }

    fn resolve_this(&self, object_id: Option<&str>) -> String {
        match object_id {
            Some(oid) => {
                if let Some(retrieval) = self.object_store.get(oid) {
                    retrieval.clone()
                } else if oid.starts_with("node-") {
                    let nid = oid.strip_prefix("node-").unwrap_or("0");
                    format!(
                        "(function() {{ \
                            var nid = {}; \
                            var cache = globalThis._cache || new Map(); \
                            if (cache.has(nid)) return cache.get(nid); \
                            return null; \
                        }})()",
                        nid
                    )
                } else {
                    "globalThis".to_string()
                }
            }
            None => "globalThis".to_string(),
        }
    }

    fn build_args(&self, arguments: &[serde_json::Value]) -> (String, String) {
        let mut setup_lines = Vec::new();
        let mut arg_names = Vec::new();

        for (i, arg) in arguments.iter().enumerate() {
            let arg_name = format!("__arg{}", i);
            if let Some(value) = arg.get("value") {
                let json_str = serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_string());
                setup_lines.push(format!("var {} = {};", arg_name, json_str));
            } else if let Some(oid) = arg.get("objectId").and_then(|v| v.as_str()) {
                if let Some(retrieval) = self.object_store.get(oid) {
                    setup_lines.push(format!("var {} = {};", arg_name, retrieval));
                } else {
                    setup_lines.push(format!("var {} = undefined;", arg_name));
                }
            } else if let Some(unser) = arg.get("unserializableValue").and_then(|v| v.as_str()) {
                setup_lines.push(format!("var {} = {};", arg_name, unser));
            } else {
                setup_lines.push(format!("var {} = undefined;", arg_name));
            }
            arg_names.push(arg_name);
        }

        (setup_lines.join("\n"), arg_names.join(", "))
    }

    fn v8_to_json(
        &mut self,
        result: deno_core::v8::Global<deno_core::v8::Value>,
    ) -> Result<serde_json::Value, String> {
        let scope = &mut self.runtime.handle_scope();
        let local = deno_core::v8::Local::new(scope, result);

        if local.is_undefined() || local.is_null() {
            return Ok(serde_json::Value::Null);
        }
        if local.is_boolean() {
            return Ok(serde_json::Value::Bool(local.boolean_value(scope)));
        }
        if local.is_number() {
            let n = local.number_value(scope).unwrap_or(0.0);
            return Ok(serde_json::json!(n));
        }
        if local.is_string() {
            let s = local.to_rust_string_lossy(scope);
            return Ok(serde_json::Value::String(s));
        }

        let global = scope.get_current_context().global(scope);
        let json_obj_str = deno_core::v8::String::new(scope, "JSON").unwrap();
        if let Some(json_obj) = global.get(scope, json_obj_str.into()) {
            if let Some(json_obj) = json_obj.to_object(scope) {
                let stringify_str = deno_core::v8::String::new(scope, "stringify").unwrap();
                if let Some(stringify_fn) = json_obj.get(scope, stringify_str.into()) {
                    if let Ok(stringify_fn) =
                        deno_core::v8::Local::<deno_core::v8::Function>::try_from(stringify_fn)
                    {
                        let args = [local];
                        if let Some(result) = stringify_fn.call(scope, json_obj.into(), &args) {
                            let json_str = result.to_rust_string_lossy(scope);
                            if let Ok(val) = serde_json::from_str(&json_str) {
                                return Ok(val);
                            }
                        }
                    }
                }
            }
        }

        let s = local.to_rust_string_lossy(scope);
        Ok(serde_json::Value::String(s))
    }

    fn info_from_json(value: &serde_json::Value) -> RemoteObjectInfo {
        match value {
            serde_json::Value::Null => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("null".into()),
                class_name: String::new(),
                description: "null".into(),
                object_id: None,
                value: Some(serde_json::Value::Null),
            },
            serde_json::Value::Bool(b) => RemoteObjectInfo {
                js_type: "boolean".into(),
                subtype: None,
                class_name: String::new(),
                description: b.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Number(n) => RemoteObjectInfo {
                js_type: "number".into(),
                subtype: None,
                class_name: String::new(),
                description: n.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::String(s) => RemoteObjectInfo {
                js_type: "string".into(),
                subtype: None,
                class_name: String::new(),
                description: s.clone(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Array(arr) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("array".into()),
                class_name: "Array".into(),
                description: format!("Array({})", arr.len()),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Object(_) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: None,
                class_name: "Object".into(),
                description: "Object".into(),
                object_id: None,
                value: Some(value.clone()),
            },
        }
    }

    fn info_from_meta(
        meta: &serde_json::Value,
        object_id: Option<String>,
    ) -> RemoteObjectInfo {
        let js_type = meta
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("undefined")
            .to_string();
        let subtype = meta
            .get("subtype")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let class_name = meta
            .get("className")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let description = meta
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let value = if js_type != "object" && js_type != "function" {
            meta.get("description")
                .and_then(|v| v.as_str())
                .map(|s| serde_json::Value::String(s.to_string()))
        } else {
            None
        };

        RemoteObjectInfo {
            js_type,
            subtype,
            class_name,
            description,
            object_id,
            value,
        }
    }
}

impl Default for ObscuraJsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_dom::parse_html;

    fn setup_runtime(html: &str) -> ObscuraJsRuntime {
        let dom = parse_html(html);
        let rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.com/test");
        rt.set_title("Test Page");
        rt
    }

    #[test]
    fn test_document_title() {
        let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
        let title = rt.evaluate("document.title").unwrap();
        assert_eq!(title, serde_json::json!("Test Page"));
    }

    #[test]
    fn test_document_url() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let url = rt.evaluate("document.URL").unwrap();
        assert_eq!(url, serde_json::json!("http://example.com/test"));
    }

    #[test]
    fn test_query_selector() {
        let mut rt = setup_runtime("<html><body><h1>Hello</h1><p>World</p></body></html>");
        let text = rt.evaluate("document.querySelector('h1').textContent").unwrap();
        assert_eq!(text, serde_json::json!("Hello"));
    }

    #[test]
    fn test_query_selector_all() {
        let mut rt = setup_runtime("<ul><li>A</li><li>B</li><li>C</li></ul>");
        let count = rt.evaluate("document.querySelectorAll('li').length").unwrap();
        assert_eq!(count.as_f64().unwrap() as i64, 3);
    }

    #[test]
    fn test_get_element_by_id() {
        let mut rt = setup_runtime(r#"<div id="test">Content</div>"#);
        let tag = rt.evaluate("document.getElementById('test').tagName").unwrap();
        assert_eq!(tag, serde_json::json!("DIV"));
    }

    #[test]
    fn test_inner_html() {
        let mut rt = setup_runtime(r#"<div id="x"><p>Hello</p></div>"#);
        let html = rt.evaluate("document.getElementById('x').innerHTML").unwrap();
        assert!(html.as_str().unwrap().contains("<p>"));
    }

    #[test]
    fn test_script_execution() {
        let mut rt = setup_runtime("<ul><li>A</li><li>B</li></ul>");
        rt.execute_script(
            "test",
            r#"
            globalThis.__result = [];
            document.querySelectorAll('li').forEach(function(el) {
                globalThis.__result.push(el.textContent);
            });
        "#,
        )
        .unwrap();
        let result = rt.evaluate("globalThis.__result").unwrap();
        assert_eq!(result, serde_json::json!(["A", "B"]));
    }

    #[test]
    fn test_console_log() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script("test", "console.log('Hello from V8!')").unwrap();
    }

    #[test]
    fn test_location() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let href = rt.evaluate("location.href").unwrap();
        assert_eq!(href, serde_json::json!("http://example.com/test"));
    }

    #[test]
    fn test_navigator() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let ua = rt.evaluate("navigator.userAgent").unwrap();
        assert!(ua.as_str().unwrap().contains("Chrome"), "UA should contain Chrome: {}", ua);
        let wd = rt.evaluate("navigator.webdriver").unwrap();
        assert_eq!(wd, serde_json::Value::Null);
        let plugins = rt.evaluate("navigator.plugins.length").unwrap();
        assert!(plugins.as_f64().unwrap() > 0.0, "Should have plugins");
        let chrome = rt.evaluate("typeof window.chrome").unwrap();
        assert_eq!(chrome, serde_json::json!("object"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_no_args() {
        let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
        let result = rt
            .call_function_on("() => document.title", None, &[], true)
            .await.unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("Test Page"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_with_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![
            serde_json::json!({"value": 10}),
            serde_json::json!({"value": 20}),
        ];
        let result = rt.call_function_on("(a, b) => a + b", None, &args, true).await.unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 30);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_with_string_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![
            serde_json::json!({"value": "hello"}),
            serde_json::json!({"value": " world"}),
        ];
        let result = rt.call_function_on("(a, b) => a + b", None, &args, true).await.unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("hello world"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_with_object_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![serde_json::json!({"value": {"name": "test", "count": 5}})];
        let result = rt
            .call_function_on("(obj) => obj.name + ':' + obj.count", None, &args, true)
            .await.unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("test:5"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_return_object() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on("() => ({a: 1, b: 2})", None, &[], true)
            .await.unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!({"a": 1, "b": 2}));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_object_ref_preserves_methods() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on(
                "() => ({ items: [1,2,3], getLen: function() { return this.items.length; } })",
                None,
                &[],
                false,
            )
            .await.unwrap();
        let oid = result.object_id.unwrap();

        let result2 = rt
            .call_function_on("function() { return this.getLen(); }", Some(&oid), &[], true)
            .await.unwrap();
        assert_eq!(result2.value.unwrap().as_f64().unwrap() as i64, 3);
    }

    #[test]
    fn test_evaluate_for_cdp_detects_node() {
        let mut rt = setup_runtime("<html><body><h1>Hello</h1></body></html>");
        let result = rt
            .evaluate_for_cdp("document.querySelector('h1')", false)
            .unwrap();
        assert_eq!(result.subtype.as_deref(), Some("node"));
        assert_eq!(result.js_type, "object");
        assert!(result.object_id.is_some());
    }

    #[test]
    fn test_evaluate_for_cdp_detects_document() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate_for_cdp("document", false).unwrap();
        assert_eq!(result.subtype.as_deref(), Some("node"));
        assert_eq!(result.class_name, "HTMLDocument");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_dom_interaction() {
        let mut rt = setup_runtime(r#"<div id="items"><span>A</span><span>B</span></div>"#);
        let args = vec![serde_json::json!({"value": "span"})];
        let result = rt
            .call_function_on(
                "(sel) => document.querySelectorAll(sel).length",
                None,
                &args,
                true,
            )
            .await.unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 2);
    }

    #[test]
    fn test_inner_html_setter() {
        let mut rt = setup_runtime(r#"<div id="target"><p>Old</p></div>"#);
        rt.execute_script("test", r#"
            var el = document.getElementById('target');
            el.innerHTML = '<strong>Bold</strong><em>Italic</em>';
        "#).unwrap();
        let result = rt.evaluate("document.getElementById('target').innerHTML").unwrap();
        let html = result.as_str().unwrap();
        assert!(html.contains("<strong>"), "innerHTML should contain <strong>, got: {}", html);
        assert!(html.contains("<em>"), "innerHTML should contain <em>, got: {}", html);
        assert!(!html.contains("Old"), "innerHTML should not contain old content, got: {}", html);
    }

    #[test]
    fn test_inner_html_with_nested() {
        let mut rt = setup_runtime(r#"<div id="root"></div>"#);
        rt.execute_script("test", r#"
            var el = document.getElementById('root');
            el.innerHTML = '<ul><li>A</li><li>B</li><li>C</li></ul>';
        "#).unwrap();
        let count = rt.evaluate("document.querySelectorAll('li').length").unwrap();
        assert_eq!(count.as_f64().unwrap() as i64, 3, "Should find 3 li elements after innerHTML set");

        let text = rt.evaluate("document.querySelector('li').textContent").unwrap();
        assert_eq!(text, serde_json::json!("A"));
    }

    #[test]
    fn test_input_value() {
        let mut rt = setup_runtime(r#"<form><input id="name" type="text" value="initial"><textarea id="bio">old text</textarea></form>"#);
        let val = rt.evaluate("document.getElementById('name').value").unwrap();
        assert_eq!(val, serde_json::json!("initial"));
        rt.execute_script("test", "document.getElementById('name').value = 'new value';").unwrap();
        let val2 = rt.evaluate("document.getElementById('name').value").unwrap();
        assert_eq!(val2, serde_json::json!("new value"));
        let bio = rt.evaluate("document.getElementById('bio').value").unwrap();
        assert_eq!(bio, serde_json::json!("old text"));
    }

    #[test]
    fn test_sequential_runtime_swap() {
        let mut rt1 = setup_runtime("<html><body><h1>Page1</h1></body></html>");
        let title1 = rt1.evaluate("document.querySelector('h1').textContent").unwrap();
        assert_eq!(title1, serde_json::json!("Page1"));

        let dom1 = rt1.take_dom();
        drop(rt1);

        let mut rt2 = setup_runtime("<html><body><h1>Page2</h1></body></html>");
        let title2 = rt2.evaluate("document.querySelector('h1').textContent").unwrap();
        assert_eq!(title2, serde_json::json!("Page2"));
        drop(rt2);

        if let Some(dom) = dom1 {
            let rt1b = ObscuraJsRuntime::new();
            rt1b.set_dom(dom);
            rt1b.set_url("http://example.com");
            rt1b.set_title("Page1");
            let mut rt1b = rt1b;
            let title1b = rt1b.evaluate("document.querySelector('h1').textContent").unwrap();
            assert_eq!(title1b, serde_json::json!("Page1"));
        }
    }

    #[test]
    fn test_checkbox_checked() {
        let mut rt = setup_runtime(r#"<input id="cb" type="checkbox" checked>"#);
        let checked = rt.evaluate("document.getElementById('cb').checked").unwrap();
        assert_eq!(checked, serde_json::json!(true));
        rt.execute_script("test", "document.getElementById('cb').checked = false;").unwrap();
        let checked2 = rt.evaluate("document.getElementById('cb').checked").unwrap();
        assert_eq!(checked2, serde_json::json!(false));
    }

    #[test]
    fn test_matches_and_closest() {
        let mut rt = setup_runtime(r#"<div class="outer"><div class="inner"><span id="target">Hi</span></div></div>"#);
        let matches = rt.evaluate("document.getElementById('target').matches('span')").unwrap();
        assert_eq!(matches, serde_json::json!(true));
        let closest = rt.evaluate("document.getElementById('target').closest('.outer').className").unwrap();
        assert_eq!(closest, serde_json::json!("outer"));
        let no_match = rt.evaluate("document.getElementById('target').closest('.nonexistent')").unwrap();
        assert_eq!(no_match, serde_json::Value::Null);
    }

    #[test]
    fn test_clone_node_deep() {
        let mut rt = setup_runtime(r#"<div id="src"><p>A</p><p>B</p></div>"#);
        rt.execute_script("test", r#"
            var src = document.getElementById('src');
            var clone = src.cloneNode(true);
            document.body.appendChild(clone);
        "#).unwrap();
        let count = rt.evaluate("document.querySelectorAll('p').length").unwrap();
        assert!(count.as_f64().unwrap() as i64 >= 4, "Deep clone should duplicate <p> children, got: {}", count);
    }

    #[test]
    fn test_evaluate_multistatement() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("var x = 5; var y = 10; return x + y;").unwrap();
        assert_eq!(result.as_f64().unwrap() as i64, 15);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_object_ref_as_argument() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let obj = rt
            .call_function_on("() => ({ x: 42 })", None, &[], false)
            .await.unwrap();
        let oid = obj.object_id.unwrap();

        let args = vec![serde_json::json!({"objectId": oid})];
        let result = rt
            .call_function_on("(obj) => obj.x * 2", None, &args, true)
            .await.unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 84);
    }

    fn setup_runtime_with_cookies(html: &str) -> (ObscuraJsRuntime, std::sync::Arc<obscura_net::CookieJar>) {
        let dom = obscura_dom::parse_html(html);
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.com/test");
        rt.set_title("Test Page");
        rt.set_cookie_jar(jar.clone());
        (rt, jar)
    }

    #[test]
    fn test_document_cookie_reads_http_cookies() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("session=abc123; Path=/", &url);
        jar.set_cookie("theme=dark; Path=/", &url);
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(cookie_str.contains("session=abc123"), "expected session cookie, got: {}", cookie_str);
        assert!(cookie_str.contains("theme=dark"), "expected theme cookie, got: {}", cookie_str);
    }

    #[test]
    fn test_document_cookie_excludes_httponly() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("visible=yes; Path=/", &url);
        jar.set_cookie("secret=token; Path=/; HttpOnly", &url);
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(cookie_str.contains("visible=yes"), "expected visible cookie, got: {}", cookie_str);
        assert!(!cookie_str.contains("secret"), "httpOnly cookie should not be visible to JS, got: {}", cookie_str);
    }

    #[test]
    fn test_document_cookie_setter_stores_in_jar() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        rt.evaluate("document.cookie = 'foo=bar; Path=/'").unwrap();
        let url = url::Url::parse("http://example.com/test").unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        assert!(result.as_str().unwrap().contains("foo=bar"));
        let header = jar.get_cookie_header(&url);
        assert!(header.contains("foo=bar"), "cookie should be in jar, got: {}", header);
    }

    #[test]
    fn test_document_cookie_delete_via_max_age() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        rt.evaluate("document.cookie = 'temp=val; Path=/'").unwrap();
        assert!(rt.evaluate("document.cookie").unwrap().as_str().unwrap().contains("temp=val"));
        rt.evaluate("document.cookie = 'temp=; Max-Age=0'").unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        assert!(!result.as_str().unwrap().contains("temp="), "cookie should be deleted, got: {}", result);
        assert!(!jar.get_cookie_header(&url).contains("temp="));
    }

    #[test]
    fn test_document_cookie_js_and_http_merge() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("server_sid=xyz; Path=/", &url);
        rt.evaluate("document.cookie = 'client_pref=light'").unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(cookie_str.contains("server_sid=xyz"), "expected server cookie, got: {}", cookie_str);
        assert!(cookie_str.contains("client_pref=light"), "expected client cookie, got: {}", cookie_str);
    }

    #[test]
    fn test_document_cookie_empty_when_no_cookies() {
        let (mut rt, _jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let result = rt.evaluate("document.cookie").unwrap();
        assert_eq!(result.as_str().unwrap(), "");
    }

    #[test]
    fn test_document_cookie_no_jar_returns_empty() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("document.cookie").unwrap();
        assert_eq!(result.as_str().unwrap(), "");
    }

    #[test]
    fn test_document_write_appends_to_body() {
        let mut rt = setup_runtime("<html><body><p>Existing</p></body></html>");
        rt.evaluate("document.write('<div>Added</div>')").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        let body = html.as_str().unwrap();
        assert!(body.contains("Existing"), "existing content should remain, got: {}", body);
        assert!(body.contains("Added"), "written content should appear, got: {}", body);
    }

    #[test]
    fn test_document_writeln() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("document.writeln('Hello')").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        assert!(html.as_str().unwrap().contains("Hello"));
    }

    #[test]
    fn test_document_write_multiple_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("document.write('Hello', ' ', 'World')").unwrap();
        let text = rt.evaluate("document.body.textContent").unwrap();
        assert_eq!(text.as_str().unwrap().trim(), "Hello World");
    }

    #[test]
    fn test_document_open_clears_body() {
        let mut rt = setup_runtime("<html><body><p>Old content</p></body></html>");
        rt.evaluate("document.open()").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        assert_eq!(html.as_str().unwrap(), "");
    }

    #[test]
    fn test_document_write_html_elements() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate(r#"document.write('<h1 id="title">Test</h1><p>Para</p>')"#).unwrap();
        let h1 = rt.evaluate("document.querySelector('h1').textContent").unwrap();
        assert_eq!(h1.as_str().unwrap(), "Test");
        let p = rt.evaluate("document.querySelector('p').textContent").unwrap();
        assert_eq!(p.as_str().unwrap(), "Para");
    }

    #[test]
    fn test_url_relative_resolution() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("new URL('data.json', 'http://example.com/path/page.html').href").unwrap();
        assert_eq!(result.as_str().unwrap(), "http://example.com/path/data.json");

        let result = rt.evaluate("new URL('/api/data', 'http://example.com/path/page.html').href").unwrap();
        assert_eq!(result.as_str().unwrap(), "http://example.com/api/data");

        let result = rt.evaluate("new URL('https://other.com/foo', 'http://example.com/bar').href").unwrap();
        assert_eq!(result.as_str().unwrap(), "https://other.com/foo");

        let result = rt.evaluate("new URL('sub/file.js', 'http://example.com/a/b/c.html').href").unwrap();
        assert_eq!(result.as_str().unwrap(), "http://example.com/a/b/sub/file.js");

        let result = rt.evaluate("new URL('api.json', 'http://localhost:8080/dir/index.html').href").unwrap();
        assert_eq!(result.as_str().unwrap(), "http://localhost:8080/dir/api.json");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_fetch_url_input_decodes_binary_body_base64() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.call_function_on_for_cdp(
            r#"async () => {
                const originalFetchOp = Deno.core.ops.op_fetch_url;
                try {
                    Deno.core.ops.op_fetch_url = (url) => {
                        globalThis.__capturedFetchUrl = url;
                        return JSON.stringify({
                            status: 200,
                            headers: { "content-type": "application/wasm" },
                            bodyBase64: "AGFzbQEAAAA=",
                            url,
                        });
                    };
                    const response = await fetch(new URL("/pkg/app_bg.wasm", document.URL));
                    const bytes = Array.from(new Uint8Array(await response.arrayBuffer()));
                    return { url: globalThis.__capturedFetchUrl, bytes };
                } finally {
                    Deno.core.ops.op_fetch_url = originalFetchOp;
                }
            }"#,
            None,
            &[],
            true,
            true,
        ).await.unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "url": "http://example.com/pkg/app_bg.wasm",
                "bytes": [0, 97, 115, 109, 1, 0, 0, 0],
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_response_array_buffer_preserves_typed_array_view() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.call_function_on_for_cdp(
            r#"async () => {
                const bytes = new Uint8Array([9, 0, 97, 115, 109, 1, 8]);
                const response = new Response(bytes.subarray(1, 6));
                return Array.from(new Uint8Array(await response.arrayBuffer()));
            }"#,
            None,
            &[],
            true,
            true,
        ).await.unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!([0, 97, 115, 109, 1]));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wasm_instantiate_streaming_uses_response_array_buffer() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.call_function_on_for_cdp(
            r#"async () => {
                const bytes = new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]);
                const result = await WebAssembly.instantiateStreaming(
                    Promise.resolve(new Response(bytes)),
                    {},
                );
                return result.instance instanceof WebAssembly.Instance;
            }"#,
            None,
            &[],
            true,
            true,
        ).await.unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!(true));
    }

    #[test]
    fn test_text_decoder_respects_typed_array_view() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate(
            "new TextDecoder().decode(new Uint8Array([65, 66, 67]).subarray(1, 2))"
        ).unwrap();
        assert_eq!(result.as_str().unwrap(), "B");
    }

    #[test]
    fn test_document_doctype() {
        let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
        let result = rt.evaluate("document.doctype !== null").unwrap();
        assert_eq!(result, serde_json::json!(true));

        let name = rt.evaluate("document.doctype.name").unwrap();
        assert_eq!(name, serde_json::json!("html"));

        let node_type = rt.evaluate("document.doctype.nodeType").unwrap();
        assert_eq!(node_type.as_f64().unwrap() as i64, 10);
    }

    #[test]
    fn test_document_doctype_null_when_missing() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("document.doctype === null").unwrap();
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_xml_serializer_doctype() {
        let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
        let result = rt.evaluate(
            "new XMLSerializer().serializeToString(document.doctype)"
        ).unwrap();
        assert_eq!(result.as_str().unwrap(), "<!DOCTYPE html>");
    }

    #[test]
    fn test_xml_serializer_element() {
        let mut rt = setup_runtime(r#"<html><body><div id="x">Hello</div></body></html>"#);
        let result = rt.evaluate(
            "new XMLSerializer().serializeToString(document.getElementById('x'))"
        ).unwrap();
        let html = result.as_str().unwrap();
        assert!(html.contains("<div"));
        assert!(html.contains("Hello"));
    }

    #[test]
    fn test_create_event_custom_event_has_init_method() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let kind = rt
            .evaluate("typeof document.createEvent('CustomEvent').initCustomEvent")
            .unwrap();
        assert_eq!(kind, serde_json::json!("function"));
    }

    #[test]
    fn test_init_custom_event_sets_fields() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "test",
            r#"
            globalThis.__e = document.createEvent('CustomEvent');
            globalThis.__e.initCustomEvent('myevent', true, false, {hello: 'world'});
        "#,
        )
        .unwrap();
        let t = rt.evaluate("globalThis.__e.type").unwrap();
        assert_eq!(t, serde_json::json!("myevent"));
        let b = rt.evaluate("globalThis.__e.bubbles").unwrap();
        assert_eq!(b, serde_json::json!(true));
        let c = rt.evaluate("globalThis.__e.cancelable").unwrap();
        assert_eq!(c, serde_json::json!(false));
        let d = rt.evaluate("globalThis.__e.detail.hello").unwrap();
        assert_eq!(d, serde_json::json!("world"));
    }

    #[test]
    fn test_create_event_returns_correct_class() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let cust = rt
            .evaluate("document.createEvent('CustomEvent') instanceof CustomEvent")
            .unwrap();
        assert_eq!(cust, serde_json::json!(true));
        let mouse = rt
            .evaluate("document.createEvent('MouseEvent') instanceof MouseEvent")
            .unwrap();
        assert_eq!(mouse, serde_json::json!(true));
        let mouses = rt
            .evaluate("document.createEvent('MouseEvents') instanceof MouseEvent")
            .unwrap();
        assert_eq!(mouses, serde_json::json!(true));
        let kb = rt
            .evaluate("document.createEvent('KeyboardEvent') instanceof KeyboardEvent")
            .unwrap();
        assert_eq!(kb, serde_json::json!(true));
    }

    #[test]
    fn test_create_event_unknown_type_returns_event() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let kind = rt
            .evaluate("document.createEvent('NotARealType') instanceof Event")
            .unwrap();
        assert_eq!(kind, serde_json::json!(true));
    }

    #[test]
    fn test_page_content_puppeteer_pattern() {
        let mut rt = setup_runtime("<!DOCTYPE html><html><head></head><body><p>Test</p></body></html>");
        let result = rt.evaluate(
            "(function() { let retVal = ''; if (document.doctype) retVal = new XMLSerializer().serializeToString(document.doctype); if (document.documentElement) retVal += document.documentElement.outerHTML; return retVal; })()"
        ).unwrap();
        let html = result.as_str().unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html>"));
        assert!(html.contains("<p>Test</p>"));
    }

}
