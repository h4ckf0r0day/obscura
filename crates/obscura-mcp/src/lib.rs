// The tools/list JSON literal expanded by serde_json::json! is large
// enough now (32 tool definitions) that the default macro recursion
// limit (128) overflows. Bumping for this crate only.
#![recursion_limit = "512"]

pub mod http;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
#[cfg(feature = "render")]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use obscura_browser::{BrowserContext, Page};
use obscura_dom::NodeId;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Cap on text returned to the agent unless the caller passes a larger
/// `max_chars`. Agents waste context on multi-KB raw page dumps; this
/// keeps a single tool call from burning a window's worth of tokens.
/// Override via tool args.
const DEFAULT_TEXT_LIMIT: usize = 4000;

#[derive(Deserialize)]
struct RpcMessage {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

impl RpcResponse {
    fn ok(id: Value, result: Value) -> Self {
        RpcResponse { jsonrpc: "2.0", id, result: Some(result), error: None }
    }

    fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        RpcResponse { jsonrpc: "2.0", id, result: None, error: Some(RpcError { code, message: message.into() }) }
    }
}

pub struct BrowserState {
    /// Open tabs keyed by tab_id (e.g. "tab-1"). BTreeMap so list ordering
    /// is stable across calls (agents reason about "tab #2" deterministically).
    tabs: std::collections::BTreeMap<String, Page>,
    /// The tab id every tool call operates on. None means there are no
    /// open tabs; the next page_mut() call creates one.
    active_tab: Option<String>,
    tab_counter: u32,
    context: Arc<BrowserContext>,
    user_agent: Option<String>,
    console_messages: Vec<String>,
    /// Element-ref table from the last `browser_snapshot` on the ACTIVE
    /// tab. Agents click / fill / type by `ref` (e.g. `"e3"`) instead of
    /// guessing a CSS selector. Refs are stable within a snapshot; the
    /// table is wiped on every navigation / tab switch and refilled on
    /// the next snapshot call.
    interactive_refs: HashMap<String, NodeId>,
}

impl BrowserState {
    pub fn new(proxy: Option<String>, user_agent: Option<String>, stealth: bool) -> Self {
        BrowserState {
            tabs: std::collections::BTreeMap::new(),
            active_tab: None,
            tab_counter: 0,
            context: Arc::new(BrowserContext::with_options("mcp".to_string(), proxy, stealth)),
            user_agent,
            console_messages: Vec::new(),
            interactive_refs: HashMap::new(),
        }
    }

    /// Make sure there is at least one tab and return a &mut to the
    /// active tab's Page. Auto-creates a default tab if none exist so
    /// every legacy single-page tool continues to work without
    /// requiring an explicit browser_tab_new.
    fn page_mut(&mut self) -> &mut Page {
        if self.active_tab.is_none() {
            self.tab_counter += 1;
            let id = format!("tab-{}", self.tab_counter);
            self.tabs.insert(id.clone(), Page::new("mcp-page".to_string(), self.context.clone()));
            self.active_tab = Some(id);
        }
        let id = self.active_tab.as_ref().unwrap().clone();
        self.activate(&id);
        self.tabs.get_mut(&id).expect("active tab must exist")
    }

    fn new_tab(&mut self) -> String {
        self.tab_counter += 1;
        let id = format!("tab-{}", self.tab_counter);
        self.tabs.insert(id.clone(), Page::new(format!("mcp-{id}"), self.context.clone()));
        self.active_tab = Some(id.clone());
        self.interactive_refs.clear();
        id
    }

    /// Enforce the single-live-isolate invariant. rusty_v8 enters each V8
    /// isolate on creation and requires isolates be dropped in reverse order of
    /// creation, so keeping more than one tab's isolate live at once and then
    /// dropping a non-newest one aborts the whole process (#258). Suspend every
    /// other tab (drops its isolate, keeps its DOM in self.dom) and make the
    /// active tab the only live isolate, mirroring the CDP server's
    /// Dispatcher::get_session_page_mut.
    fn activate(&mut self, tab_id: &str) {
        for (id, page) in self.tabs.iter_mut() {
            if id.as_str() != tab_id && page.has_js() {
                page.suspend_js();
            }
        }
        if let Some(page) = self.tabs.get_mut(tab_id) {
            page.resume_js();
        }
    }

    /// Close a tab without breaking the LIFO isolate-drop rule. suspend_js drops
    /// this tab's isolate (if it is the live one) while it is still the only
    /// entered isolate, so the following remove disposes no isolate (#258).
    fn close_tab(&mut self, tab_id: &str) -> bool {
        if let Some(page) = self.tabs.get_mut(tab_id) {
            page.suspend_js();
        }
        self.tabs.remove(tab_id).is_some()
    }

    fn has_active_page_runtime(&self) -> bool {
        self.active_tab
            .as_ref()
            .and_then(|tab_id| self.tabs.get(tab_id))
            .is_some_and(Page::has_js)
    }

    /// Advance the active page by one wake-driven browser task and immediately
    /// consume any navigation that task queued. MCP owns its pages continuously,
    /// so leaving either half for the next tool call strands timers, fetches,
    /// and location/form/click navigations while the transport waits on stdin.
    async fn advance_active_page_tasks(&mut self) -> Result<bool, String> {
        let page = self.page_mut();
        let reached_idle = page.run_autonomous_event_loop_turn().await?;
        let navigated = page
            .process_pending_navigation()
            .await
            .map_err(|error| error.to_string())?;
        if navigated {
            self.interactive_refs.clear();
        }
        Ok(reached_idle && !navigated)
    }

    /// Resolve `ref=eN` to a CSS selector that uniquely targets the
    /// element. Snapshot writes `data-obscura-ref="eN"` onto every
    /// interactable, so the attribute survives across calls as long as
    /// the page isn't re-rendered without it. Returns `Err` if the ref
    /// hasn't been registered (caller must call browser_snapshot first).
    fn ref_to_selector(&self, r: &str) -> Result<String, String> {
        if !self.interactive_refs.contains_key(r) {
            return Err(format!(
                "unknown ref '{r}'; call browser_snapshot first to refresh the ref table"
            ));
        }
        Ok(format!("[data-obscura-ref=\"{r}\"]"))
    }
}

pub(crate) async fn dispatch(method: &str, id: Value, params: &Value, state: &mut BrowserState) -> RpcResponse {
    match method {
        "initialize" => handle_initialize(id, params),
        "ping" => RpcResponse::ok(id, json!({})),
        "tools/list" => handle_tools_list(id),
        "tools/call" => handle_tool_call(id, params, state).await,
        "resources/list" => RpcResponse::ok(id, json!({"resources": []})),
        "prompts/list" => RpcResponse::ok(id, json!({"prompts": []})),
        _ => RpcResponse::err(id, -32601, format!("Unknown method: {method}")),
    }
}

pub async fn run(proxy: Option<String>, user_agent: Option<String>, stealth: bool) -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = stdout;

    let mut state = BrowserState::new(proxy, user_agent, stealth);
    let mut runtime_pump_armed = false;

    loop {
        // MCP stdio transport: newline-delimited JSON (one message per line)
        let mut line = String::new();
        let n = if runtime_pump_armed {
            tokio::select! {
                biased;
                read = reader.read_line(&mut line) => Some(read?),
                pump_result = state.advance_active_page_tasks() => {
                    match pump_result {
                        Ok(reached_idle) => runtime_pump_armed = !reached_idle,
                        Err(error) => {
                            runtime_pump_armed = false;
                            eprintln!("MCP page task failed: {error}");
                        }
                    }
                    None
                }
            }
        } else {
            Some(reader.read_line(&mut line).await?)
        };
        let Some(n) = n else {
            continue;
        };
        if n == 0 {
            return Ok(());
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: RpcMessage = match serde_json::from_str(trimmed) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Notifications (no id) need no response
        if msg.id.is_none() {
            continue;
        }

        let id = msg.id.clone().unwrap_or(Value::Null);
        let response = dispatch(&msg.method, id, &msg.params, &mut state).await;
        runtime_pump_armed = state.has_active_page_runtime();

        let mut body = serde_json::to_string(&response)?;
        body.push('\n');
        writer.write_all(body.as_bytes()).await?;
        writer.flush().await?;
    }
}

fn handle_initialize(id: Value, params: &Value) -> RpcResponse {
    let _client_version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    RpcResponse::ok(id, json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "obscura-mcp",
            "version": env!("CARGO_PKG_VERSION")
        }
    }))
}

fn handle_tools_list(id: Value) -> RpcResponse {
    #[allow(unused_mut)]
    let mut tools = json!([
            {
                "name": "browser_navigate",
                "description": "Navigate to a URL and wait for the page to load",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "URL to navigate to" },
                        "waitUntil": {
                            "type": "string",
                            "enum": ["load", "domcontentloaded", "networkidle0"],
                            "description": "Navigation wait condition (default: load)"
                        }
                    },
                    "required": ["url"]
                }
            },
            {
                "name": "browser_snapshot",
                "description": "Get the current page content as text (title, URL, and readable body text)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "max_chars": { "type": "number", "minimum": 0, "description": "Truncate readable body text to this many characters (default: 4000)" }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "browser_click",
                "description": "Click an element. Pass `ref` (preferred, from browser_snapshot / browser_interactive_elements) OR a `selector`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "string", "description": "Element ref like 'e3' from a recent snapshot" },
                        "selector": { "type": "string", "description": "CSS selector (fallback if ref unavailable)" }
                    }
                }
            },
            {
                "name": "browser_fill",
                "description": "Set the value of an input element. Pass `ref` (preferred) OR `selector`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "string" },
                        "selector": { "type": "string" },
                        "value": { "type": "string", "description": "Value to set" }
                    },
                    "required": ["value"]
                }
            },
            {
                "name": "browser_type",
                "description": "Type text into an input element (appends to existing value). Pass `ref` (preferred) OR `selector`.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "string" },
                        "selector": { "type": "string" },
                        "text": { "type": "string", "description": "Text to type" }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "browser_press_key",
                "description": "Dispatch a keyboard event on an element or the document",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string", "description": "Key name (e.g. Enter, Tab, Escape)" },
                        "selector": { "type": "string", "description": "CSS selector (optional, defaults to document)" }
                    },
                    "required": ["key"]
                }
            },
            {
                "name": "browser_select_option",
                "description": "Select an option from a <select> element",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "CSS selector of the <select> element" },
                        "value": { "type": "string", "description": "Value or text of the option to select" }
                    },
                    "required": ["selector", "value"]
                }
            },
            {
                "name": "browser_evaluate",
                "description": "Evaluate a JavaScript expression in the page context and return the result",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "expression": { "type": "string", "description": "JavaScript expression to evaluate" }
                    },
                    "required": ["expression"]
                }
            },
            {
                "name": "browser_wait_for",
                "description": "Wait for a CSS selector to appear in the DOM",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "CSS selector to wait for" },
                        "timeout": { "type": "number", "description": "Timeout in seconds (default: 30)" }
                    },
                    "required": ["selector"]
                }
            },
            {
                "name": "browser_network_requests",
                "description": "Return the list of network requests made by the current page",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "browser_console_messages",
                "description": "Return the console messages logged by the current page",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "browser_close",
                "description": "Close the current browser page and reset state",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "browser_markdown",
                "description": "Extract the current page as Markdown (headings, paragraphs, lists, links, code blocks). Use this instead of browser_snapshot when you want token-dense structured content rather than plain text.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "max_chars": { "type": "number", "description": "Truncate to this many characters (default 4000)" }
                    }
                }
            },
            {
                "name": "browser_links",
                "description": "List every anchor link on the current page as one JSON object per line: {text, href}. Use when you need to enumerate where to navigate next.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "number", "description": "Max number of links to return (default 100)" },
                        "internal_only": { "type": "boolean", "description": "If true, only return links on the same origin as the current page" }
                    }
                }
            },
            {
                "name": "browser_interactive_elements",
                "description": "List every clickable / typeable element on the current page with a stable ref ID and a brief description. Use this BEFORE clicking or filling so you can refer to elements by ref instead of guessing a CSS selector. Refs look like 'e3' and stay valid until the next navigation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "number", "description": "Max number of elements (default 100)" }
                    }
                }
            },
            {
                "name": "browser_back",
                "description": "Navigate back in the page history (equivalent to the browser back button).",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "browser_forward",
                "description": "Navigate forward in the page history.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "browser_reload",
                "description": "Reload the current page.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "browser_get_cookies",
                "description": "Return all cookies in the browser's cookie jar as one JSON object per line.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "domain": { "type": "string", "description": "Filter to cookies on this domain (default: all)" }
                    }
                }
            },
            {
                "name": "browser_set_cookie",
                "description": "Add or replace a cookie in the jar. Use this to skip a login flow when you already have a session token.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "value": { "type": "string" },
                        "domain": { "type": "string", "description": "e.g. example.com or .example.com" },
                        "path": { "type": "string", "description": "default '/'" },
                        "secure": { "type": "boolean" },
                        "http_only": { "type": "boolean" }
                    },
                    "required": ["name", "value", "domain"]
                }
            },
            {
                "name": "browser_clear_cookies",
                "description": "Wipe every cookie from the jar.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "browser_wait_for_text",
                "description": "Wait until a substring appears anywhere in the rendered page text. Use when you want to wait for a result message or notification rather than a specific selector.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "timeout": { "type": "number", "description": "Seconds (default 30)" }
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "browser_detect_forms",
                "description": "List every <form> on the page with its action URL, method, and a description of each input/textarea/select. Use to understand a form's structure before filling it in.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "browser_fill_form",
                "description": "Fill multiple inputs in one call. `fields` is an array of {ref?, selector?, value, type?}. type='text' (default) sets value, type='check'/'uncheck' toggles checkboxes, type='select' picks an option by value or visible text. Saves N round-trips vs N browser_fill calls.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "fields": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "ref": { "type": "string" },
                                    "selector": { "type": "string" },
                                    "value": { "type": "string" },
                                    "type": { "type": "string", "enum": ["text", "check", "uncheck", "select"] }
                                }
                            }
                        },
                        "submit_ref": { "type": "string", "description": "Optional: click this element after filling (e.g. submit button ref)" },
                        "submit_selector": { "type": "string" }
                    },
                    "required": ["fields"]
                }
            },
            {
                "name": "browser_scroll",
                "description": "Scroll the page or an element. `direction` is 'top'|'bottom'|'up'|'down'|'left'|'right' (default 'down'). `amount` in pixels (default viewport height). Use 'bottom' to trigger infinite-scroll loaders.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "direction": { "type": "string", "enum": ["top", "bottom", "up", "down", "left", "right"] },
                        "amount": { "type": "number", "description": "Pixels (default: one viewport)" },
                        "ref": { "type": "string", "description": "Optional element to scroll into view" },
                        "selector": { "type": "string" }
                    }
                }
            },
            {
                "name": "browser_get_attribute",
                "description": "Read an attribute of an element (href, src, value, class, data-*, etc.). Returns the raw attribute value as a string, or empty string if missing.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "ref": { "type": "string" },
                        "selector": { "type": "string" },
                        "attribute": { "type": "string", "description": "Attribute name (e.g. href, value, src)" }
                    },
                    "required": ["attribute"]
                }
            },
            {
                "name": "browser_count",
                "description": "Count how many elements on the page match a CSS selector. Cheap existence / pagination probe.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string" }
                    },
                    "required": ["selector"]
                }
            },
            {
                "name": "browser_extract",
                "description": "Extract a structured object from the page given a map of {field_name: css_selector}. Returns one JSON object with each field set to the matching element's text content (or attribute via 'selector@attr' syntax, e.g. 'a@href'). For list extraction, append '[]' to the field name (e.g. 'rows[]') and the value will be an array.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "schema": {
                            "type": "object",
                            "description": "Map of field_name to CSS selector. Suffix selector with '@attr' for attribute, suffix field name with '[]' for array."
                        }
                    },
                    "required": ["schema"]
                }
            },
            {
                "name": "browser_tab_new",
                "description": "Open a new tab (isolated browser page). Returns the tab ID; subsequent tool calls operate on the most recently opened or browser_tab_switch'd tab.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "Optional URL to navigate the new tab to" }
                    }
                }
            },
            {
                "name": "browser_tab_list",
                "description": "List all open tabs with their ID, URL, title, and which one is active.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "browser_tab_switch",
                "description": "Switch the active tab. All subsequent tool calls (snapshot, click, etc.) target this tab.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": "string" }
                    },
                    "required": ["tab_id"]
                }
            },
            {
                "name": "browser_tab_close",
                "description": "Close a tab by ID (default: the active tab). If you close the active tab, the next remaining tab becomes active.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tab_id": { "type": "string" }
                    }
                }
            },
            {
                "name": "browser_search",
                "description": "Find substring matches in the visible page text. Returns each match with its surrounding context. Use this to confirm content exists before scraping or to locate a section.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "case_sensitive": { "type": "boolean" },
                        "limit": { "type": "number", "description": "Max matches to return (default 10)" },
                        "context_chars": { "type": "number", "description": "Chars on each side of the match (default 80)" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "browser_storage_state",
                "description": "Export the full authentication / session state (cookies + localStorage + sessionStorage) as a JSON object. Save this to skip a login on a subsequent run via browser_set_storage_state.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "browser_set_storage_state",
                "description": "Restore session state previously returned by browser_storage_state. Pass the JSON object. Use to bring an authenticated session back without re-logging in.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "state": {
                            "type": "object",
                            "description": "{cookies: [...], origins: [{origin, localStorage: [...], sessionStorage: [...]}]}"
                        }
                    },
                    "required": ["state"]
                }
            }
    ]).as_array().cloned().expect("MCP tool list must be an array");

    #[cfg(feature = "render")]
    {
        tools.extend([
            json!({
                "name": "browser_screenshot",
                "description": "Capture the current rendered viewport as a PNG image. Width and height default to the page's current CSS viewport.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "width": { "type": "number", "exclusiveMinimum": 0, "maximum": 32768, "description": "Optional CSS-pixel capture width" },
                        "height": { "type": "number", "exclusiveMinimum": 0, "maximum": 32768, "description": "Optional CSS-pixel capture height" }
                    },
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "browser_pdf",
                "description": "Export the current rendered document as a paginated raster PDF using print media and bounded PDF defaults.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "landscape": { "type": "boolean" },
                        "print_background": { "type": "boolean" },
                        "scale": { "type": "number", "minimum": 0.1, "maximum": 2.0 },
                        "paper_width": { "type": "number", "exclusiveMinimum": 0, "maximum": 200, "description": "Paper width in inches" },
                        "paper_height": { "type": "number", "exclusiveMinimum": 0, "maximum": 200, "description": "Paper height in inches" },
                        "margin_top": { "type": "number", "minimum": 0, "description": "Top margin in inches" },
                        "margin_bottom": { "type": "number", "minimum": 0, "description": "Bottom margin in inches" },
                        "margin_left": { "type": "number", "minimum": 0, "description": "Left margin in inches" },
                        "margin_right": { "type": "number", "minimum": 0, "description": "Right margin in inches" }
                    },
                    "additionalProperties": false
                }
            }),
        ]);
    }

    RpcResponse::ok(id, json!({ "tools": tools }))
}

async fn handle_tool_call(id: Value, params: &Value, state: &mut BrowserState) -> RpcResponse {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => return RpcResponse::err(id, -32602, "Missing tool name"),
    };
    let args = params.get("arguments").unwrap_or(&Value::Null);

    #[cfg(feature = "render")]
    {
        let media_result = match name {
            "browser_screenshot" => Some(tool_screenshot(args, state).await),
            "browser_pdf" => Some(tool_pdf(args, state).await),
            _ => None,
        };
        if let Some(result) = media_result {
            return match result {
                Ok(content) => RpcResponse::ok(id, json!({ "content": [content] })),
                Err(error) => RpcResponse::ok(id, json!({
                    "content": [{ "type": "text", "text": format!("Error: {error}") }],
                    "isError": true
                })),
            };
        }
    }

    let result = match name {
        "browser_navigate" => tool_navigate(args, state).await,
        "browser_snapshot" => tool_snapshot(args, state),
        "browser_click" => tool_click(args, state),
        "browser_fill" => tool_fill(args, state),
        "browser_type" => tool_type(args, state),
        "browser_press_key" => tool_press_key(args, state),
        "browser_select_option" => tool_select_option(args, state),
        "browser_evaluate" => tool_evaluate(args, state),
        "browser_wait_for" => tool_wait_for(args, state).await,
        "browser_network_requests" => tool_network_requests(state),
        "browser_console_messages" => tool_console_messages(state),
        "browser_close" => tool_close(state),
        // Tier 1 agent-UX additions
        "browser_markdown" => tool_markdown(args, state),
        "browser_links" => tool_links(args, state),
        "browser_interactive_elements" => tool_interactive_elements(args, state),
        "browser_back" => tool_back(state).await,
        "browser_forward" => tool_forward(state).await,
        "browser_reload" => tool_reload(state).await,
        "browser_get_cookies" => tool_get_cookies(args, state),
        "browser_set_cookie" => tool_set_cookie(args, state),
        "browser_clear_cookies" => tool_clear_cookies(state),
        "browser_wait_for_text" => tool_wait_for_text(args, state).await,
        // Tier 2 agent-UX additions
        "browser_detect_forms" => tool_detect_forms(state),
        "browser_fill_form" => tool_fill_form(args, state),
        "browser_scroll" => tool_scroll(args, state),
        "browser_get_attribute" => tool_get_attribute(args, state),
        "browser_count" => tool_count(args, state),
        "browser_extract" => tool_extract(args, state),
        "browser_tab_new" => tool_tab_new(args, state).await,
        "browser_tab_list" => tool_tab_list(state),
        "browser_tab_switch" => tool_tab_switch(args, state),
        "browser_tab_close" => tool_tab_close(args, state),
        "browser_search" => tool_search(args, state),
        "browser_storage_state" => tool_storage_state(state),
        "browser_set_storage_state" => tool_set_storage_state(args, state),
        _ => Err(format!("Unknown tool: {name}")),
    };

    match result {
        Ok(content) => RpcResponse::ok(id, json!({
            "content": [{ "type": "text", "text": content }]
        })),
        Err(e) => RpcResponse::ok(id, json!({
            "content": [{ "type": "text", "text": format!("Error: {e}") }],
            "isError": true
        })),
    }
}

#[cfg(feature = "render")]
fn validate_tool_options(args: &Value, allowed: &[&str]) -> Result<(), String> {
    if args.is_null() {
        return Ok(());
    }
    let object = args.as_object().ok_or("tool arguments must be an object")?;
    if let Some(name) = object.keys().find(|name| !allowed.contains(&name.as_str())) {
        return Err(format!("unsupported option '{name}'"));
    }
    Ok(())
}

#[cfg(feature = "render")]
fn optional_number(args: &Value, name: &str) -> Result<Option<f32>, String> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let value = value.as_f64().ok_or_else(|| format!("'{name}' must be a number"))?;
    if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err(format!("'{name}' must be a finite number"));
    }
    Ok(Some(value as f32))
}

#[cfg(feature = "render")]
fn optional_bool(args: &Value, name: &str) -> Result<Option<bool>, String> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value.as_bool().map(Some).ok_or_else(|| format!("'{name}' must be a boolean"))
}

#[cfg(feature = "render")]
fn validate_screenshot_viewport(viewport: (f32, f32)) -> Result<(), String> {
    const MAX_DIMENSION: f32 = 32_768.0;
    const MAX_PIXELS: f64 = (16 * 1024 * 1024) as f64;
    let (width, height) = viewport;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0
        || width > MAX_DIMENSION || height > MAX_DIMENSION
    {
        return Err("screenshot dimensions must be finite, positive, and at most 32768 CSS pixels".into());
    }
    if f64::from(width.ceil()) * f64::from(height.ceil()) > MAX_PIXELS {
        return Err("screenshot dimensions exceed the 16-megapixel capture limit".into());
    }
    Ok(())
}

#[cfg(feature = "render")]
async fn tool_screenshot(args: &Value, state: &mut BrowserState) -> Result<Value, String> {
    validate_tool_options(args, &["width", "height"])?;
    let current = state.page_mut().viewport;
    let viewport = (
        optional_number(args, "width")?.unwrap_or(current.0),
        optional_number(args, "height")?.unwrap_or(current.1),
    );
    validate_screenshot_viewport(viewport)?;

    let page = state.page_mut();
    let _ = page.prepare_screenshot_resources(1_000).await;
    let png = page.screenshot(viewport).ok_or("the current page has no renderable viewport")?;
    Ok(json!({
        "type": "image",
        "data": BASE64.encode(png),
        "mimeType": "image/png"
    }))
}

#[cfg(feature = "render")]
async fn tool_pdf(args: &Value, state: &mut BrowserState) -> Result<Value, String> {
    validate_tool_options(args, &[
        "landscape", "print_background", "scale", "paper_width", "paper_height",
        "margin_top", "margin_bottom", "margin_left", "margin_right",
    ])?;
    let mut options = obscura_browser::RasterPdfOptions::default();
    if let Some(value) = optional_bool(args, "landscape")? { options.landscape = value; }
    if let Some(value) = optional_bool(args, "print_background")? { options.print_background = value; }
    if let Some(value) = optional_number(args, "scale")? { options.scale = value; }
    if let Some(value) = optional_number(args, "paper_width")? { options.paper_width_in = value; }
    if let Some(value) = optional_number(args, "paper_height")? { options.paper_height_in = value; }
    if let Some(value) = optional_number(args, "margin_top")? { options.margin_top_in = value; }
    if let Some(value) = optional_number(args, "margin_bottom")? { options.margin_bottom_in = value; }
    if let Some(value) = optional_number(args, "margin_left")? { options.margin_left_in = value; }
    if let Some(value) = optional_number(args, "margin_right")? { options.margin_right_in = value; }

    let page = state.page_mut();
    let _ = page.prepare_screenshot_resources(1_000).await;
    let pdf = page.raster_pdf(options).map_err(|error| error.to_string())?;
    Ok(json!({
        "type": "resource",
        "resource": {
            "uri": "obscura://capture/current-page.pdf",
            "mimeType": "application/pdf",
            "blob": BASE64.encode(pdf)
        }
    }))
}

/// Resolve a tool call's element target from either `ref` (preferred) or
/// `selector` (fallback). Agents that called `browser_snapshot` /
/// `browser_interactive_elements` get a ref table they can refer to;
/// scripted clients can still pass raw CSS selectors.
fn resolve_target(args: &Value, state: &BrowserState) -> Result<String, String> {
    if let Some(r) = args.get("ref").and_then(Value::as_str) {
        return state.ref_to_selector(r);
    }
    if let Some(sel) = args.get("selector").and_then(Value::as_str) {
        return Ok(sel.to_string());
    }
    Err("Missing 'ref' or 'selector' parameter".to_string())
}

/// Clamp text to `max_chars` and tack on a `...(truncated, N more chars)`
/// marker so the agent can ask for more if needed. Default ceiling is
/// 4 KiB to prevent a single tool call from consuming a window of context.
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    let remaining = text.chars().count() - max_chars;
    format!("{head}\n...(truncated, {remaining} more chars)")
}

async fn tool_navigate(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let url = args.get("url").and_then(Value::as_str)
        .ok_or("Missing url parameter")?;
    let wait_until = args.get("waitUntil").and_then(Value::as_str).unwrap_or("load");

    let condition = obscura_browser::lifecycle::WaitUntil::from_str(wait_until);
    let ua = state.user_agent.clone();
    let page = state.page_mut();
    if let Some(ref ua) = ua {
        page.http_client.set_user_agent(ua).await;
    }

    page.navigate_with_wait(url, condition).await
        .map_err(|e| e.to_string())?;

    let summary = format!("Navigated to {} — \"{}\"", page.url_string(), page.title);
    // DOM changed — invalidate the ref table. Next snapshot will rebuild.
    state.interactive_refs.clear();
    Ok(summary)
}

fn tool_snapshot(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let max_chars = args.get("max_chars").and_then(Value::as_u64).map(|n| n as usize)
        .unwrap_or(DEFAULT_TEXT_LIMIT);
    rebuild_interactive_refs(state)?;
    let page = state.page_mut();
    let url = page.url_string();
    let title = page.title.clone();

    let body_text = page.with_dom(|dom| {
        if let Ok(Some(body)) = dom.query_selector("body") {
            extract_text(dom, body)
        } else {
            String::new()
        }
    }).unwrap_or_default();

    let refs_summary = if state.interactive_refs.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n{} interactive element(s) registered. Call browser_interactive_elements to list, or pass `ref` to browser_click/browser_fill/browser_type.",
            state.interactive_refs.len(),
        )
    };

    let body = truncate(body_text.trim(), max_chars);
    Ok(format!("URL: {url}\nTitle: {title}\n\n{body}{refs_summary}"))
}

fn tool_click(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = resolve_target(args, state)?;

    let js = format!(
        r#"(function(){{
            var el = document.querySelector({sel});
            if (!el) return "error:element not found";
            el.click();
            return "ok";
        }})()"#,
        sel = serde_json::to_string(&selector).unwrap()
    );

    let result = state.page_mut().evaluate(&js);
    if result.as_str() == Some("error:element not found") {
        Err(format!("Element not found: {selector}"))
    } else {
        // A click can navigate or rewrite the DOM; the old ref table may
        // no longer match. Conservative: invalidate. Next snapshot rebuilds.
        state.interactive_refs.clear();
        Ok(format!("Clicked '{selector}'"))
    }
}

fn tool_fill(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = resolve_target(args, state)?;
    let value = args.get("value").and_then(Value::as_str)
        .ok_or("Missing value parameter")?;

    let js = format!(
        r#"(function(){{
            var el = document.querySelector({sel});
            if (!el) return "error:element not found";
            globalThis.__obscura_setFieldValue(el, "value", {val});
            el.dispatchEvent(globalThis.__obscura_markTrusted(new Event("input", {{bubbles:true}})));
            el.dispatchEvent(globalThis.__obscura_markTrusted(new Event("change", {{bubbles:true}})));
            return "ok";
        }})()"#,
        sel = serde_json::to_string(&selector).unwrap(),
        val = serde_json::to_string(value).unwrap()
    );

    let result = state.page_mut().evaluate(&js);
    if result.as_str() == Some("error:element not found") {
        Err(format!("Element not found: {selector}"))
    } else {
        Ok(format!("Filled '{selector}' with value"))
    }
}

fn tool_type(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = resolve_target(args, state)?;
    let text = args.get("text").and_then(Value::as_str)
        .ok_or("Missing text parameter")?;

    let js = format!(
        r#"(function(){{
            var el = document.querySelector({sel});
            if (!el) return "error:element not found";
            globalThis.__obscura_setFieldValue(el, "value", (el.value || "") + {txt});
            el.dispatchEvent(globalThis.__obscura_markTrusted(new Event("input", {{bubbles:true}})));
            return "ok";
        }})()"#,
        sel = serde_json::to_string(&selector).unwrap(),
        txt = serde_json::to_string(text).unwrap()
    );

    let result = state.page_mut().evaluate(&js);
    if result.as_str() == Some("error:element not found") {
        Err(format!("Element not found: {selector}"))
    } else {
        Ok(format!("Typed into '{selector}'"))
    }
}

fn tool_press_key(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let key = args.get("key").and_then(Value::as_str)
        .ok_or("Missing key parameter")?;
    let selector = args.get("selector").and_then(Value::as_str);

    let target = match selector {
        Some(sel) => format!("document.querySelector({})", serde_json::to_string(sel).unwrap()),
        None => "document".to_string(),
    };

    let js = format!(
        r#"(function(){{
            var t = {target};
            if (!t) return "error:element not found";
            t.dispatchEvent(new KeyboardEvent("keydown", {{key:{key},bubbles:true}}));
            t.dispatchEvent(new KeyboardEvent("keyup", {{key:{key},bubbles:true}}));
            return "ok";
        }})()"#,
        target = target,
        key = serde_json::to_string(key).unwrap()
    );

    state.page_mut().evaluate(&js);
    Ok(format!("Pressed key '{key}'"))
}

fn tool_select_option(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = args.get("selector").and_then(Value::as_str)
        .ok_or("Missing selector parameter")?;
    let value = args.get("value").and_then(Value::as_str)
        .ok_or("Missing value parameter")?;

    let js = format!(
        r#"(function(){{
            var el = document.querySelector({sel});
            if (!el) return "error:element not found";
            var opts = Array.from(el.options);
            var opt = opts.find(function(o){{ return o.value === {val} || o.text === {val}; }});
            if (!opt) return "error:option not found";
            el.value = opt.value;
            el.dispatchEvent(new Event("change", {{bubbles:true}}));
            return "ok";
        }})()"#,
        sel = serde_json::to_string(selector).unwrap(),
        val = serde_json::to_string(value).unwrap()
    );

    let result = state.page_mut().evaluate(&js);
    match result.as_str() {
        Some("error:element not found") => Err(format!("Element not found: {selector}")),
        Some("error:option not found") => Err(format!("Option not found: {value}")),
        _ => Ok(format!("Selected '{value}' in '{selector}'")),
    }
}

fn tool_evaluate(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let expression = args.get("expression").and_then(Value::as_str)
        .ok_or("Missing expression parameter")?;

    let result = state.page_mut().evaluate(expression);
    Ok(match &result {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    })
}

async fn tool_wait_for(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = args.get("selector").and_then(Value::as_str)
        .ok_or("Missing selector parameter")?;
    let timeout_secs = args.get("timeout").and_then(Value::as_f64).unwrap_or(30.0) as u64;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    // Exponential backoff: 5 -> 10 -> 20 -> ... -> 200 ms. The old fixed
    // 200ms tick added up to a full poll cycle of latency every time;
    // a selector that appears in 30ms now returns in ~35ms instead of
    // the next 200ms tick.
    let mut tick_ms: u64 = 5;
    loop {
        let found = state.page_mut().with_dom(|dom| {
            dom.query_selector(selector).ok().flatten().is_some()
        }).unwrap_or(false);

        if found {
            return Ok(format!("Found '{selector}'"));
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Timeout waiting for '{selector}'"));
        }

        let tick = tokio::time::Duration::from_millis(tick_ms);
        match tokio::time::timeout(tick, state.advance_active_page_tasks()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => {}
        }
        if tick_ms < 200 { tick_ms = (tick_ms * 2).min(200); }
    }
}

fn tool_network_requests(state: &mut BrowserState) -> Result<String, String> {
    let page = state.page_mut();
    let events = &page.network_events;

    if events.is_empty() {
        return Ok("No network requests recorded.".to_string());
    }

    let lines: Vec<String> = events.iter().map(|e| {
        format!("[{}] {} {} ({}B)", e.status, e.method, e.url, e.body_size)
    }).collect();

    Ok(lines.join("\n"))
}

fn tool_console_messages(state: &BrowserState) -> Result<String, String> {
    if state.console_messages.is_empty() {
        Ok("No console messages.".to_string())
    } else {
        Ok(state.console_messages.join("\n"))
    }
}

fn tool_close(state: &mut BrowserState) -> Result<String, String> {
    // Drop the one live isolate (if any) via suspend_js before clearing, so the
    // map drop disposes no isolate and the LIFO rule holds regardless of the
    // BTreeMap's ascending drop order (#258).
    for page in state.tabs.values_mut() {
        page.suspend_js();
    }
    state.tabs.clear();
    state.active_tab = None;
    state.console_messages.clear();
    state.interactive_refs.clear();
    Ok("All browser tabs closed.".to_string())
}

// ===== Tier 1 agent-UX additions =====

/// Convert the rendered page to Markdown by running the JS-side converter
/// already used by `obscura fetch --dump markdown`. More token-dense than
/// browser_snapshot for content-heavy pages (article bodies, docs sites).
fn tool_markdown(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let max_chars = args.get("max_chars").and_then(Value::as_u64).map(|n| n as usize)
        .unwrap_or(DEFAULT_TEXT_LIMIT);
    let page = state.page_mut();
    let result = page.evaluate(obscura_browser::HTML_TO_MARKDOWN_JS);
    let md = result.as_str().unwrap_or_default();
    Ok(truncate(md, max_chars))
}

/// Enumerate every `<a href>` on the page. One JSON object per line so
/// the agent can grep / split without round-tripping to a JSON parser.
fn tool_links(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
    let internal_only = args.get("internal_only").and_then(Value::as_bool).unwrap_or(false);
    let page = state.page_mut();
    let base_origin = url::Url::parse(&page.url_string())
        .ok()
        .map(|u| u.origin())
        .unwrap_or_else(|| url::Url::parse("about:blank").unwrap().origin());

    let js = r#"(function(){
        var out = [];
        var seen = new Set();
        var as = document.querySelectorAll('a[href]');
        for (var i = 0; i < as.length; i++) {
            var a = as[i];
            var href = a.href || '';
            if (!href || href === '#' || href.startsWith('javascript:')) continue;
            if (seen.has(href)) continue;
            seen.add(href);
            var t = (a.innerText || a.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 200);
            out.push({text: t, href: href});
        }
        return out;
    })()"#;
    let val = page.evaluate(js);
    let arr = val.as_array().cloned().unwrap_or_default();
    let lines: Vec<String> = arr.into_iter()
        .filter(|item| {
            if !internal_only { return true; }
            item.get("href").and_then(|v| v.as_str())
                .and_then(|h| url::Url::parse(h).ok())
                .map(|u| u.origin() == base_origin)
                .unwrap_or(false)
        })
        .take(limit)
        .map(|item| item.to_string())
        .collect();
    if lines.is_empty() {
        Ok("No links found.".to_string())
    } else {
        Ok(lines.join("\n"))
    }
}

/// List every interactable element with a stable ref ID, the kind of
/// element, and a one-line description. Agents pass `ref` to click/fill/
/// type instead of crafting selectors. Also assigns `data-obscura-ref`
/// to each element so the ref survives until the next navigation.
fn tool_interactive_elements(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100) as usize;
    rebuild_interactive_refs(state)?;
    if state.interactive_refs.is_empty() {
        return Ok("No interactive elements on this page.".to_string());
    }
    let page = state.page_mut();
    let js = format!(r#"(function(){{
        var els = document.querySelectorAll('[data-obscura-ref]');
        var out = [];
        for (var i = 0; i < els.length && out.length < {limit}; i++) {{
            var e = els[i];
            var label = (e.innerText || e.textContent || e.getAttribute('aria-label') || e.getAttribute('placeholder') || e.getAttribute('value') || e.getAttribute('name') || '').trim().replace(/\s+/g, ' ').slice(0, 80);
            var role = e.getAttribute('role') || '';
            var typeAttr = e.getAttribute('type') || '';
            out.push({{
                ref: e.getAttribute('data-obscura-ref'),
                tag: e.tagName.toLowerCase(),
                type: typeAttr,
                role: role,
                name: e.getAttribute('name') || '',
                label: label,
            }});
        }}
        return out;
    }})()"#);
    let val = page.evaluate(&js);
    let arr = val.as_array().cloned().unwrap_or_default();
    let lines: Vec<String> = arr.into_iter().map(|item| {
        let r = item.get("ref").and_then(|v| v.as_str()).unwrap_or("?");
        let tag = item.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
        let ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or("");
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let kind = if !ty.is_empty() { format!("{tag}[{ty}]") } else if !role.is_empty() { format!("{tag}[role={role}]") } else { tag.to_string() };
        let detail = if !name.is_empty() { format!(" name={name:?}") } else { String::new() };
        format!("ref={r:<5} {kind:<22} {label:?}{detail}")
    }).collect();
    Ok(lines.join("\n"))
}

/// Rebuild the ref table: walk the DOM, find every interactable, assign
/// a stable `data-obscura-ref="eN"` attribute, remember the nid for later
/// validation. Called on every snapshot / interactive-elements call so the
/// agent always sees fresh refs.
fn rebuild_interactive_refs(state: &mut BrowserState) -> Result<(), String> {
    state.interactive_refs.clear();
    let page = state.page_mut();
    // Tag every interactable with data-obscura-ref="eN" in DOM order.
    let tag_js = r#"(function(){
        var sel = 'a[href], button, input:not([type=hidden]), select, textarea, [role=button], [role=link], [role=checkbox], [role=tab], [role=menuitem], [role=option], [onclick], [tabindex]:not([tabindex="-1"])';
        var els = document.querySelectorAll(sel);
        var refs = [];
        for (var i = 0; i < els.length; i++) {
            var ref = 'e' + (i + 1);
            els[i].setAttribute('data-obscura-ref', ref);
            refs.push(ref);
        }
        return refs;
    })()"#;
    let val = page.evaluate(tag_js);
    let refs: Vec<String> = val.as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    // Map ref -> nid via a second pass so ref_to_selector can sanity-check.
    for r in refs {
        let selector = format!("[data-obscura-ref=\"{r}\"]");
        let page = state.page_mut();
        let nid = page.with_dom(|dom| dom.query_selector(&selector).ok().flatten());
        if let Some(Some(node_id)) = nid {
            state.interactive_refs.insert(r, node_id);
        }
    }
    Ok(())
}

async fn tool_back(state: &mut BrowserState) -> Result<String, String> {
    let history_url = state.page_mut().with_dom(|_| ()).map(|_| ());
    let _ = history_url;
    // We track simple page history on the Page itself; navigate to the
    // entry before the cursor.
    let page = state.page_mut();
    if page.history.len() < 2 || page.history_index == 0 {
        return Err("No previous page in history.".to_string());
    }
    let prev_idx = page.history_index - 1;
    let url = page.history[prev_idx].clone();
    page.set_history_index(prev_idx);
    let condition = obscura_browser::lifecycle::WaitUntil::DomContentLoaded;
    let stash = (page.history.clone(), page.history_index);
    page.navigate_with_wait(&url, condition).await.map_err(|e| e.to_string())?;
    let page = state.page_mut();
    page.history = stash.0;
    page.history_index = stash.1;
    state.interactive_refs.clear();
    Ok(format!("Back to {url}"))
}

async fn tool_forward(state: &mut BrowserState) -> Result<String, String> {
    let page = state.page_mut();
    if page.history_index + 1 >= page.history.len() {
        return Err("No forward page in history.".to_string());
    }
    let next_idx = page.history_index + 1;
    let url = page.history[next_idx].clone();
    page.set_history_index(next_idx);
    let condition = obscura_browser::lifecycle::WaitUntil::DomContentLoaded;
    let stash = (page.history.clone(), page.history_index);
    page.navigate_with_wait(&url, condition).await.map_err(|e| e.to_string())?;
    let page = state.page_mut();
    page.history = stash.0;
    page.history_index = stash.1;
    state.interactive_refs.clear();
    Ok(format!("Forward to {url}"))
}

async fn tool_reload(state: &mut BrowserState) -> Result<String, String> {
    let url = state.page_mut().url_string();
    if url == "about:blank" {
        return Err("Nothing to reload.".to_string());
    }
    let condition = obscura_browser::lifecycle::WaitUntil::DomContentLoaded;
    state.page_mut().navigate_with_wait(&url, condition).await.map_err(|e| e.to_string())?;
    state.interactive_refs.clear();
    Ok(format!("Reloaded {url}"))
}

fn tool_get_cookies(args: &Value, state: &BrowserState) -> Result<String, String> {
    let domain_filter = args.get("domain").and_then(Value::as_str);
    let cookies = state.context.cookie_jar.get_all_cookies();
    let lines: Vec<String> = cookies.iter()
        .filter(|c| domain_filter.is_none_or(|d| c.domain == d || c.domain.trim_start_matches('.') == d))
        .map(|c| serde_json::to_string(&json!({
            "name": c.name,
            "value": c.value,
            "domain": c.domain,
            "path": c.path,
            "secure": c.secure,
            "http_only": c.http_only,
        })).unwrap_or_default())
        .collect();
    if lines.is_empty() {
        Ok("No cookies.".to_string())
    } else {
        Ok(lines.join("\n"))
    }
}

fn tool_set_cookie(args: &Value, state: &BrowserState) -> Result<String, String> {
    let name = args.get("name").and_then(Value::as_str)
        .ok_or("Missing name parameter")?;
    let value = args.get("value").and_then(Value::as_str)
        .ok_or("Missing value parameter")?;
    let domain = args.get("domain").and_then(Value::as_str)
        .ok_or("Missing domain parameter")?;
    let path = args.get("path").and_then(Value::as_str).unwrap_or("/");
    let secure = args.get("secure").and_then(Value::as_bool).unwrap_or(false);
    let http_only = args.get("http_only").and_then(Value::as_bool).unwrap_or(false);
    let cookie = obscura_net::CookieInfo {
        name: name.to_string(),
        value: value.to_string(),
        domain: domain.to_string(),
        path: path.to_string(),
        secure,
        http_only,
        same_site: String::new(),
        expires: None,
    };
    state.context.cookie_jar.set_cookies_from_cdp(vec![cookie]);
    Ok(format!("Set cookie {name} on {domain}{path}"))
}

fn tool_clear_cookies(state: &BrowserState) -> Result<String, String> {
    state.context.cookie_jar.clear();
    Ok("Cleared all cookies.".to_string())
}

async fn tool_wait_for_text(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let needle = args.get("text").and_then(Value::as_str)
        .ok_or("Missing text parameter")?;
    let timeout_secs = args.get("timeout").and_then(Value::as_f64).unwrap_or(30.0) as u64;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    let escaped = serde_json::to_string(needle).unwrap_or_else(|_| "\"\"".to_string());
    let js = format!(r#"(function(){{
        var t = (document.body && (document.body.innerText || document.body.textContent)) || '';
        return t.indexOf({needle}) >= 0;
    }})()"#, needle = escaped);
    // Exponential backoff like browser_wait_for (see comment there).
    let mut tick_ms: u64 = 5;
    loop {
        let found = state.page_mut().evaluate(&js).as_bool().unwrap_or(false);
        if found {
            return Ok(format!("Found text {needle:?}"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Timeout waiting for text {needle:?}"));
        }
        let tick = tokio::time::Duration::from_millis(tick_ms);
        match tokio::time::timeout(tick, state.advance_active_page_tasks()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => {}
        }
        if tick_ms < 200 { tick_ms = (tick_ms * 2).min(200); }
    }
}

// ===== Tier 2 agent-UX additions =====

/// Describe every <form> on the page. For each form, list its action,
/// method, and every field (input/select/textarea) with its name, type,
/// current value, and any visible label text. Agents call this to
/// understand a form's shape before filling it.
fn tool_detect_forms(state: &mut BrowserState) -> Result<String, String> {
    let page = state.page_mut();
    let js = r#"(function(){
        var forms = document.querySelectorAll('form');
        var out = [];
        for (var i = 0; i < forms.length; i++) {
            var f = forms[i];
            var fields = [];
            var inputs = f.querySelectorAll('input, select, textarea, button');
            for (var j = 0; j < inputs.length; j++) {
                var el = inputs[j];
                var tag = el.tagName.toLowerCase();
                var type = (el.getAttribute('type') || (tag === 'input' ? 'text' : tag)).toLowerCase();
                if (tag === 'input' && type === 'hidden') continue;
                var name = el.getAttribute('name') || '';
                var label = '';
                if (el.id) {
                    var lab = document.querySelector('label[for="' + el.id + '"]');
                    if (lab) label = (lab.innerText || lab.textContent || '').trim();
                }
                if (!label) label = el.getAttribute('aria-label') || el.getAttribute('placeholder') || '';
                var opts = null;
                if (tag === 'select') {
                    opts = [];
                    var os = el.querySelectorAll('option');
                    for (var k = 0; k < os.length; k++) {
                        opts.push({ value: os[k].value, text: (os[k].textContent || '').trim() });
                    }
                }
                fields.push({
                    tag: tag,
                    type: type,
                    name: name,
                    value: el.value || '',
                    checked: el.checked || false,
                    required: el.required || false,
                    label: label.trim().slice(0, 100),
                    ref: el.getAttribute('data-obscura-ref') || null,
                    options: opts,
                });
            }
            out.push({
                index: i,
                id: f.id || '',
                name: f.getAttribute('name') || '',
                action: f.action || '',
                method: (f.method || 'get').toLowerCase(),
                fields: fields,
            });
        }
        return out;
    })()"#;
    let val = page.evaluate(js);
    if val.is_null() {
        return Ok("No forms found.".to_string());
    }
    serde_json::to_string_pretty(&val).map_err(|e| e.to_string())
}

/// Fill multiple fields in one call. Each entry: {ref|selector, value, type?}.
/// type='text' (default) sets value, 'check'/'uncheck' toggles checkbox,
/// 'select' picks an option by value or visible text. Optional
/// `submit_ref`/`submit_selector` clicks after filling.
fn tool_fill_form(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let fields = args.get("fields").and_then(Value::as_array)
        .ok_or("Missing fields array")?
        .clone();
    let mut filled = 0u32;
    let mut errors = Vec::new();
    for field in fields {
        let value = field.get("value").and_then(Value::as_str).unwrap_or("");
        let kind = field.get("type").and_then(Value::as_str).unwrap_or("text");
        let selector = match resolve_target(&field, state) {
            Ok(s) => s,
            Err(e) => { errors.push(e); continue; }
        };
        let js = match kind {
            "check" => format!(r#"(function(){{
                var el = document.querySelector({sel});
                if (!el) return "error:not found";
                globalThis.__obscura_setFieldValue(el, 'checked', true);
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {{bubbles:true}})));
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('change', {{bubbles:true}})));
                return "ok";
            }})()"#, sel = serde_json::to_string(&selector).unwrap()),
            "uncheck" => format!(r#"(function(){{
                var el = document.querySelector({sel});
                if (!el) return "error:not found";
                globalThis.__obscura_setFieldValue(el, 'checked', false);
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {{bubbles:true}})));
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('change', {{bubbles:true}})));
                return "ok";
            }})()"#, sel = serde_json::to_string(&selector).unwrap()),
            "select" => format!(r#"(function(){{
                var el = document.querySelector({sel});
                if (!el) return "error:not found";
                var want = {val};
                var matched = false;
                for (var i = 0; i < el.options.length; i++) {{
                    var o = el.options[i];
                    if (o.value === want || (o.textContent || '').trim() === want) {{
                        el.selectedIndex = i;
                        matched = true;
                        break;
                    }}
                }}
                if (!matched) return "error:no matching option";
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {{bubbles:true}})));
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('change', {{bubbles:true}})));
                return "ok";
            }})()"#, sel = serde_json::to_string(&selector).unwrap(), val = serde_json::to_string(value).unwrap()),
            _ => format!(r#"(function(){{
                var el = document.querySelector({sel});
                if (!el) return "error:not found";
                globalThis.__obscura_setFieldValue(el, 'value', {val});
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('input', {{bubbles:true}})));
                el.dispatchEvent(globalThis.__obscura_markTrusted(new Event('change', {{bubbles:true}})));
                return "ok";
            }})()"#, sel = serde_json::to_string(&selector).unwrap(), val = serde_json::to_string(value).unwrap()),
        };
        let res = state.page_mut().evaluate(&js);
        match res.as_str() {
            Some("ok") => filled += 1,
            Some(e) => errors.push(format!("{selector}: {e}")),
            None => errors.push(format!("{selector}: unknown error")),
        }
    }

    // Optional submit click
    let submit_target = if args.get("submit_ref").is_some() || args.get("submit_selector").is_some() {
        let pseudo = json!({
            "ref": args.get("submit_ref"),
            "selector": args.get("submit_selector"),
        });
        resolve_target(&pseudo, state).ok()
    } else { None };
    if let Some(sel) = submit_target {
        let js = format!(r#"(function(){{
            var el = document.querySelector({sel});
            if (!el) return "error:not found";
            el.click();
            return "ok";
        }})()"#, sel = serde_json::to_string(&sel).unwrap());
        let _ = state.page_mut().evaluate(&js);
        state.interactive_refs.clear();
    }

    if errors.is_empty() {
        Ok(format!("Filled {filled} fields."))
    } else {
        Ok(format!("Filled {filled} fields. Errors: {}", errors.join("; ")))
    }
}

/// Scroll the page (or an element) by direction + amount, or scroll an
/// element into view. Used to trigger infinite-scroll loaders or to
/// reach off-viewport content. Returns the new scroll position.
fn tool_scroll(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let direction = args.get("direction").and_then(Value::as_str).unwrap_or("down");
    let amount = args.get("amount").and_then(Value::as_f64);

    // Element scroll-into-view path
    if args.get("ref").is_some() || args.get("selector").is_some() {
        let selector = resolve_target(args, state)?;
        let js = format!(r#"(function(){{
            var el = document.querySelector({sel});
            if (!el) return "error:not found";
            el.scrollIntoView({{behavior:'instant', block:'center'}});
            return JSON.stringify({{x: window.scrollX, y: window.scrollY}});
        }})()"#, sel = serde_json::to_string(&selector).unwrap());
        let res = state.page_mut().evaluate(&js);
        if res.as_str() == Some("error:not found") {
            return Err(format!("Element not found: {selector}"));
        }
        return Ok(format!("Scrolled element into view. {}", res.as_str().unwrap_or("")));
    }

    // Page-level scroll. Also dispatch a 'scroll' event so infinite-
    // scroll handlers fire (we don't have a real layout engine, so the
    // window.scrollY value won't change but the event is what matters).
    let amt = amount.unwrap_or(720.0);
    let js = format!(r#"(function(){{
        var dir = {dir};
        var amt = {amt};
        switch (dir) {{
            case 'top': window.scrollTo(0, 0); break;
            case 'bottom': window.scrollTo(0, document.body.scrollHeight); break;
            case 'up': window.scrollBy(0, -amt); break;
            case 'down': window.scrollBy(0, amt); break;
            case 'left': window.scrollBy(-amt, 0); break;
            case 'right': window.scrollBy(amt, 0); break;
        }}
        try {{ window.dispatchEvent(new Event('scroll', {{bubbles:true}})); }} catch(e) {{}}
        try {{ document.dispatchEvent(new Event('scroll', {{bubbles:true}})); }} catch(e) {{}}
        return JSON.stringify({{x: window.scrollX, y: window.scrollY, max_y: document.body.scrollHeight, viewport_h: window.innerHeight}});
    }})()"#,
        dir = serde_json::to_string(direction).unwrap(),
        amt = amt,
    );
    let res = state.page_mut().evaluate(&js);
    // A scroll can reveal new DOM (infinite scroll); invalidate refs.
    state.interactive_refs.clear();
    Ok(format!("Scrolled {direction}. {}", res.as_str().unwrap_or("")))
}

fn tool_get_attribute(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = resolve_target(args, state)?;
    let attr = args.get("attribute").and_then(Value::as_str)
        .ok_or("Missing attribute parameter")?;
    let js = format!(r#"(function(){{
        var el = document.querySelector({sel});
        if (!el) return null;
        var v = el.getAttribute({a});
        if (v === null && {a} === 'value') v = el.value || '';
        return v == null ? '' : v;
    }})()"#,
        sel = serde_json::to_string(&selector).unwrap(),
        a = serde_json::to_string(attr).unwrap(),
    );
    let res = state.page_mut().evaluate(&js);
    if res.is_null() {
        return Err(format!("Element not found: {selector}"));
    }
    Ok(res.as_str().unwrap_or("").to_string())
}

fn tool_count(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = args.get("selector").and_then(Value::as_str)
        .ok_or("Missing selector parameter")?;
    let js = format!(
        "document.querySelectorAll({sel}).length",
        sel = serde_json::to_string(selector).unwrap()
    );
    let res = state.page_mut().evaluate(&js);
    // V8 numbers come back as f64 even when they are integer-valued; as_u64
    // returns None for f64 in serde_json, so coerce via f64.
    let n = res.as_u64()
        .or_else(|| res.as_f64().map(|f| f as u64))
        .unwrap_or(0);
    Ok(n.to_string())
}

/// Extract structured data: `schema` is a map of field name to CSS
/// selector. Suffix selector with `@attr` to read an attribute instead
/// of text. Suffix field name with `[]` to return an array (queries all
/// matching elements rather than the first).
fn tool_extract(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let schema = args.get("schema").and_then(Value::as_object)
        .ok_or("Missing schema object")?
        .clone();
    let schema_json = serde_json::to_string(&schema).unwrap();
    let js = format!(r#"(function(){{
        var schema = {schema};
        var out = {{}};
        for (var key in schema) {{
            if (!Object.prototype.hasOwnProperty.call(schema, key)) continue;
            var spec = schema[key];
            var is_array = key.endsWith('[]');
            var name = is_array ? key.slice(0, -2) : key;
            // Selector may end with `@attr` to read an attribute.
            var attr = null;
            var sel = spec;
            var at = spec.lastIndexOf('@');
            if (at > 0 && spec.indexOf(' ', at) < 0) {{
                attr = spec.slice(at + 1);
                sel = spec.slice(0, at);
            }}
            var get = function(el) {{
                if (!el) return null;
                if (attr) return el.getAttribute(attr) || '';
                return ((el.innerText || el.textContent) || '').trim();
            }};
            if (is_array) {{
                var els = document.querySelectorAll(sel);
                var arr = [];
                for (var i = 0; i < els.length; i++) arr.push(get(els[i]));
                out[name] = arr;
            }} else {{
                out[name] = get(document.querySelector(sel));
            }}
        }}
        return out;
    }})()"#, schema = schema_json);
    let res = state.page_mut().evaluate(&js);
    serde_json::to_string_pretty(&res).map_err(|e| e.to_string())
}

async fn tool_tab_new(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let url = args.get("url").and_then(Value::as_str);
    let id = state.new_tab();
    if let Some(u) = url {
        let ua = state.user_agent.clone();
        let page = state.page_mut();
        if let Some(ref ua) = ua {
            page.http_client.set_user_agent(ua).await;
        }
        page.navigate_with_wait(u, obscura_browser::lifecycle::WaitUntil::DomContentLoaded)
            .await.map_err(|e| e.to_string())?;
        Ok(format!("Opened {id} and navigated to {}", page.url_string()))
    } else {
        Ok(format!("Opened {id} (about:blank)."))
    }
}

fn tool_tab_list(state: &BrowserState) -> Result<String, String> {
    if state.tabs.is_empty() {
        return Ok("No tabs open.".to_string());
    }
    let lines: Vec<String> = state.tabs.iter().map(|(id, page)| {
        let active = if Some(id) == state.active_tab.as_ref() { "*" } else { " " };
        let url = page.url_string();
        let title = page.title.replace('\n', " ");
        format!("{active} {id}  {url}  \"{title}\"")
    }).collect();
    Ok(lines.join("\n"))
}

fn tool_tab_switch(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let tab_id = args.get("tab_id").and_then(Value::as_str)
        .ok_or("Missing tab_id parameter")?;
    if !state.tabs.contains_key(tab_id) {
        return Err(format!("No such tab: {tab_id}"));
    }
    state.active_tab = Some(tab_id.to_string());
    state.interactive_refs.clear();
    Ok(format!("Active tab: {tab_id}"))
}

fn tool_tab_close(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let tab_id = args.get("tab_id").and_then(Value::as_str)
        .map(String::from)
        .or_else(|| state.active_tab.clone())
        .ok_or("No tab to close")?;
    if !state.close_tab(&tab_id) {
        return Err(format!("No such tab: {tab_id}"));
    }
    if state.active_tab.as_deref() == Some(&tab_id) {
        // Promote some remaining tab to active, if any.
        state.active_tab = state.tabs.keys().next().cloned();
        state.interactive_refs.clear();
    }
    let summary = if let Some(ref a) = state.active_tab {
        format!("Closed {tab_id}. Active tab now {a}.")
    } else {
        format!("Closed {tab_id}. No tabs remain.")
    };
    Ok(summary)
}

fn extract_text(dom: &obscura_dom::DomTree, node_id: obscura_dom::NodeId) -> String {
    use obscura_dom::NodeData;

    let mut result = String::new();
    let node = match dom.get_node(node_id) {
        Some(n) => n,
        None => return result,
    };

    match &node.data {
        NodeData::Text { contents } => {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                result.push_str(trimmed);
                result.push(' ');
            }
        }
        NodeData::Element { name, .. } => {
            let tag = name.local.as_ref();
            if matches!(tag, "script" | "style" | "noscript") {
                return result;
            }

            let is_block = matches!(
                tag,
                "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    | "li" | "tr" | "br" | "hr" | "section" | "article"
                    | "header" | "footer" | "nav" | "main" | "aside"
                    | "blockquote" | "pre" | "ul" | "ol" | "table"
            );

            if is_block {
                result.push('\n');
            }

            for child in dom.children(node_id) {
                result.push_str(&extract_text(dom, child));
            }

            if is_block {
                result.push('\n');
            }
        }
        _ => {
            for child in dom.children(node_id) {
                result.push_str(&extract_text(dom, child));
            }
        }
    }

    result
}

// ===== Tier 3 agent-UX additions =====

/// Substring search in visible page text. Returns each match with N chars
/// of surrounding context so the agent can locate the section without
/// pulling the whole page into its window.
fn tool_search(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let query = args.get("query").and_then(Value::as_str)
        .ok_or("Missing query parameter")?;
    let case_sensitive = args.get("case_sensitive").and_then(Value::as_bool).unwrap_or(false);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let context = args.get("context_chars").and_then(Value::as_u64).unwrap_or(80) as usize;

    let page = state.page_mut();
    let body = page.with_dom(|dom| {
        dom.query_selector("body").ok().flatten()
            .map(|b| extract_text(dom, b))
            .unwrap_or_default()
    }).unwrap_or_default();

    let haystack = if case_sensitive { body.clone() } else { body.to_lowercase() };
    let needle = if case_sensitive { query.to_string() } else { query.to_lowercase() };

    let mut out = Vec::new();
    let mut idx = 0;
    while let Some(pos) = haystack[idx..].find(&needle) {
        let abs = idx + pos;
        let mut start = abs.saturating_sub(context);
        let mut end = (abs + needle.len() + context).min(body.len());
        // start/end are byte offsets derived from char counts and needle.len(),
        // so they can land inside a multi-byte (CJK) character. Snap to char
        // boundaries before slicing or body[..start] panics (#257).
        while start > 0 && !body.is_char_boundary(start) { start -= 1; }
        while end < body.len() && !body.is_char_boundary(end) { end += 1; }
        // Trim inward to the nearest whitespace so snippets start/end on words.
        if let Some(i) = body[..start].rfind(|c: char| c.is_whitespace()) {
            start = i + body[i..].chars().next().map_or(1, char::len_utf8);
        }
        if let Some(i) = body[end..].find(|c: char| c.is_whitespace()) {
            end += i;
        }
        let snippet = body.get(start..end).unwrap_or("").trim().replace('\n', " ");
        out.push(json!({
            "offset": abs,
            "snippet": snippet,
        }));
        idx = abs + needle.len();
        if out.len() >= limit { break; }
    }
    if out.is_empty() {
        Ok(format!("No matches for {query:?}."))
    } else {
        Ok(format!("{} match(es). {}", out.len(),
            out.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n")))
    }
}

/// Export full session state: cookies + localStorage + sessionStorage
/// for every origin the page knows about. Agents stash this between
/// runs to skip a login flow.
fn tool_storage_state(state: &mut BrowserState) -> Result<String, String> {
    let cookies: Vec<Value> = state.context.cookie_jar.get_all_cookies().iter().map(|c| json!({
        "name": c.name,
        "value": c.value,
        "domain": c.domain,
        "path": c.path,
        "secure": c.secure,
        "http_only": c.http_only,
        "same_site": c.same_site,
        "expires": c.expires,
    })).collect();
    // Pull localStorage + sessionStorage for the current page's origin.
    let storage_js = r#"(function(){
        var ls = [], ss = [];
        try { for (var i = 0; i < localStorage.length; i++) { var k = localStorage.key(i); ls.push([k, localStorage.getItem(k)]); } } catch(e) {}
        try { for (var j = 0; j < sessionStorage.length; j++) { var k2 = sessionStorage.key(j); ss.push([k2, sessionStorage.getItem(k2)]); } } catch(e) {}
        return { origin: location.origin || '', localStorage: ls, sessionStorage: ss };
    })()"#;
    let storage = if state.active_tab.is_some() {
        state.page_mut().evaluate(storage_js)
    } else {
        Value::Null
    };
    let origins = if storage.is_object() { vec![storage] } else { vec![] };
    let out = json!({ "cookies": cookies, "origins": origins });
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

fn tool_set_storage_state(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let s = args.get("state").ok_or("Missing state object")?;
    let mut applied = 0u32;
    // Cookies
    if let Some(cookies) = s.get("cookies").and_then(Value::as_array) {
        let parsed: Vec<obscura_net::CookieInfo> = cookies.iter().filter_map(|c| {
            Some(obscura_net::CookieInfo {
                name: c.get("name")?.as_str()?.to_string(),
                value: c.get("value")?.as_str()?.to_string(),
                domain: c.get("domain")?.as_str()?.to_string(),
                path: c.get("path").and_then(Value::as_str).unwrap_or("/").to_string(),
                secure: c.get("secure").and_then(Value::as_bool).unwrap_or(false),
                http_only: c.get("http_only").and_then(Value::as_bool).unwrap_or(false),
                same_site: c.get("same_site").and_then(Value::as_str).unwrap_or("").to_string(),
                expires: c.get("expires").and_then(Value::as_i64),
            })
        }).collect();
        applied += parsed.len() as u32;
        state.context.cookie_jar.set_cookies_from_cdp(parsed);
    }
    // Storage (per origin). Only applies if there's an active page; we
    // restore on whatever origin is currently loaded, which usually
    // matches because agents navigate before restoring state.
    if state.active_tab.is_some() {
        if let Some(origins) = s.get("origins").and_then(Value::as_array) {
            for origin_entry in origins {
                let mut snippets = Vec::new();
                if let Some(arr) = origin_entry.get("localStorage").and_then(Value::as_array) {
                    for pair in arr {
                        if let (Some(k), Some(v)) = (
                            pair.get(0).and_then(Value::as_str),
                            pair.get(1).and_then(Value::as_str),
                        ) {
                            snippets.push(format!(
                                "try {{ localStorage.setItem({k},{v}); }} catch(e) {{}};",
                                k = serde_json::to_string(k).unwrap(),
                                v = serde_json::to_string(v).unwrap(),
                            ));
                            applied += 1;
                        }
                    }
                }
                if let Some(arr) = origin_entry.get("sessionStorage").and_then(Value::as_array) {
                    for pair in arr {
                        if let (Some(k), Some(v)) = (
                            pair.get(0).and_then(Value::as_str),
                            pair.get(1).and_then(Value::as_str),
                        ) {
                            snippets.push(format!(
                                "try {{ sessionStorage.setItem({k},{v}); }} catch(e) {{}};",
                                k = serde_json::to_string(k).unwrap(),
                                v = serde_json::to_string(v).unwrap(),
                            ));
                            applied += 1;
                        }
                    }
                }
                if !snippets.is_empty() {
                    let _ = state.page_mut().evaluate(&snippets.join("\n"));
                }
            }
        }
    }
    Ok(format!("Restored {applied} state entries."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed_tools() -> Vec<Value> {
        handle_tools_list(json!(1)).result.expect("tools/list result")
            .get("tools").and_then(Value::as_array).cloned().expect("tools array")
    }

    #[test]
    fn tool_schemas_expose_snapshot_limit_without_nested_properties() {
        let tools = listed_tools();
        let snapshot = tools.iter().find(|tool| tool["name"] == "browser_snapshot")
            .expect("browser_snapshot tool");
        assert_eq!(snapshot["inputSchema"]["properties"]["max_chars"]["type"], "number");
        for tool in tools {
            assert!(
                tool["inputSchema"]["properties"].get("properties").is_none(),
                "{} has a nested duplicate properties object", tool["name"]
            );
        }
    }

    #[cfg(not(feature = "render"))]
    #[test]
    fn render_tools_are_not_advertised_without_render_feature() {
        let tools = listed_tools();
        assert!(tools.iter().all(|tool| {
            tool["name"] != "browser_screenshot" && tool["name"] != "browser_pdf"
        }));
    }

    #[cfg(feature = "render")]
    #[test]
    fn render_tools_are_advertised_with_flat_schemas() {
        let tools = listed_tools();
        for name in ["browser_screenshot", "browser_pdf"] {
            let tool = tools.iter().find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(tool["inputSchema"]["properties"].is_object());
            assert!(tool["inputSchema"]["properties"].get("properties").is_none());
        }
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn render_tool_calls_return_mcp_binary_content_and_reject_bad_options() {
        let mut state = BrowserState::new(None, None, false);
        state.page_mut().navigate(
            "data:text/html,<html style='margin:0'><body style='margin:0;background:red'><div style='width:64px;height:48px'></div></body></html>",
        ).await.expect("render test page should navigate");
        state.page_mut().set_viewport((64.0, 48.0));

        let screenshot = handle_tool_call(
            json!(1), &json!({ "name": "browser_screenshot", "arguments": {} }), &mut state,
        ).await.result.expect("screenshot response");
        let image = &screenshot["content"][0];
        assert_eq!(image["type"], "image");
        assert_eq!(image["mimeType"], "image/png");
        let png = BASE64.decode(image["data"].as_str().expect("PNG base64"))
            .expect("valid PNG base64");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

        let pdf = handle_tool_call(
            json!(2),
            &json!({ "name": "browser_pdf", "arguments": { "print_background": true } }),
            &mut state,
        ).await.result.expect("PDF response");
        let resource = &pdf["content"][0];
        assert_eq!(resource["type"], "resource");
        assert_eq!(resource["resource"]["mimeType"], "application/pdf");
        let bytes = BASE64.decode(resource["resource"]["blob"].as_str().expect("PDF base64"))
            .expect("valid PDF base64");
        assert!(bytes.starts_with(b"%PDF-"));

        let invalid_screenshot = handle_tool_call(
            json!(3),
            &json!({ "name": "browser_screenshot", "arguments": { "width": 0 } }),
            &mut state,
        ).await.result.expect("invalid screenshot response");
        assert_eq!(invalid_screenshot["isError"], true);

        let invalid_pdf = handle_tool_call(
            json!(4),
            &json!({ "name": "browser_pdf", "arguments": { "scale": 3 } }),
            &mut state,
        ).await.result.expect("invalid PDF response");
        assert_eq!(invalid_pdf["isError"], true);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fill_tools_notify_controlled_input_tracker() {
        let mut state = BrowserState::new(None, None, false);
        state
            .page_mut()
            .navigate("data:text/html,<div id=root><input id=field></div>")
            .await
            .expect("test page should navigate");

        state.page_mut().evaluate(
            r#"(function () {
                var root = document.getElementById('root');
                var input = document.getElementById('field');
                var descriptor = Object.getOwnPropertyDescriptor(input.constructor.prototype, 'value');
                var tracked = String(input.value);
                Object.defineProperty(input, 'value', {
                    configurable: true,
                    get: function () { return descriptor.get.call(this); },
                    set: function (value) {
                        tracked = String(value);
                        descriptor.set.call(this, value);
                    }
                });
                window.__controlledState = '';
                window.__controlledUpdates = 0;
                window.__lastInputTarget = '';
                window.__lastInputTrusted = false;
                root.addEventListener('input', function (event) {
                    window.__lastInputTarget = event.target.id;
                    window.__lastInputTrusted = event.isTrusted;
                    var next = String(event.target.value);
                    if (next !== tracked) {
                        tracked = next;
                        window.__controlledState = next;
                        window.__controlledUpdates++;
                    }
                });
            })()"#,
        );

        tool_fill(
            &json!({ "selector": "#field", "value": "filled" }),
            &mut state,
        )
        .expect("browser_fill should succeed");
        tool_type(
            &json!({ "selector": "#field", "text": "-typed" }),
            &mut state,
        )
        .expect("browser_type should succeed");
        tool_fill_form(
            &json!({
                "fields": [{ "selector": "#field", "value": "form-filled" }]
            }),
            &mut state,
        )
        .expect("browser_fill_form should succeed");

        let actual = state.page_mut().evaluate(
            r#"JSON.stringify({
                domValue: document.getElementById('field').value,
                controlledState: window.__controlledState,
                controlledUpdates: window.__controlledUpdates,
                lastInputTarget: window.__lastInputTarget,
                lastInputTrusted: window.__lastInputTrusted
            })"#,
        );
        assert_eq!(
            actual,
            json!(r#"{"domValue":"form-filled","controlledState":"form-filled","controlledUpdates":3,"lastInputTarget":"field","lastInputTrusted":true}"#),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fill_form_check_and_select_use_native_setter_and_trusted_events() {
        let mut state = BrowserState::new(None, None, false);
        state
            .page_mut()
            .navigate(
                "data:text/html,<div id=root><input id=box type=checkbox>\
                 <select id=sel><option value=a>a</option><option value=b>b</option></select></div>",
            )
            .await
            .expect("test page should navigate");

        // Install a React-style tracker on the checkbox's `checked`, redefined on
        // the instance. A direct `el.checked = true` runs this wrapper in lockstep,
        // so a change handler comparing target.checked to the tracked value sees no
        // change and never commits. Writing through the prototype setter (what
        // __obscura_setFieldValue does) leaves the tracker stale so the edit
        // registers. Also record whether the dispatched change is trusted.
        state.page_mut().evaluate(
            r#"(function () {
                var box = document.getElementById('box');
                var root = document.getElementById('root');
                var d = Object.getOwnPropertyDescriptor(box.constructor.prototype, 'checked');
                var tracked = box.checked;
                Object.defineProperty(box, 'checked', {
                    configurable: true,
                    get: function () { return d.get.call(this); },
                    set: function (v) { tracked = !!v; d.set.call(this, v); }
                });
                window.__checkedCommitted = false;
                window.__checkTrusted = false;
                window.__selectTrusted = false;
                root.addEventListener('change', function (event) {
                    if (event.target.id === 'box') {
                        window.__checkTrusted = event.isTrusted;
                        if (event.target.checked !== tracked) {
                            tracked = event.target.checked;
                            window.__checkedCommitted = true;
                        }
                    } else if (event.target.id === 'sel') {
                        window.__selectTrusted = event.isTrusted;
                    }
                });
            })()"#,
        );

        tool_fill_form(
            &json!({
                "fields": [
                    { "selector": "#box", "type": "check" },
                    { "selector": "#sel", "type": "select", "value": "b" }
                ]
            }),
            &mut state,
        )
        .expect("browser_fill_form should succeed");

        let actual = state.page_mut().evaluate(
            r#"JSON.stringify({
                domChecked: document.getElementById('box').checked,
                checkedCommitted: window.__checkedCommitted,
                checkTrusted: window.__checkTrusted,
                selValue: document.getElementById('sel').value,
                selectTrusted: window.__selectTrusted
            })"#,
        );
        assert_eq!(
            actual,
            json!(r#"{"domChecked":true,"checkedCommitted":true,"checkTrusted":true,"selValue":"b","selectTrusted":true}"#),
        );
    }
}
