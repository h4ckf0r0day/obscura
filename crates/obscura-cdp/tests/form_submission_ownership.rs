use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = dispatch(&CdpRequest {
        id: 1, method: method.into(), params, session_id: Some("form-session".into()),
    }, ctx).await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    let result = response.result.expect("CDP result");
    assert!(result["exceptionDetails"].is_null(), "{method}: {result}");
    result
}

async fn submitted(html: &str, script: &str, click: bool, method: &str, encoded: &str) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let html = html.to_owned();
    let server = tokio::spawn(async move {
        let mut submitted = None;
        for index in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut buffer = [0u8; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0, "request ended before its headers");
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() < 1024 * 1024, "request too large");
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..header_end]).unwrap().to_owned();
            let request_line = headers.lines().next().unwrap().to_owned();
            let length = headers.lines().filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap()).unwrap_or(0);
            assert!(length < 1024 * 1024, "request body too large");
            while bytes.len() < header_end + length {
                let mut buffer = [0u8; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0, "request body truncated");
                bytes.extend_from_slice(&buffer[..count]);
            }
            let body = if index == 0 {
                assert_eq!(request_line, "GET /form HTTP/1.1");
                html.as_str()
            } else {
                let origin = format!("http://{address}");
                let headers = headers.to_ascii_lowercase();
                assert!(headers.contains("sec-fetch-site: same-origin\r\n"));
                assert!(headers.contains(&format!("referer: {origin}/form\r\n")));
                if request_line.starts_with("POST ") {
                    assert!(headers.contains(&format!("origin: {origin}\r\n")));
                } else {
                    assert!(!headers.contains("\r\norigin:"));
                }
                submitted = Some((request_line,
                    String::from_utf8(bytes[header_end..header_end + length].to_vec()).unwrap()));
                "<html><body>submitted</body></html>"
            };
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ).as_bytes()).await.unwrap();
        }
        submitted.unwrap()
    });
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    ctx.sessions.insert("form-session".into(), page_id.clone());
    cdp(&mut ctx, "Page.navigate", json!({"url":format!("http://{address}/form"),"waitUntil":"load"})).await;
    cdp(&mut ctx, "Runtime.evaluate", json!({"expression":script})).await;
    if click {
        for kind in ["mousePressed", "mouseReleased"] {
            cdp(&mut ctx, "Input.dispatchMouseEvent", json!({
                "type":kind,"x":0,"y":0,"button":"left","clickCount":1
            })).await;
        }
        // Direct dispatch omits the connection processor's post-ack navigation.
        let page = ctx.get_page_mut(&page_id).unwrap();
        assert!(page.has_pending_navigation(), "click did not queue submission");
        page.process_pending_navigation_outcome().await.unwrap();
    }
    let (line, body) = tokio::time::timeout(Duration::from_secs(10), server).await
        .expect("form did not submit").expect("fixture server failed");
    if method == "POST" {
        assert_eq!(line, "POST /submitted HTTP/1.1");
        assert_eq!(body, encoded);
    } else {
        let target = if encoded.is_empty() { "/submitted".into() } else { format!("/submitted?{encoded}") };
        assert_eq!(line, format!("GET {target} HTTP/1.1"));
        assert!(body.is_empty());
    }
    assert_eq!(ctx.get_page_mut(&page_id).unwrap().url.as_ref().unwrap().path(), "/submitted");
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_form_owners_include_external_and_exclude_reassigned_controls() {
    submitted("<form id=f action=/submitted><input name=inside value=1><input name=other value=x form=g></form><form id=g></form><input name=outside value=2 form=f>",
        "document.getElementById('f').submit()", false, "GET", "inside=1&outside=2").await;
}

#[tokio::test(flavor = "current_thread")]
async fn parser_associated_controls_send_exact_post_body() {
    submitted("<table><form id=f action=/submitted method=post><tr><td><input name=q value='caf\u{e9}'><button id=go name=go value=yes>Go</button></td></tr></form></table>",
        "document.getElementById('f').requestSubmit(document.getElementById('go'))", false,
        "POST", "q=caf%C3%A9&go=yes").await;
}

#[tokio::test(flavor = "current_thread")]
async fn multiple_select_submits_each_enabled_live_selection() {
    submitted("<form id=f action=/submitted><select name=m multiple><option value=a selected>A</option><option value=b selected>B</option><option value=c>C</option><option value=d selected disabled>D</option><optgroup disabled><option value=e selected>E</option></optgroup></select></form>",
        "document.getElementById('f').submit()", false, "GET", "m=a&m=b").await;
}

#[tokio::test(flavor = "current_thread")]
async fn button_submitter_adds_only_its_own_entry() {
    submitted("<form id=f action=/submitted><input name=a value=1><button id=go name=go value=yes>Go</button><button name=other value=no>Other</button></form>",
        "document.getElementById('f').requestSubmit(document.getElementById('go'))", false,
        "GET", "a=1&go=yes").await;
}

#[tokio::test(flavor = "current_thread")]
async fn single_select_uses_live_selection_and_preserves_explicit_deselection() {
    submitted("<form id=f action=/submitted><select id=s name=s><option value=a selected>A</option><option value=b>B</option></select><select name=first><option value=x>X</option></select><select id=none name=none><option value=z>Z</option></select></form>",
        "document.getElementById('s').selectedIndex=1;document.getElementById('none').selectedIndex=-1;document.getElementById('f').submit()",
        false, "GET", "s=b&first=x").await;
}

#[tokio::test(flavor = "current_thread")]
async fn select_defaults_respect_integer_size_and_disabled_options() {
    submitted("<form id=f action=/submitted><select name=s size='1.5'><option value=a disabled>A</option><option value=b>B</option></select><select name=list size=2><option value=x>X</option><option value=y>Y</option></select></form>",
        "document.getElementById('f').submit()", false, "GET", "s=b").await;
}

#[tokio::test(flavor = "current_thread")]
async fn external_submitter_click_uses_explicit_owner() {
    submitted("<form id=f action=/submitted><input name=a value=1></form><button id=go form=f name=go value=yes>Go</button>",
        "document.getElementById('go').click()", false, "GET", "a=1&go=yes").await;
}

#[tokio::test(flavor = "current_thread")]
async fn parser_submitter_cdp_click_uses_native_owner() {
    submitted("<table><form id=f action=/submitted><tr><td><input name=a value=1><button id=go name=go value=yes>Go</button></td></tr></form></table>",
        "document.elementFromPoint=()=>document.getElementById('go')", true,
        "GET", "a=1&go=yes").await;
}
