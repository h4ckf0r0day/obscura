//! The page `WebSocket` must be a real client: it opens only when a server
//! accepts the handshake, carries messages both ways, and reports failures and
//! closures through `error` / `close` the way browsers do.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use obscura_browser::{BrowserContext, Page};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

/// What the server observed, for assertions about what actually crossed the
/// wire rather than what the page believes happened.
#[derive(Default, Debug)]
struct ServerLog {
    connections: usize,
    handshakes: Vec<Handshake>,
    received: Vec<String>,
    close_codes: Vec<Option<u16>>,
}

#[derive(Debug, Clone)]
struct Handshake {
    path: String,
    origin: Option<String>,
    cookie: Option<String>,
    protocols: Option<String>,
}

fn header(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// A WebSocket server on its own thread and runtime, so it keeps serving
/// while the page's runtime is parked inside `settle`.
///
/// Paths: `/echo` echoes every data message (a text `close-me` makes the
/// server close with 4001), `/greet` sends a message as soon as it connects,
/// `/drop` accepts and then drops the TCP connection without a Close frame.
/// A client offering the `superchat` subprotocol has it selected.
fn spawn_ws_server() -> (SocketAddr, Arc<Mutex<ServerLog>>) {
    let log = Arc::new(Mutex::new(ServerLog::default()));
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    let server_log = Arc::clone(&log);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let log = Arc::clone(&server_log);
                tokio::spawn(async move {
                    log.lock().unwrap().connections += 1;
                    let mut path = String::new();
                    let callback = |req: &Request, mut resp: Response| {
                        path = req.uri().path().to_string();
                        let protocols = header(req, "sec-websocket-protocol");
                        if protocols
                            .as_deref()
                            .is_some_and(|p| p.split(',').any(|p| p.trim() == "superchat"))
                        {
                            resp.headers_mut().insert(
                                "sec-websocket-protocol",
                                HeaderValue::from_static("superchat"),
                            );
                        }
                        log.lock().unwrap().handshakes.push(Handshake {
                            path: path.clone(),
                            origin: header(req, "origin"),
                            cookie: header(req, "cookie"),
                            protocols,
                        });
                        Ok(resp)
                    };
                    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await
                    else {
                        return;
                    };
                    match path.as_str() {
                        "/drop" => {
                            drop(ws);
                        }
                        "/greet" => {
                            let _ = ws.send(Message::text("welcome")).await;
                            while let Some(Ok(_)) = ws.next().await {}
                        }
                        _ => {
                            while let Some(Ok(message)) = ws.next().await {
                                match message {
                                    Message::Text(text) if text.as_str() == "close-me" => {
                                        log.lock().unwrap().received.push(text.to_string());
                                        let _ = ws
                                            .close(Some(CloseFrame {
                                                code: CloseCode::from(4001),
                                                reason: "server bye".into(),
                                            }))
                                            .await;
                                    }
                                    Message::Text(text) => {
                                        log.lock().unwrap().received.push(text.to_string());
                                        let _ = ws.send(Message::Text(text)).await;
                                    }
                                    Message::Binary(bytes) => {
                                        log.lock()
                                            .unwrap()
                                            .received
                                            .push(format!("binary:{:?}", bytes.as_ref()));
                                        let _ = ws.send(Message::Binary(bytes)).await;
                                    }
                                    Message::Close(frame) => {
                                        log.lock()
                                            .unwrap()
                                            .close_codes
                                            .push(frame.map(|f| u16::from(f.code)));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                });
            }
        });
    });
    (addr_rx.recv().unwrap(), log)
}

/// Serves one HTML page that sets a cookie, so the socket has an origin and a
/// cookie to carry.
fn spawn_http_page() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = "<!doctype html><html><body>websocket fixture</body></html>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: session=abc123; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    format!("http://{addr}/")
}

async fn page_on_fixture(allow_private_network: bool) -> Page {
    let context = Arc::new(BrowserContext::with_storage_and_network(
        "websocket".to_string(),
        None,
        false,
        None,
        None,
        allow_private_network,
    ));
    let mut page = Page::new("websocket-page".to_string(), context);
    if allow_private_network {
        page.navigate(&spawn_http_page()).await.unwrap();
    } else {
        page.navigate("about:blank").await.unwrap();
    }
    page.evaluate("window.__log = []");
    page
}

/// Settle until `condition` (a JS expression) is truthy or 10s pass.
async fn settle_until(page: &mut Page, condition: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if page.evaluate(condition) == serde_json::json!(true) {
            return;
        }
        page.settle(100).await;
        // An idle page settles immediately; give the server wall-clock time
        // to answer before pumping again.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Run `body` as a function body; `evaluate` alone treats input as a single
/// expression.
fn run(page: &mut Page, body: &str) -> serde_json::Value {
    page.evaluate(&format!("(() => {{\n{body}\n}})()"))
}

fn log(page: &mut Page) -> serde_json::Value {
    page.evaluate("JSON.stringify(window.__log)")
        .as_str()
        .map(|s| serde_json::from_str(s).unwrap())
        .unwrap_or(serde_json::Value::Null)
}

/// Record every lifecycle event of `ws` into `window.__log`.
const RECORD: &str = r#"
    window.__record = (ws) => {
        ws.addEventListener('open', () => __log.push(['open', ws.readyState, ws.protocol]));
        ws.addEventListener('message', (e) => __log.push(['message', typeof e.data === 'string' ? e.data : Object.prototype.toString.call(e.data), e.origin]));
        ws.addEventListener('error', () => __log.push(['error', ws.readyState]));
        ws.addEventListener('close', (e) => __log.push(['close', e.code, e.reason, e.wasClean, ws.readyState]));
        return ws;
    };
    return true;
"#;

#[tokio::test(flavor = "current_thread")]
async fn open_fires_only_after_a_real_handshake_and_messages_round_trip() {
    let (addr, server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!(
            r#"
        window.__ws = __record(new WebSocket('ws://{addr}/echo'));
        __log.push(['constructed', __ws.readyState]);
        __ws.addEventListener('open', () => __ws.send('hello'));
        return true;
        "#
        ),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'message')").await;

    assert_eq!(
        log(&mut page),
        serde_json::json!([
            ["constructed", 0],
            ["open", 1, ""],
            ["message", "hello", format!("ws://{addr}")],
        ])
    );
    let server = server.lock().unwrap();
    assert_eq!(server.connections, 1);
    assert_eq!(server.received, vec!["hello".to_string()]);
}

#[tokio::test(flavor = "current_thread")]
async fn connecting_to_a_closed_port_reports_error_then_abnormal_close() {
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);

    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!("window.__ws = __record(new WebSocket('ws://{dead_addr}/')); return true;"),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'close')").await;

    assert_eq!(
        log(&mut page),
        serde_json::json!([["error", 3], ["close", 1006, "", false, 3]])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn send_while_connecting_throws_invalid_state_error() {
    let (addr, _server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    let result = page.evaluate(&format!(
        r#"
        (() => {{
            const ws = new WebSocket('ws://{addr}/echo');
            try {{ ws.send('too early'); return 'no throw'; }}
            catch (e) {{ return e.name; }}
        }})()
        "#
    ));
    assert_eq!(result, serde_json::json!("InvalidStateError"));
}

#[tokio::test(flavor = "current_thread")]
async fn server_initiated_message_is_delivered_without_client_traffic() {
    let (addr, _server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!("window.__ws = __record(new WebSocket('ws://{addr}/greet')); return true;"),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'message')").await;
    assert_eq!(
        log(&mut page),
        serde_json::json!([
            ["open", 1, ""],
            ["message", "welcome", format!("ws://{addr}")]
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn binary_messages_follow_binary_type() {
    let (addr, server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(
        &mut page,
        &format!(
            r#"
        window.__ws = new WebSocket('ws://{addr}/echo');
        __ws.binaryType = 'arraybuffer';
        __ws.onopen = () => {{
            const bytes = new Uint8Array([0, 128, 255, 16]);
            __ws.send(bytes);
            bytes[0] = 99; // mutation after send() must not change the frame
        }};
        __ws.onmessage = (e) => {{
            __log.push([Object.prototype.toString.call(e.data), Array.from(new Uint8Array(e.data))]);
            __ws.binaryType = 'blob';
            __ws.send(new Blob([new Uint8Array([7, 8])]));
            __ws.onmessage = (e2) => {{
                __log.push([Object.prototype.toString.call(e2.data), e2.data.size]);
            }};
        }};
        return true;
        "#
        ),
    );
    settle_until(&mut page, "__log.length >= 2").await;
    assert_eq!(
        log(&mut page),
        serde_json::json!([
            ["[object ArrayBuffer]", [0, 128, 255, 16]],
            ["[object Blob]", 2]
        ])
    );
    assert_eq!(
        server.lock().unwrap().received,
        vec![
            "binary:[0, 128, 255, 16]".to_string(),
            "binary:[7, 8]".to_string()
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn client_close_performs_the_closing_handshake() {
    let (addr, server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!(
            r#"
        window.__ws = __record(new WebSocket('ws://{addr}/echo'));
        __ws.onopen = () => {{
            __ws.send('before close');
            __ws.close(4000, 'done');
            __log.push(['after close()', __ws.readyState]);
            __ws.send('after close');
            __log.push(['buffered', __ws.bufferedAmount > 0]);
        }};
        return true;
        "#
        ),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'close')").await;

    assert_eq!(
        log(&mut page),
        serde_json::json!([
            ["open", 1, ""],
            ["after close()", 2],
            ["buffered", true],
            ["close", 4000, "done", true, 3],
        ])
    );
    let server = server.lock().unwrap();
    assert_eq!(server.received, vec!["before close".to_string()]);
    assert_eq!(server.close_codes, vec![Some(4000)]);
}

#[tokio::test(flavor = "current_thread")]
async fn server_close_code_and_reason_reach_the_page() {
    let (addr, _server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!(
            r#"
        window.__ws = __record(new WebSocket('ws://{addr}/echo'));
        __ws.onopen = () => __ws.send('close-me');
        return true;
        "#
        ),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'close')").await;
    assert_eq!(
        log(&mut page),
        serde_json::json!([["open", 1, ""], ["close", 4001, "server bye", true, 3]])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_connection_is_an_unclean_close() {
    let (addr, _server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!("window.__ws = __record(new WebSocket('ws://{addr}/drop')); return true;"),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'close')").await;
    assert_eq!(
        log(&mut page),
        serde_json::json!([["open", 1, ""], ["error", 3], ["close", 1006, "", false, 3]])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn handshake_carries_origin_cookies_and_subprotocols() {
    let (addr, server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(&mut page, &format!(
        "window.__ws = __record(new WebSocket('ws://{addr}/echo', ['chat', 'superchat'])); return true;"
    ));
    settle_until(&mut page, "__log.some(e => e[0] === 'open')").await;

    assert_eq!(
        log(&mut page),
        serde_json::json!([["open", 1, "superchat"]])
    );
    let page_origin = page.evaluate("location.origin");
    let handshake = server.lock().unwrap().handshakes[0].clone();
    assert_eq!(handshake.path, "/echo");
    assert_eq!(handshake.origin.as_deref(), page_origin.as_str());
    assert_eq!(handshake.cookie.as_deref(), Some("session=abc123"));
    assert_eq!(handshake.protocols.as_deref(), Some("chat, superchat"));
}

#[tokio::test(flavor = "current_thread")]
async fn relative_and_http_urls_resolve_to_ws_urls() {
    let mut page = page_on_fixture(true).await;
    let urls = page.evaluate(
        r#"
        (() => {
            const a = new WebSocket('/socket?x=1');
            const b = new WebSocket(location.origin + '/other');
            const out = [a.url, b.url];
            a.close(); b.close();
            return out;
        })()
        "#,
    );
    let origin = page.evaluate("location.origin");
    let ws_origin = origin.as_str().unwrap().replacen("http:", "ws:", 1);
    assert_eq!(
        urls,
        serde_json::json!([
            format!("{ws_origin}/socket?x=1"),
            format!("{ws_origin}/other"),
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn constructor_and_close_reject_invalid_arguments() {
    let mut page = page_on_fixture(true).await;
    let result = page.evaluate(
        r#"
        (() => {
            const name = (fn) => { try { fn(); return 'ok'; } catch (e) { return e.name; } };
            const ws = new WebSocket('ws://127.0.0.1:9/');
            const out = [
                name(() => new WebSocket('ftp://example.com/')),
                name(() => new WebSocket('ws://example.com/#frag')),
                name(() => new WebSocket('ws://example.com/', ['a', 'a'])),
                name(() => new WebSocket('ws://example.com/', 'not a token')),
                name(() => ws.close(1001)),
                name(() => ws.close(1000, 'x'.repeat(124))),
            ];
            ws.close();
            return out;
        })()
        "#,
    );
    assert_eq!(
        result,
        serde_json::json!([
            "SyntaxError",
            "SyntaxError",
            "SyntaxError",
            "SyntaxError",
            "InvalidAccessError",
            "SyntaxError",
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn close_while_connecting_fails_the_connection() {
    let (addr, _server) = spawn_ws_server();
    let mut page = page_on_fixture(true).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!(
            r#"
        window.__ws = __record(new WebSocket('ws://{addr}/echo'));
        __ws.close();
        __log.push(['after close()', __ws.readyState]);
        return true;
        "#
        ),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'close')").await;
    page.settle(200).await;
    assert_eq!(
        log(&mut page),
        serde_json::json!([
            ["after close()", 2],
            ["error", 3],
            ["close", 1006, "", false, 3]
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn private_network_policy_applies_to_websockets() {
    let (addr, server) = spawn_ws_server();
    let mut page = page_on_fixture(false).await;
    run(&mut page, RECORD);
    run(
        &mut page,
        &format!("window.__ws = __record(new WebSocket('ws://{addr}/echo')); return true;"),
    );
    settle_until(&mut page, "__log.some(e => e[0] === 'close')").await;
    assert_eq!(
        log(&mut page),
        serde_json::json!([["error", 3], ["close", 1006, "", false, 3]])
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(server.lock().unwrap().connections, 0);
}
