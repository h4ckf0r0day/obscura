use obscura_mcp::mcp;
use serde_json::{json, Value};

// ── parse_request ──────────────────────────────────────────────────────────

#[test]
fn parse_valid_json() {
    let req = mcp::parse_request(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
    assert!(req.is_some());
    assert_eq!(req.unwrap()["method"], "initialize");
}

#[test]
fn parse_strips_whitespace() {
    let req = mcp::parse_request(r#"  {"jsonrpc":"2.0","id":1,"method":"initialize"}  "#);
    assert!(req.is_some());
}

#[test]
fn parse_empty_returns_none() {
    assert!(mcp::parse_request("").is_none());
    assert!(mcp::parse_request("   ").is_none());
    assert!(mcp::parse_request("\n").is_none());
}

#[test]
fn parse_invalid_json_returns_none() {
    assert!(mcp::parse_request("not json").is_none());
    assert!(mcp::parse_request("{broken").is_none());
}

// ── make_response / make_error ─────────────────────────────────────────────

#[test]
fn response_format() {
    let resp = mcp::make_response(&json!(42), json!({"ok": true}));
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["id"], 42);
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["result"]["ok"], true);
    assert!(resp.ends_with('\n'));
}

#[test]
fn error_format() {
    let resp = mcp::make_error(&json!(1), -32601, "not found");
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["id"], 1);
    assert_eq!(parsed["error"]["code"], -32601);
    assert_eq!(parsed["error"]["message"], "not found");
}

// ── handle_request ─────────────────────────────────────────────────────────

#[test]
fn initialize() {
    let resp = mcp::handle_request(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {}
    }))
    .unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(parsed["result"]["serverInfo"]["name"], "obscura-mcp");
    assert_eq!(parsed["result"]["capabilities"]["tools"], json!({}));
}

#[test]
fn notification_initialized_no_response() {
    let resp = mcp::handle_request(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    assert!(resp.is_none());
}

#[test]
fn tools_list() {
    let resp = mcp::handle_request(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    }))
    .unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    let tools = parsed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 5);

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"obscura_fetch"));
    assert!(names.contains(&"obscura_scrape"));
    assert!(names.contains(&"obscura_serve"));
    assert!(names.contains(&"obscura_screenshot"));
    assert!(names.contains(&"obscura_extract_markdown"));
}

#[test]
fn unknown_method() {
    let resp = mcp::handle_request(&json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "nonexistent"
    }))
    .unwrap();
    let parsed: Value = serde_json::from_str(&resp).unwrap();
    assert_eq!(parsed["error"]["code"], -32601);
    assert!(parsed["error"]["message"]
        .as_str()
        .unwrap()
        .contains("nonexistent"));
}

#[test]
fn no_method_returns_none() {
    let resp = mcp::handle_request(&json!({"jsonrpc": "2.0", "id": 1}));
    assert!(resp.is_none());
}

// ── call_tool: parameter validation ────────────────────────────────────────

#[test]
fn fetch_missing_url() {
    let result = mcp::call_tool("obscura_fetch", &json!({}));
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("url"));
}

#[test]
fn screenshot_missing_params() {
    let result = mcp::call_tool("obscura_screenshot", &json!({"url": "https://example.com"}));
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("expression"));
}

#[test]
fn screenshot_empty_expression() {
    let result = mcp::call_tool(
        "obscura_screenshot",
        &json!({"url": "https://example.com", "expression": ""}),
    );
    assert_eq!(result["isError"], true);
}

#[test]
fn scrape_missing_urls() {
    let result = mcp::call_tool("obscura_scrape", &json!({}));
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("urls"));
}

#[test]
fn scrape_empty_urls() {
    let result = mcp::call_tool("obscura_scrape", &json!({"urls": []}));
    assert_eq!(result["isError"], true);
}

#[test]
fn extract_markdown_missing_url() {
    let result = mcp::call_tool("obscura_extract_markdown", &json!({}));
    assert_eq!(result["isError"], true);
}

#[test]
fn unknown_tool() {
    let result = mcp::call_tool("nonexistent_tool", &json!({}));
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Unknown tool"));
}

// ── text_result / error_result helpers ─────────────────────────────────────

#[test]
fn text_result_structure() {
    let result = mcp::text_result("hello");
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(result["content"][0]["text"], "hello");
    assert!(result.get("isError").is_none());
}

#[test]
fn error_result_structure() {
    let result = mcp::error_result("fail");
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(result["content"][0]["text"], "fail");
    assert_eq!(result["isError"], true);
}

// ── tools_list schema validation ───────────────────────────────────────────

#[test]
fn tools_have_required_fields() {
    let tools = mcp::tools_list();
    for tool in tools.as_array().unwrap() {
        assert!(tool["name"].is_string(), "tool missing name");
        assert!(tool["description"].is_string(), "tool missing description");
        assert!(
            tool["inputSchema"]["type"].is_string(),
            "tool missing inputSchema.type"
        );
        assert!(
            tool["inputSchema"]["properties"].is_object(),
            "tool missing inputSchema.properties"
        );
    }
}

#[test]
fn fetch_has_required_url() {
    let tools = mcp::tools_list();
    let fetch = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "obscura_fetch")
        .unwrap();
    let required = fetch["inputSchema"]["required"].as_array().unwrap();
    assert!(required.iter().any(|r| r == "url"));
}

#[test]
fn screenshot_has_required_url_and_expression() {
    let tools = mcp::tools_list();
    let screenshot = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "obscura_screenshot")
        .unwrap();
    let required: Vec<&str> = screenshot["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert!(required.contains(&"url"));
    assert!(required.contains(&"expression"));
}

#[test]
fn scrape_has_required_urls() {
    let tools = mcp::tools_list();
    let scrape = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "obscura_scrape")
        .unwrap();
    let required = scrape["inputSchema"]["required"].as_array().unwrap();
    assert!(required.iter().any(|r| r == "urls"));
}

// ── Integration: full stdio round-trip ─────────────────────────────────────

#[test]
fn stdio_roundtrip() {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let bin =
        std::env::var("OBSCURA_TEST_BIN").unwrap_or_else(|_| "target/release/obscura-mcp".into());

    let mut child = Command::new(&bin)
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| {
            panic!("Failed to spawn {bin}: {e}. Run `cargo build --release` first.")
        });

    // Write initialize request
    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .unwrap();
    }

    // Read response
    let mut buf = vec![0u8; 4096];
    let n = child.stdout.as_mut().unwrap().read(&mut buf).unwrap();
    let resp: Value = serde_json::from_slice(&buf[..n]).unwrap();

    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["serverInfo"]["name"], "obscura-mcp");
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");

    // Drop stdin to signal EOF
    drop(child.stdin.take());
    let _ = child.wait();
}
