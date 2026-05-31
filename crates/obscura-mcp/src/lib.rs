pub mod http;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
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
    page: Option<Page>,
    context: Arc<BrowserContext>,
    user_agent: Option<String>,
    console_messages: Vec<String>,
    /// Element-ref table from the last `browser_snapshot`. Agents click /
    /// fill / type by `ref` (e.g. `"e3"`) instead of guessing a CSS
    /// selector — this is the single biggest agent UX win, ported from
    /// playwright-mcp. Refs are stable within a single snapshot; the
    /// table is wiped on every navigation and refilled on the next
    /// snapshot call.
    interactive_refs: HashMap<String, NodeId>,
}

impl BrowserState {
    pub fn new(proxy: Option<String>, user_agent: Option<String>, stealth: bool) -> Self {
        BrowserState {
            page: None,
            context: Arc::new(BrowserContext::with_options("mcp".to_string(), proxy, stealth)),
            user_agent,
            console_messages: Vec::new(),
            interactive_refs: HashMap::new(),
        }
    }

    fn page_mut(&mut self) -> &mut Page {
        if self.page.is_none() {
            self.page = Some(Page::new("mcp-page".to_string(), self.context.clone()));
        }
        self.page.as_mut().unwrap()
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

pub async fn dispatch(method: &str, id: Value, params: &Value, state: &mut BrowserState) -> RpcResponse {
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

    loop {
        // MCP stdio transport: newline-delimited JSON (one message per line)
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
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
    RpcResponse::ok(id, json!({
        "tools": [
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
                    "properties": {}
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
            }
        ]
    }))
}

async fn handle_tool_call(id: Value, params: &Value, state: &mut BrowserState) -> RpcResponse {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => return RpcResponse::err(id, -32602, "Missing tool name"),
    };
    let args = params.get("arguments").unwrap_or(&Value::Null);

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
            el.value = {val};
            el.dispatchEvent(new Event("input", {{bubbles:true}}));
            el.dispatchEvent(new Event("change", {{bubbles:true}}));
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
            el.value = (el.value || "") + {txt};
            el.dispatchEvent(new Event("input", {{bubbles:true}}));
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

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
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
    state.page = None;
    state.console_messages.clear();
    state.interactive_refs.clear();
    Ok("Browser page closed.".to_string())
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
    loop {
        let found = state.page_mut().evaluate(&js).as_bool().unwrap_or(false);
        if found {
            return Ok(format!("Found text {needle:?}"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Timeout waiting for text {needle:?}"));
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
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
