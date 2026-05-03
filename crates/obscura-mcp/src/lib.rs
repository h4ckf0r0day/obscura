pub mod http;

use std::sync::Arc;

use anyhow::Result;
use obscura_browser::{BrowserContext, Page};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
}

impl BrowserState {
    pub fn new(proxy: Option<String>, user_agent: Option<String>, stealth: bool) -> Self {
        BrowserState {
            page: None,
            context: Arc::new(BrowserContext::with_options("mcp".to_string(), proxy, stealth)),
            user_agent,
            console_messages: Vec::new(),
        }
    }

    fn page_mut(&mut self) -> &mut Page {
        if self.page.is_none() {
            self.page = Some(Page::new("mcp-page".to_string(), self.context.clone()));
        }
        self.page.as_mut().unwrap()
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
                "description": "Click an element matching the CSS selector",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "CSS selector of the element to click" }
                    },
                    "required": ["selector"]
                }
            },
            {
                "name": "browser_fill",
                "description": "Set the value of an input element",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "CSS selector of the input element" },
                        "value": { "type": "string", "description": "Value to set" }
                    },
                    "required": ["selector", "value"]
                }
            },
            {
                "name": "browser_type",
                "description": "Type text into an input element (appends to existing value)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string", "description": "CSS selector of the element" },
                        "text": { "type": "string", "description": "Text to type" }
                    },
                    "required": ["selector", "text"]
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
        "browser_snapshot" => tool_snapshot(state),
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

    Ok(format!("Navigated to {} — \"{}\"", page.url_string(), page.title))
}

fn tool_snapshot(state: &mut BrowserState) -> Result<String, String> {
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

    Ok(format!("URL: {url}\nTitle: {title}\n\n{}", body_text.trim()))
}

fn tool_click(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = args.get("selector").and_then(Value::as_str)
        .ok_or("Missing selector parameter")?;

    let js = format!(
        r#"(function(){{
            var el = document.querySelector({sel});
            if (!el) return "error:element not found";
            el.click();
            return "ok";
        }})()"#,
        sel = serde_json::to_string(selector).unwrap()
    );

    let result = state.page_mut().evaluate(&js);
    if result.as_str() == Some("error:element not found") {
        Err(format!("Element not found: {selector}"))
    } else {
        Ok(format!("Clicked '{selector}'"))
    }
}

fn tool_fill(args: &Value, state: &mut BrowserState) -> Result<String, String> {
    let selector = args.get("selector").and_then(Value::as_str)
        .ok_or("Missing selector parameter")?;
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
        sel = serde_json::to_string(selector).unwrap(),
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
    let selector = args.get("selector").and_then(Value::as_str)
        .ok_or("Missing selector parameter")?;
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
        sel = serde_json::to_string(selector).unwrap(),
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
    Ok("Browser page closed.".to_string())
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
