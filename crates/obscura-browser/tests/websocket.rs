use std::io::{Read, Write};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use obscura_browser::{BrowserContext, Page};
use serde_json::json;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

fn spawn_page_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|b| b == b"\r\n\r\n") {
                        break;
                    }
                }
                let body = "<!doctype html><html><body>ws fixture</body></html>";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: sid=abc; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    format!("http://{address}")
}

/// Echoes text and binary; "bye" makes the server close with 4001 "done".
/// The first text frame sent to a client reports what the handshake carried.
async fn spawn_ws_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut seen = String::new();
                let callback = |req: &Request, mut resp: Response| {
                    let h = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    seen = format!("cookie={};origin={};ua={}", h("cookie"), h("origin"), !h("user-agent").is_empty());
                    if let Some(first) = h("sec-websocket-protocol").split(',').next().filter(|s| !s.is_empty()) {
                        resp.headers_mut().insert("sec-websocket-protocol", first.trim().parse().unwrap());
                    }
                    Ok(resp)
                };
                let mut ws = tokio_tungstenite::accept_hdr_async(stream, callback).await.unwrap();
                ws.send(Message::text(seen)).await.unwrap();
                while let Some(Ok(msg)) = ws.next().await {
                    match msg {
                        Message::Text(t) if t.as_str() == "bye" => {
                            let _ = ws
                                .close(Some(CloseFrame { code: CloseCode::from(4001), reason: "done".into() }))
                                .await;
                        }
                        Message::Text(_) | Message::Binary(_) => {
                            let _ = ws.send(msg).await;
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    port
}

async fn wait_for(page: &mut Page, expr: &str) -> serde_json::Value {
    for _ in 0..50 {
        page.settle(100).await;
        let v = page.evaluate(expr);
        if v != json!(null) && v != json!(false) {
            return v;
        }
    }
    page.evaluate(expr)
}

#[tokio::test(flavor = "current_thread")]
async fn websocket_round_trips_over_a_real_connection() {
    round_trip(false).await;
}

/// Stealth routes the socket through the wreq client (Chrome TLS fingerprint).
#[cfg(feature = "stealth")]
#[tokio::test(flavor = "current_thread")]
async fn websocket_round_trips_through_the_stealth_client() {
    round_trip(true).await;
}

async fn round_trip(stealth: bool) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let page_url = spawn_page_server();
    let ws_port = spawn_ws_server().await;
    let ctx = Arc::new(BrowserContext::with_storage_and_network("ws".into(), None, stealth, None, None, true));
    let mut page = Page::new("ws-page".into(), ctx);
    page.navigate(&format!("{page_url}/")).await.unwrap();

    page.evaluate(&format!(
        r#"(function() {{
            window.log = [];
            const ws = new WebSocket('ws://127.0.0.1:{ws_port}/chat', ['v2', 'v1']);
            ws.binaryType = 'arraybuffer';
            window.ws = ws;
            ws.onopen = (e) => {{
                log.push('open:' + ws.readyState + ':' + ws.protocol + ':' + e.isTrusted);
                ws.send('hello');
                ws.send(new Uint8Array([7, 8, 9]));
                ws.send(new Blob(['from-blob']));
                ws.send('bye');
            }};
            ws.addEventListener('message', (e) => {{
                log.push(typeof e.data === 'string' ? 'text:' + e.data : 'bin:' + Array.from(new Uint8Array(e.data)).join('.'));
            }});
            ws.onclose = (e) => {{ log.push('close:' + e.code + ':' + e.reason + ':' + e.wasClean + ':' + ws.readyState); window.done = true; }};
            ws.onerror = () => log.push('error');
            return 1;
        }})()"#
    ));

    assert_eq!(wait_for(&mut page, "window.done === true").await, json!(true));
    let log = page.evaluate("log.join(' | ')");
    let log = log.as_str().unwrap();
    assert_eq!(
        log,
        "open:1:v2:true | text:cookie=sid=abc;origin=http://127.0.0.1:".to_string()
            + &page_url.rsplit(':').next().unwrap().to_string()
            + ";ua=true | text:hello | bin:7.8.9 | bin:102.114.111.109.45.98.108.111.98 | close:4001:done:true:3"
    );

    // The same traffic is reported to CDP clients as Network.webSocket* events.
    let events = page.take_websocket_events();
    let methods: Vec<&str> = events.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods[..3],
        [
            "Network.webSocketCreated",
            "Network.webSocketWillSendHandshakeRequest",
            "Network.webSocketHandshakeResponseReceived",
        ]
    );
    assert_eq!(methods.last(), Some(&"Network.webSocketClosed"));
    assert!(events.iter().all(|(_, p)| p["requestId"] == json!("ws-1")));
    assert_eq!(events[2].1["response"]["status"], json!(101));
    // Sent and received frames interleave by timing; each direction is ordered.
    let frames = |method: &str| -> Vec<(u64, String)> {
        events
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| {
                let r = &p["response"];
                (r["opcode"].as_u64().unwrap(), r["payloadData"].as_str().unwrap().to_string())
            })
            .collect()
    };
    assert_eq!(
        frames("Network.webSocketFrameSent"),
        [(1, "hello".into()), (2, "BwgJ".into()), (2, "ZnJvbS1ibG9i".into()), (1, "bye".into())]
    );
    let received = frames("Network.webSocketFrameReceived");
    assert_eq!(received.len(), 4);
    assert!(received[0].1.starts_with("cookie=sid=abc"));
    assert_eq!(received[1..], [(1, "hello".into()), (2, "BwgJ".into()), (2, "ZnJvbS1ibG9i".into())]);
}

#[tokio::test(flavor = "current_thread")]
async fn failed_connection_fires_error_then_close_1006() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let page_url = spawn_page_server();
    // Bind then drop so nothing listens on the port.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let ctx = Arc::new(BrowserContext::with_storage_and_network("ws-fail".into(), None, false, None, None, true));
    let mut page = Page::new("ws-fail-page".into(), ctx);
    page.navigate(&format!("{page_url}/")).await.unwrap();
    page.evaluate(&format!(
        r#"(function() {{
            window.log = [];
            const ws = new WebSocket('ws://127.0.0.1:{port}/');
            ws.onopen = () => log.push('open');
            ws.onerror = () => log.push('error:' + ws.readyState);
            ws.onclose = (e) => {{ log.push('close:' + e.code + ':' + e.wasClean); window.done = true; }};
            return 1;
        }})()"#
    ));
    assert_eq!(wait_for(&mut page, "window.done === true").await, json!(true));
    assert_eq!(page.evaluate("log.join(',')"), json!("error:3,close:1006:false"));
}

#[tokio::test(flavor = "current_thread")]
async fn constructor_and_close_validate_like_chrome() {
    let page_url = spawn_page_server();
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let ctx = Arc::new(BrowserContext::with_storage_and_network("ws-validate".into(), None, false, None, None, true));
    let mut page = Page::new("ws-validate-page".into(), ctx);
    page.navigate(&format!("{page_url}/")).await.unwrap();
    let result = page.evaluate(
        r#"(function() {
            const out = [];
            const t = (f) => { try { f(); out.push('ok'); } catch (e) { out.push(e.name); } };
            t(() => new WebSocket('ftp://x.test/'));
            t(() => new WebSocket('wss://x.test/#frag'));
            t(() => new WebSocket('wss://x.test/', ['a', 'a']));
            const ws = new WebSocket('/relative');
            out.push(ws.url.startsWith('ws://127.0.0.1:'));
            t(() => ws.send('x'));
            t(() => ws.close(1001));
            out.push(WebSocket.OPEN, ws.CLOSED, String(ws));
            ws.close();
            return out.join(',');
        })()"#,
    );
    assert_eq!(
        result,
        json!("SyntaxError,SyntaxError,SyntaxError,true,InvalidStateError,InvalidAccessError,1,3,[object WebSocket]")
    );
}
