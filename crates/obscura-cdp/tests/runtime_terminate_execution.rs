//! `Runtime.terminateExecution` interrupts synchronous page script.
//!
//! A busy script holds the connection thread, so every command queued behind
//! it (even browser-level `Target.getTargets`) waited for the script to end.
//! `Runtime.terminateExecution` is answered on the socket thread and fires the
//! page's V8 watchdogs, so the script unwinds and the queue drains.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

struct Ws {
    inner: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    replies: std::collections::HashMap<u64, Value>,
}

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
    ws.inner.send(Message::Text(message.to_string().into())).await.unwrap();
}

/// Buffer other replies: the socket interrupt can overtake queued commands.
async fn response(ws: &mut Ws, id: u64, within: Duration) -> Value {
    if let Some(reply) = ws.replies.remove(&id) { return reply; }
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("no response to command {id} within {within:?}"));
        let message = tokio::time::timeout(remaining, ws.inner.next())
            .await
            .unwrap_or_else(|_| panic!("no response to command {id} within {within:?}"))
            .unwrap()
            .unwrap();
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text).unwrap();
            if let Some(reply_id) = value.get("id").and_then(Value::as_u64) {
                if reply_id == id { return value; }
                ws.replies.insert(reply_id, value);
            }
        }
    }
}

async fn open_page(port: u16) -> (Ws, String) {
    let (inner, _) = connect_async(format!("ws://127.0.0.1:{port}/devtools/browser"))
        .await
        .unwrap();
    let mut ws = Ws { inner, replies: Default::default() };
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


async fn another_page(ws: &mut Ws, id: u64) -> (String, String) {
    send(ws, json!({"id": id, "method": "Target.createTarget", "params": {"url": "about:blank"}})).await;
    let created = response(ws, id, Duration::from_secs(5)).await;
    let target = created["result"]["targetId"].as_str().unwrap().to_string();
    send(ws, json!({"id": id + 1, "method": "Target.attachToTarget", "params": {"targetId": target, "flatten": true}})).await;
    let attached = response(ws, id + 1, Duration::from_secs(5)).await;
    (target, attached["result"]["sessionId"].as_str().unwrap().to_string())
}

async fn wrong_page_does_not_unblock(ws: &mut Ws, other: &str, pending_id: u64) {
    send(ws, json!({"id": 80, "sessionId": other, "method": "Runtime.terminateExecution"})).await;
    let reply = response(ws, 80, Duration::from_secs(2)).await;
    assert_eq!(reply["result"], json!({}), "{reply}");
    assert!(tokio::time::timeout(Duration::from_millis(100),
        response(ws, pending_id, Duration::from_secs(1))).await.is_err(),
        "terminating another page must leave the active task running");
}

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_interrupts_a_busy_evaluate() { check_busy_evaluate().await; }

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_with_command_deadline_disabled() {
    std::env::set_var("OBSCURA_CDP_COMMAND_TIMEOUT_MS", "0");
    check_busy_evaluate().await;
}

async fn check_busy_evaluate() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let port = start_server().await;
            let (mut ws, session) = open_page(port).await;
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
            let (_, other) = another_page(&mut ws, 3).await;

            let scheduled = evaluate(&mut ws, &session, 10,
                "setTimeout(() => { while (true) {} }, 0); 'scheduled'").await;
            assert_eq!(scheduled["result"]["result"]["value"], json!("scheduled"), "{scheduled}");
            tokio::time::sleep(Duration::from_millis(300)).await;
            send(&mut ws, json!({"id": 11, "method": "Target.getTargets"})).await;
            wrong_page_does_not_unblock(&mut ws, &other, 11).await;
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
            let (_, other) = another_page(&mut ws, 3).await;

            send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Page.navigate",
                "params": {"url": format!("http://127.0.0.1:{site}/")}})).await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            wrong_page_does_not_unblock(&mut ws, &other, 10).await;
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

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_routes_sessions_and_accepts_escaped_json() {
    tokio::task::LocalSet::new().run_until(async {
        let port = start_server().await;
        let (mut ws, session) = open_page(port).await;
        let (_, other) = another_page(&mut ws, 3).await;
        let (mut independent, independent_session) = open_page(port).await;
        let (closed_target, closed_session) = another_page(&mut ws, 90).await;
        send(&mut ws, json!({"id": 92, "method": "Target.closeTarget", "params": {"targetId": closed_target}})).await;
        assert_eq!(response(&mut ws, 92, Duration::from_secs(5)).await["result"]["success"], true);
        send(&mut ws, json!({"id": 5, "method": "Target.attachToTarget", "params": {"targetId": "page-1", "flatten": true}})).await;
        let alias = response(&mut ws, 5, Duration::from_secs(5)).await["result"]["sessionId"].as_str().unwrap().to_string();
        send(&mut ws, json!({"id": 6, "method": "Target.attachToTarget", "params": {"targetId": "page-1", "flatten": true}})).await;
        let detached = response(&mut ws, 6, Duration::from_secs(5)).await["result"]["sessionId"].as_str().unwrap().to_string();
        send(&mut ws, json!({"id": 7, "method": "Target.detachFromTarget", "params": {"sessionId": detached}})).await;
        response(&mut ws, 7, Duration::from_secs(5)).await;
        send(&mut ws, json!({"id": 8, "method": "Target.attachToBrowserTarget"})).await;
        let browser = response(&mut ws, 8, Duration::from_secs(5)).await["result"]["sessionId"].as_str().unwrap().to_string();
        send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Runtime.evaluate", "params": {"expression": "while(true){}"}})).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        wrong_page_does_not_unblock(&mut ws, &other, 10).await;
        for (id, bad_session) in [(20, "nonexistent"), (21, detached.as_str()), (24, closed_session.as_str())] {
            send(&mut ws, json!({"id": id, "sessionId": bad_session, "method": "Runtime.terminateExecution"})).await;
            let reply = response(&mut ws, id, Duration::from_secs(2)).await;
            assert_eq!(reply["error"]["code"], -32001, "{reply}");
        }
        for (id, route) in [(22, None), (23, Some(browser))] {
            send(&mut ws, json!({"id": id, "sessionId": route, "method": "Runtime.terminateExecution"})).await;
            let reply = response(&mut ws, id, Duration::from_secs(2)).await;
            assert!(reply.get("error").is_some(), "root/browser sessions have no page: {reply}");
        }
        send(&mut independent, json!({"id": 10, "sessionId": independent_session, "method": "Runtime.evaluate", "params": {"expression": "while(true){}"}})).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        send(&mut independent, json!({"id": 9, "sessionId": independent_session, "method": "Runtime.terminateExecution"})).await;
        let reply = response(&mut independent, 9, Duration::from_secs(2)).await;
        assert_eq!(reply["result"], json!({}), "{reply}");
        let other_busy = response(&mut independent, 10, Duration::from_secs(2)).await;
        assert!(other_busy.get("error").is_some() || other_busy["result"].get("exceptionDetails").is_some(), "{other_busy}");
        assert!(tokio::time::timeout(Duration::from_millis(100), response(&mut ws, 10, Duration::from_secs(1))).await.is_err());
        let escaped = json!({"id": 30, "sessionId": alias, "method": "Runtime.terminateExecution"}).to_string()
            .replace("Runtime.terminateExecution", "Runtime.\\u0074erminateExecution");
        ws.inner.send(Message::Text(escaped.into())).await.unwrap();
        let terminated = response(&mut ws, 30, Duration::from_secs(2)).await;
        assert_eq!(terminated["result"], json!({}), "{terminated}");
        let busy = response(&mut ws, 10, Duration::from_secs(2)).await;
        assert!(busy.get("error").is_some() || busy["result"].get("exceptionDetails").is_some(), "{busy}");
        let after = evaluate(&mut ws, &session, 31, "1+1").await;
        assert_eq!(after["result"]["result"]["value"].as_f64(), Some(2.0), "{after}");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn response_helper_preserves_replies_that_arrive_before_the_interrupt_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket.send(Message::Text(json!({"id": 1, "result": {"finished": true}}).to_string().into())).await.unwrap();
        socket.send(Message::Text(json!({"id": 2, "result": {}}).to_string().into())).await.unwrap();
    });
    let (inner, _) = connect_async(format!("ws://127.0.0.1:{port}/")).await.unwrap();
    let mut ws = Ws { inner, replies: Default::default() };
    assert_eq!(response(&mut ws, 2, Duration::from_secs(1)).await["result"], json!({}));
    assert_eq!(response(&mut ws, 1, Duration::from_secs(1)).await["result"]["finished"], true);
    peer.await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_during_network_idle_does_not_fail_navigation() {
    tokio::task::LocalSet::new().run_until(async {
        let port = start_server().await;
        let (mut ws, session) = open_page(port).await;
        send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Page.navigate",
            "params": {"url": "data:text/html,<title>quiet</title><body>ready", "waitUntil": "networkidle0"}})).await;
        // Network-idle requires a 500ms quiet window after the document loads.
        tokio::time::sleep(Duration::from_millis(200)).await;
        send(&mut ws, json!({"id": 11, "sessionId": session, "method": "Runtime.terminateExecution"})).await;
        assert_eq!(response(&mut ws, 11, Duration::from_secs(2)).await["result"], json!({}));
        let navigated = response(&mut ws, 10, Duration::from_secs(3)).await;
        assert!(navigated.get("error").is_none() && navigated["result"].get("errorText").is_none(), "idle interruption must not fail navigation: {navigated}");
        let after = evaluate(&mut ws, &session, 12, "document.title").await;
        assert_eq!(after["result"]["result"]["value"], "quiet", "{after}");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn terminate_execution_during_script_fetch_does_not_poison_the_script() {
    tokio::task::LocalSet::new().run_until(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let site = listener.local_addr().unwrap().port();
        let (requested, request_seen) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let resources = tokio::spawn(async move {
            let mut requested = Some(requested);
            let mut released = Some(released);
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                let script = String::from_utf8_lossy(&buf[..n]).starts_with("GET /gate.js ");
                let (mime, body) = if script {
                    requested.take().unwrap().send(()).unwrap();
                    released.take().unwrap().await.unwrap();
                    ("text/javascript", "globalThis.delayedRuns=1;document.title='script ran';")
                } else {
                    ("text/html", "<!doctype html><title>loading</title><script src=/gate.js></script><p id=after>after</p>")
                };
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let port = start_server().await;
        let (mut ws, session) = open_page(port).await;
        send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Page.navigate",
            "params": {"url": format!("http://127.0.0.1:{site}/"), "waitUntil": "load"}})).await;
        tokio::time::timeout(Duration::from_secs(3), request_seen).await.unwrap().unwrap();
        send(&mut ws, json!({"id": 11, "sessionId": session, "method": "Runtime.terminateExecution"})).await;
        assert_eq!(response(&mut ws, 11, Duration::from_secs(2)).await["result"], json!({}));
        release.send(()).unwrap();
        let navigated = response(&mut ws, 10, Duration::from_secs(3)).await;
        assert!(navigated.get("error").is_none() && navigated["result"].get("errorText").is_none(), "{navigated}");
        let after = evaluate(&mut ws, &session, 12, "[globalThis.delayedRuns||0,document.title,!!document.getElementById('after')]").await;
        assert_eq!(after["result"]["result"]["value"], json!([1, "script ran", true]), "idle interruption must not poison later script: {after}");
        resources.abort();
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn manual_termination_preserves_the_original_parser_phase_deadline() {
    std::env::set_var("OBSCURA_SCRIPT_DEADLINE_MS", "500");
    tokio::task::LocalSet::new().run_until(async {
        let site = serve_html("<!doctype html><title>phase</title><script>globalThis.firstStarted=true;while(true){};globalThis.firstFinished=true;</script><script>globalThis.secondStarted=true;var secondStart=Date.now();while(Date.now()-secondStart<4000){};globalThis.secondFinished=true;</script>").await;
        let port = start_server().await;
        let (mut ws, session) = open_page(port).await;
        let began = Instant::now();
        send(&mut ws, json!({"id": 10, "sessionId": session, "method": "Page.navigate",
            "params": {"url": format!("http://127.0.0.1:{site}/"), "waitUntil": "load"}})).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        send(&mut ws, json!({"id": 11, "sessionId": session, "method": "Runtime.terminateExecution"})).await;
        assert_eq!(response(&mut ws, 11, Duration::from_secs(2)).await["result"], json!({}));
        let navigated = response(&mut ws, 10, Duration::from_secs(6)).await;
        assert!(navigated.get("error").is_none(), "{navigated}");
        let elapsed = began.elapsed();
        let after = evaluate(&mut ws, &session, 12,
            "[!!globalThis.firstStarted,!!globalThis.firstFinished,!!globalThis.secondStarted,!!globalThis.secondFinished]").await;
        assert_eq!(after["result"]["result"]["value"], json!([true, false, true, false]),
            "manual interruption must preserve the phase watchdog for the second short script (elapsed {elapsed:?}): {after}");
        // The original phase deadline is 500ms plus its 1000ms hard-stop grace.
        // The finite second loop keeps a broken implementation bounded at 4s.
        assert!(elapsed < Duration::from_secs(3), "phase deadline was lost: {elapsed:?}");
    }).await;
}
