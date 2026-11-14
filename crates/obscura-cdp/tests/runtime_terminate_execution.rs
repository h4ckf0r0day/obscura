//! `Runtime.terminateExecution` interrupts synchronous page script.
//!
//! A busy script holds the connection thread, so every command queued behind
//! it (even browser-level `Target.getTargets`) waited for the script to end.
//! `Runtime.terminateExecution` is answered on the socket thread and fires the
//! connection's V8 watchdogs, so the script unwinds and the queue drains.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn pick_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

async fn start_server() -> u16 {
    let port = pick_port().await;
    tokio::task::spawn_local(async move {
        let _ = obscura_cdp::server::start_with_full_serve_options(
            port, "127.0.0.1", None, false, None, false, None, true,
        )
        .await;
    });
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return port;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("CDP server did not start");
}

/// Serves `body` as text/html to every request.
async fn serve_html(body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    port
}

async fn send(ws: &mut Ws, message: Value) {
    ws.send(Message::Text(message.to_string().into())).await.unwrap();
}

/// Wait for the response to `id`, skipping events and other responses.
async fn response(ws: &mut Ws, id: u64, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("no response to command {id} within {within:?}"));
        let message = tokio::time::timeout(remaining, ws.next())
            .await
            .unwrap_or_else(|_| panic!("no response to command {id} within {within:?}"))
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text).unwrap();
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }
}

async fn open_page(port: u16) -> (Ws, String) {
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/devtools/browser"))
        .await
        .unwrap();
    send(&mut ws, json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}})).await;
    let target = response(&mut ws, 1, Duration::from_secs(10)).await;
    let target_id = target["result"]["targetId"].as_str().unwrap().to_string();
    send(&mut ws, json!({"id": 2, "method": "Target.attachToTarget", "params": {"targetId": target_id, "flatten": true}})).await;
    let attached = response(&mut ws, 2, Duration::from_secs(10)).await;
    let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
    (ws, session)
}

async fn evaluate(ws: &mut Ws, session: &str, id: u64, expression: &str) -> Value {
    send(ws, json!({"id": id, "sessionId": session, "method": "Runtime.evaluate",
        "params": {"expression": expression, "returnByValue": true}})).await;
    response(ws, id, Duration::from_secs(5)).await
}

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_interrupts_a_busy_evaluate() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let port = start_server().await;
            let (mut ws, session) = open_page(port).await;
            // The first command on a blank target creates its runtime; a
            // command's watchdog is armed only for a runtime that exists.
            evaluate(&mut ws, &session, 9, "0").await;

            send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Runtime.evaluate",
                "params": {"expression": "while (true) {}", "returnByValue": true}})).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            send(&mut ws, json!({"id": 11, "method": "Target.getTargets"})).await;
            let started = Instant::now();
            send(&mut ws, json!({"id": 12, "sessionId": session, "method": "Runtime.terminateExecution"})).await;

            let terminated = response(&mut ws, 12, Duration::from_secs(2)).await;
            assert_eq!(terminated["result"], json!({}), "{terminated}");
            let busy = response(&mut ws, 10, Duration::from_secs(3)).await;
            assert!(
                busy.get("error").is_some() || busy["result"].get("exceptionDetails").is_some(),
                "the terminated evaluation must not report success: {busy}"
            );
            let targets = response(&mut ws, 11, Duration::from_secs(3)).await;
            assert!(targets["result"]["targetInfos"].is_array(), "{targets}");
            assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());

            let after = evaluate(&mut ws, &session, 13, "1 + 1").await;
            assert_eq!(after["result"]["result"]["value"].as_f64(), Some(2.0), "isolate reusable: {after}");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_interrupts_a_busy_timer_task() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let port = start_server().await;
            let (mut ws, session) = open_page(port).await;

            let scheduled = evaluate(&mut ws, &session, 10,
                "setTimeout(() => { while (true) {} }, 0); 'scheduled'").await;
            assert_eq!(scheduled["result"]["result"]["value"], json!("scheduled"), "{scheduled}");
            tokio::time::sleep(Duration::from_millis(300)).await;
            send(&mut ws, json!({"id": 11, "method": "Target.getTargets"})).await;
            let started = Instant::now();
            send(&mut ws, json!({"id": 12, "sessionId": session, "method": "Runtime.terminateExecution"})).await;

            response(&mut ws, 12, Duration::from_secs(2)).await;
            let targets = response(&mut ws, 11, Duration::from_secs(3)).await;
            assert!(targets["result"]["targetInfos"].is_array(), "{targets}");
            // The event-loop watchdog alone would allow the task five seconds.
            assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());

            let after = evaluate(&mut ws, &session, 13, "1 + 1").await;
            assert_eq!(after["result"]["result"]["value"].as_f64(), Some(2.0), "isolate reusable: {after}");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_interrupts_a_busy_parser_script_during_navigation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let site = serve_html(
                "<!doctype html><title>busy</title><script>while (true) {}</script><p id=after>after</p>",
            )
            .await;
            let port = start_server().await;
            let (mut ws, session) = open_page(port).await;

            send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Page.navigate",
                "params": {"url": format!("http://127.0.0.1:{site}/")}})).await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            let started = Instant::now();
            send(&mut ws, json!({"id": 11, "sessionId": session, "method": "Runtime.terminateExecution"})).await;
            response(&mut ws, 11, Duration::from_secs(2)).await;

            // Without the interrupt the navigation waits for the 30 s script deadline.
            let navigated = response(&mut ws, 10, Duration::from_secs(5)).await;
            assert!(navigated.get("result").is_some(), "{navigated}");
            assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

            let after = evaluate(&mut ws, &session, 12, "document.title + ' ' + !!document.getElementById('after')").await;
            assert_eq!(after["result"]["result"]["value"], json!("busy true"), "{after}");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_without_running_script_is_a_no_op() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let port = start_server().await;
            let (mut ws, session) = open_page(port).await;

            send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Runtime.terminateExecution"})).await;
            let terminated = response(&mut ws, 10, Duration::from_secs(2)).await;
            assert_eq!(terminated["result"], json!({}), "{terminated}");
            // No termination may be left pending for the next script.
            let after = evaluate(&mut ws, &session, 11, "'still runs'").await;
            assert_eq!(after["result"]["result"]["value"], json!("still runs"), "{after}");
        })
        .await;
}
