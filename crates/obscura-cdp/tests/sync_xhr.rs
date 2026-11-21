// Regression for issue #908: `xhr.open(method, url, false)` must perform the
// request before send() returns. It used to ignore the flag, so the line after
// send() saw status 0 and empty responseText. Legacy ASP.NET apps (iasWorld
// county property sites) check their session with a sync SOAP call and reload
// the page when it "fails".
//
// The server runs on its own OS thread: a sync XHR blocks the page thread, so
// a server spawned on the test's current-thread runtime could never answer.

use std::io::{Read, Write};
use std::net::TcpListener;

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

const PAGE: &str = r#"<html><body><script>
var r = {};
function trace(xhr, log) {
  ['loadstart', 'progress', 'load', 'loadend', 'error', 'readystatechange'].forEach(function (t) {
    xhr.addEventListener(t, function () { log.push(t + ':' + xhr.readyState); });
  });
}
// GET with the HttpOnly session cookie the document response set.
var log = [];
var a = new XMLHttpRequest();
trace(a, log);
a.open('GET', '/api/session', false);
a.send();
r.get = { status: a.status, readyState: a.readyState, text: a.responseText, events: log,
          contentType: a.getResponseHeader('content-type') };
// POST body round trip.
var b = new XMLHttpRequest();
b.open('POST', '/api/echo', false);
b.setRequestHeader('Content-Type', 'text/xml; charset=utf-8');
b.send('<soap>GetSearchExists</soap>');
r.post = { status: b.status, text: b.responseText };
// Set-Cookie on a sync response lands in the jar.
var c = new XMLHttpRequest();
c.open('GET', '/api/setcookie', false);
c.send();
r.cookieAfter = document.cookie;
// A redirect is followed inside the same send().
var d = new XMLHttpRequest();
d.open('GET', '/api/redirect', false);
d.send();
r.redirect = { status: d.status, text: d.responseText, url: d.responseURL };
// A dead endpoint throws NetworkError instead of firing events.
var e = new XMLHttpRequest();
var elog = [];
trace(e, elog);
e.open('GET', 'DEAD_URL', false);
try { e.send(); r.dead = { threw: false }; }
catch (err) { r.dead = { threw: true, name: err.name, status: e.status, readyState: e.readyState, events: elog }; }
// The two-argument form stays asynchronous.
var f = new XMLHttpRequest();
f.open('GET', '/api/session');
f.send();
r.asyncImmediate = { status: f.status, readyState: f.readyState };
window.__r = JSON.stringify(r);
</script></body></html>"#;

// Read headers, then as many body bytes as Content-Length says.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&data).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).to_string()
}

fn respond(stream: &mut std::net::TcpStream, status: &str, extra: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn serve(dead_url: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let dead_url = dead_url.clone();
            std::thread::spawn(move || {
                let req = read_request(&mut stream);
                let line = req.lines().next().unwrap_or("").to_string();
                let has_session = req.lines().any(|l| {
                    l.to_ascii_lowercase().starts_with("cookie:") && l.contains("ASP.NET_SessionId=abc123")
                });
                if line.starts_with("GET /api/session") {
                    respond(&mut stream, "200 OK", "", if has_session { "session:true" } else { "session:false" });
                } else if line.starts_with("POST /api/echo") {
                    let body = req.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                    respond(&mut stream, "200 OK", "", &format!("echo:{body}"));
                } else if line.starts_with("GET /api/setcookie") {
                    respond(&mut stream, "200 OK", "Set-Cookie: fromsync=1; Path=/\r\n", "ok");
                } else if line.starts_with("GET /api/redirect") {
                    respond(&mut stream, "302 Found", "Location: /api/final\r\n", "");
                } else if line.starts_with("GET /api/final") {
                    respond(&mut stream, "200 OK", "", "final");
                } else {
                    let page = PAGE.replace("DEAD_URL", &dead_url);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: ASP.NET_SessionId=abc123; Path=/; HttpOnly\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                        page.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
            });
        }
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session_id: &str) -> Value {
    let resp = dispatch(
        &CdpRequest { id, method: method.to_string(), params, session_id: Some(session_id.to_string()) },
        ctx,
    )
    .await;
    assert!(resp.error.is_none(), "CDP {method} failed: {:?}", resp.error);
    resp.result.unwrap_or_else(|| json!({}))
}

#[tokio::test(flavor = "current_thread")]
async fn sync_xhr_completes_inside_send() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    // A port nothing listens on: bind, read the port, close.
    let dead_url = {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/gone", probe.local_addr().unwrap())
    };
    let base = serve(dead_url);
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);

    cdp(&mut ctx, 1, "Page.navigate", json!({"url": base, "waitUntil": "load"}), session_id).await;
    let eval = cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": "window.__r", "returnByValue": true}),
        session_id,
    )
    .await;
    let raw = eval["result"]["value"].as_str().expect("page script must finish and set window.__r");
    let r: Value = serde_json::from_str(raw).unwrap();

    // The response is there on the line after send().
    assert_eq!(r["get"]["status"], 200, "{r}");
    assert_eq!(r["get"]["readyState"], 4, "{r}");
    assert_eq!(r["get"]["text"], "session:true", "the HttpOnly session cookie must be sent: {r}");
    assert_eq!(r["get"]["contentType"], "text/plain", "{r}");
    // Spec: open() fires readystatechange(1); a sync send() fires no
    // loadstart/progress and ends with readystatechange(4), load, loadend.
    assert_eq!(
        r["get"]["events"],
        json!(["readystatechange:1", "readystatechange:4", "load:4", "loadend:4"]),
        "{r}"
    );

    assert_eq!(r["post"]["status"], 200, "{r}");
    assert_eq!(r["post"]["text"], "echo:<soap>GetSearchExists</soap>", "{r}");

    assert!(
        r["cookieAfter"].as_str().unwrap_or("").contains("fromsync=1"),
        "Set-Cookie from a sync response must reach document.cookie: {r}"
    );
    assert!(
        !r["cookieAfter"].as_str().unwrap_or("").contains("ASP.NET_SessionId"),
        "the HttpOnly cookie must stay hidden from script: {r}"
    );

    assert_eq!(r["redirect"]["status"], 200, "{r}");
    assert_eq!(r["redirect"]["text"], "final", "{r}");
    assert!(r["redirect"]["url"].as_str().unwrap_or("").ends_with("/api/final"), "{r}");

    assert_eq!(r["dead"]["threw"], true, "{r}");
    assert_eq!(r["dead"]["name"], "NetworkError", "{r}");
    assert_eq!(r["dead"]["status"], 0, "{r}");
    assert_eq!(r["dead"]["readyState"], 4, "{r}");
    assert_eq!(
        r["dead"]["events"],
        json!(["readystatechange:1"]),
        "a failed sync send() fires no events (the one seen is from open()): {r}"
    );

    assert_eq!(r["asyncImmediate"]["status"], 0, "two-argument open() must stay async: {r}");

    // CDP clients see the sync request like any other XHR.
    let request_urls: Vec<String> = ctx
        .pending_events
        .iter()
        .filter(|e| e.method == "Network.requestWillBeSent")
        .filter_map(|e| e.params["request"]["url"].as_str().map(str::to_string))
        .collect();
    assert!(
        request_urls.iter().any(|u| u.ends_with("/api/echo")),
        "sync XHR must emit Network.requestWillBeSent; saw {request_urls:?}"
    );
}
