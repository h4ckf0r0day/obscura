use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::LocalSet;
use tokio::time::{sleep, timeout};

async fn rpc(port: u16, method: &str, params: Value) -> Value {
    timeout(Duration::from_secs(3), async {
        let body = json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}).to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        let response: Value = serde_json::from_str(body).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        let result = response["result"].clone();
        assert_ne!(result["isError"], json!(true), "{result}");
        result
    }).await.expect("idle browser work must not block MCP requests")
}

#[tokio::test(flavor = "current_thread")]
async fn page_callbacks_progress_without_more_http_tool_calls() {
    std::env::remove_var("OBSCURA_MCP_TOKEN");
    std::env::remove_var("OBSCURA_STORAGE_DIR");
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture_port = fixture.local_addr().unwrap().port();
    let (sent, arrived) = tokio::sync::oneshot::channel();
    let fixture_task = tokio::spawn(async move {
        let (stream, _) = fixture.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader.read_line(&mut request).await.unwrap();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).await.unwrap() == 0 || header == "\r\n" { break; }
        }
        reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n").await.unwrap();
        let _ = sent.send(request);
    });
    let local = LocalSet::new();
    let server = local.spawn_local(obscura_mcp::http::run(
        "127.0.0.1".into(), port, None, None, false,
    ));
    local.run_until(async {
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() { break; }
            sleep(Duration::from_millis(10)).await;
        }
        let tabs = rpc(port, "tools/call", json!({"name":"browser_tab_list", "arguments":{}})).await;
        assert_eq!(tabs["content"][0]["text"], json!("No tabs open."), "an unarmed pump must not create a page");
        rpc(port, "tools/call", json!({"name":"browser_navigate", "arguments":{
            "url":"data:text/html,<p>Waiting</p>"
        }})).await;
        // A distant timer must remain preemptible by a new RPC, not hold the owner loop.
        rpc(port, "tools/call", json!({"name":"browser_evaluate", "arguments":{
            "expression":"(function(){globalThis.distantTimer=setTimeout(()=>{},60000);return 'armed';})()"
        }})).await;
        rpc(port, "ping", json!({})).await;
        rpc(port, "tools/call", json!({"name":"browser_evaluate", "arguments":{
            "expression":format!("(function(){{clearTimeout(distantTimer);setTimeout(()=>fetch('http://127.0.0.1:{fixture_port}/idle-callback'),30);return 'queued';}})()")
        }})).await;

        // No tool call pumps the page here. Only the HTTP owner can run the timer/fetch.
        let request = timeout(Duration::from_secs(3), arrived).await
            .expect("HTTP MCP must drive idle page callbacks")
            .expect("fixture request");
        assert!(request.starts_with("GET /idle-callback "), "{request}");
        rpc(port, "ping", json!({})).await;

        let navigation = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let navigation_port = navigation.local_addr().unwrap().port();
        let (started, navigating) = tokio::sync::oneshot::channel();
        let navigation_task = tokio::spawn(async move {
            let (stream, _) = navigation.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" { break; }
            }
            let _ = started.send(());
            // A command arrives while native navigation is awaiting its response.
            sleep(Duration::from_millis(100)).await;
            reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<p>Landed</p>").await.unwrap();
        });
        rpc(port, "tools/call", json!({"name":"browser_evaluate", "arguments":{
            "expression":format!("(function(){{setTimeout(()=>location.href='http://127.0.0.1:{navigation_port}/landed',30);return 'queued';}})()")
        }})).await;
        timeout(Duration::from_secs(3), navigating).await.expect("idle navigation must start").unwrap();
        rpc(port, "ping", json!({})).await;
        let snapshot = rpc(port, "tools/call", json!({"name":"browser_snapshot", "arguments":{}})).await;
        assert!(snapshot["content"][0]["text"].as_str().unwrap().ends_with("\n\nLanded"), "incoming RPC must not discard navigation: {snapshot}");
        navigation_task.await.unwrap();

        rpc(port, "tools/call", json!({"name":"browser_close", "arguments":{}})).await;
        let tabs = rpc(port, "tools/call", json!({"name":"browser_tab_list", "arguments":{}})).await;
        assert_eq!(tabs["content"][0]["text"], json!("No tabs open."), "idle driving must not reopen a closed page");
        server.abort();
    }).await;
    fixture_task.await.unwrap();
}
