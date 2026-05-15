use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

const OBSCURA: &str = env!("CARGO_BIN_EXE_obscura");

// ── minimal MCP client ────────────────────────────────────────────────────────

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn spawn() -> Self {
        let mut child = Command::new(OBSCURA)
            .args(["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn obscura mcp");

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let mut client = McpClient {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        };

        // Perform the initialize handshake automatically
        let id = client.next_id;
        client.next_id += 1;
        let _resp = client.call_raw(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "0.0.0" }
            }
        }));

        // Send initialized notification (no id, no response expected)
        client.notify("notifications/initialized", serde_json::json!({}));

        client
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        let msg = serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_msg(&msg);
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.call_raw(msg)
    }

    fn call_raw(&mut self, msg: serde_json::Value) -> serde_json::Value {
        self.write_msg(&msg);
        self.read_msg()
    }

    fn tool(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        self.call("tools/call", serde_json::json!({ "name": name, "arguments": args }))
    }

    fn write_msg(&mut self, msg: &serde_json::Value) {
        // MCP stdio transport: newline-delimited JSON
        let mut body = serde_json::to_string(msg).unwrap();
        body.push('\n');
        self.stdin.write_all(body.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read_msg(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read line");
        serde_json::from_str(line.trim()).expect("parse JSON")
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// helper: extract first content text from a tools/call response
fn content_text(resp: &serde_json::Value) -> &str {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_initialize() {
    let mut c = McpClient::spawn();
    // A second initialize should still return a valid response
    let resp = c.call(
        "initialize",
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {}
        }),
    );
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(resp["result"]["serverInfo"]["name"], "obscura-mcp");
}

#[test]
fn test_ping() {
    let mut c = McpClient::spawn();
    let resp = c.call("ping", serde_json::json!({}));
    assert!(resp.get("error").is_none(), "ping should not error");
    assert_eq!(resp["result"], serde_json::json!({}));
}

#[test]
fn test_tools_list() {
    let mut c = McpClient::spawn();
    let resp = c.call("tools/list", serde_json::json!({}));
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty());
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in &[
        "browser_navigate",
        "browser_snapshot",
        "browser_click",
        "browser_fill",
        "browser_type",
        "browser_press_key",
        "browser_select_option",
        "browser_evaluate",
        "browser_wait_for",
        "browser_network_requests",
        "browser_console_messages",
        "browser_close",
    ] {
        assert!(names.contains(expected), "missing tool: {expected}");
    }
}

#[test]
fn test_resources_list() {
    let mut c = McpClient::spawn();
    let resp = c.call("resources/list", serde_json::json!({}));
    assert!(resp.get("error").is_none());
    assert_eq!(resp["result"]["resources"], serde_json::json!([]));
}

#[test]
fn test_prompts_list() {
    let mut c = McpClient::spawn();
    let resp = c.call("prompts/list", serde_json::json!({}));
    assert!(resp.get("error").is_none());
    assert_eq!(resp["result"]["prompts"], serde_json::json!([]));
}

#[test]
fn test_unknown_method_returns_error() {
    let mut c = McpClient::spawn();
    let resp = c.call("nonexistent/method", serde_json::json!({}));
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn test_notifications_are_silent() {
    // Notifications must not produce a response; the next real call should succeed
    let mut c = McpClient::spawn();
    c.notify("notifications/initialized", serde_json::json!({}));
    c.notify("some/other_notification", serde_json::json!({"x": 1}));
    // If the server accidentally replied to a notification, the next read would
    // consume that stray response and the ping result would be mismatched.
    let resp = c.call("ping", serde_json::json!({}));
    assert!(resp.get("error").is_none());
}

#[test]
fn test_navigate_and_snapshot() {
    let mut c = McpClient::spawn();

    let nav = c.tool("browser_navigate", serde_json::json!({"url": "https://example.com"}));
    assert!(nav["result"]["isError"].is_null(), "navigate failed: {nav}");
    let text = content_text(&nav);
    assert!(text.contains("example.com"), "unexpected nav text: {text}");

    let snap = c.tool("browser_snapshot", serde_json::json!({}));
    assert!(snap["result"]["isError"].is_null(), "snapshot failed: {snap}");
    let text = content_text(&snap);
    assert!(text.contains("Example Domain"), "unexpected snapshot: {text}");
    assert!(text.contains("URL:"), "snapshot missing URL line");
}

#[test]
fn test_evaluate() {
    let mut c = McpClient::spawn();
    c.tool("browser_navigate", serde_json::json!({"url": "https://example.com"}));

    let resp = c.tool(
        "browser_evaluate",
        serde_json::json!({"expression": "document.title"}),
    );
    let text = content_text(&resp);
    assert_eq!(text, "Example Domain");
}

#[test]
fn test_evaluate_math() {
    let mut c = McpClient::spawn();
    c.tool("browser_navigate", serde_json::json!({"url": "https://example.com"}));

    let resp = c.tool(
        "browser_evaluate",
        serde_json::json!({"expression": "1 + 2"}),
    );
    let text = content_text(&resp);
    // V8 serialises integer results as floats ("3" or "3.0" depending on context)
    assert!(text == "3" || text == "3.0", "unexpected result: {text}");
}

#[test]
fn test_wait_for_selector() {
    let mut c = McpClient::spawn();
    c.tool("browser_navigate", serde_json::json!({"url": "https://example.com"}));

    let resp = c.tool(
        "browser_wait_for",
        serde_json::json!({"selector": "h1", "timeout": 5}),
    );
    assert!(resp["result"]["isError"].is_null(), "wait_for failed: {resp}");
    assert!(content_text(&resp).contains("Found"));
}

#[test]
fn test_wait_for_timeout() {
    let mut c = McpClient::spawn();
    c.tool("browser_navigate", serde_json::json!({"url": "https://example.com"}));

    let resp = c.tool(
        "browser_wait_for",
        serde_json::json!({"selector": "#does-not-exist", "timeout": 1}),
    );
    assert_eq!(resp["result"]["isError"], true);
    assert!(content_text(&resp).contains("Timeout"));
}

#[test]
fn test_navigate_missing_url_returns_error() {
    let mut c = McpClient::spawn();
    let resp = c.tool("browser_navigate", serde_json::json!({}));
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn test_unknown_tool_returns_error() {
    let mut c = McpClient::spawn();
    let resp = c.tool("browser_does_not_exist", serde_json::json!({}));
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn test_network_requests() {
    let mut c = McpClient::spawn();
    c.tool("browser_navigate", serde_json::json!({"url": "https://example.com"}));

    let resp = c.tool("browser_network_requests", serde_json::json!({}));
    let text = content_text(&resp);
    assert!(
        text.contains("example.com") || text.contains("No network"),
        "unexpected: {text}"
    );
}

#[test]
fn test_close_resets_state() {
    let mut c = McpClient::spawn();
    c.tool("browser_navigate", serde_json::json!({"url": "https://example.com"}));
    let close = c.tool("browser_close", serde_json::json!({}));
    assert!(close["result"]["isError"].is_null());

    // After close, snapshot should return empty/default page (no panic)
    let snap = c.tool("browser_snapshot", serde_json::json!({}));
    assert!(snap["result"]["isError"].is_null());
}
