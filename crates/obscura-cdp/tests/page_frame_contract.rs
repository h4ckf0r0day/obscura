use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let redirect = if request.starts_with("GET /redirect-chain.html ") {
                    Some(("302 Found", "/redirect.html"))
                } else if request.starts_with("GET /redirect.html ") {
                    Some(("307 Temporary Redirect", "/child.html"))
                } else {
                    None
                };
                if let Some((status, location)) = redirect {
                    let response = format!("HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    let _ = socket.write_all(response.as_bytes()).await;
                    return;
                }
                let body = if request.starts_with("GET /child.html ") {
                    "<html><body>child</body></html>"
                } else {
                    r##"<!doctype html><html><body>
                        <a id="route" href="#next">next</a>
                        <iframe src="/child.html"></iframe>
                        <script>
                            route.addEventListener('click', event => {
                                event.preventDefault();
                                history.pushState({}, '', '/next');
                            });
                        </script>
                    </body></html>"##
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}/")
}

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session_id: Option<&str>,
) -> Value {
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.map(str::to_string),
        },
        ctx,
    )
    .await;
    assert!(
        response.error.is_none(),
        "CDP {method} failed: {:?}",
        response.error
    );
    response.result.unwrap_or_else(|| json!({}))
}

fn assert_frame_contract(frame: &Value) {
    for field in [
        "id",
        "loaderId",
        "url",
        "domainAndRegistry",
        "securityOrigin",
        "mimeType",
        "secureContextType",
        "crossOriginIsolatedContextType",
    ] {
        assert!(
            frame[field].is_string(),
            "{field} is missing or not a string: {frame}"
        );
    }
    assert!(
        matches!(
            frame["secureContextType"].as_str(),
            Some("Secure" | "SecureLocalhost" | "InsecureScheme" | "InsecureAncestor")
        ),
        "invalid secureContextType: {frame}"
    );
    assert!(
        matches!(
            frame["crossOriginIsolatedContextType"].as_str(),
            Some("Isolated" | "NotIsolated" | "NotIsolatedFeatureDisabled")
        ),
        "invalid crossOriginIsolatedContextType: {frame}"
    );
    assert!(
        frame["gatedAPIFeatures"].is_array(),
        "gatedAPIFeatures is missing: {frame}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn redirected_document_response_is_bound_to_the_committed_loader() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = format!("{page_id}-session");
    ctx.sessions.insert(session_id.clone(), page_id);
    cdp(&mut ctx, 900, "Page.enable", json!({}), Some(&session_id)).await;
    cdp(&mut ctx, 901, "Network.enable", json!({}), Some(&session_id)).await;
    let base = serve().await;
    for path in ["child.html", "redirect.html", "redirect-chain.html"] {
        ctx.pending_events.clear();
        let navigation = cdp(&mut ctx, 1, "Page.navigate",
            json!({"url": format!("{base}{path}"), "waitUntil": "load"}),
            Some(&session_id)).await;
        let loader = &navigation["loaderId"];
        let request = ctx.pending_events.iter().position(|event|
            event.method == "Network.requestWillBeSent" && event.params["type"] == "Document"
        ).expect("document request");
        let commit = ctx.pending_events.iter().position(|event|
            event.method == "Page.frameNavigated"
        ).expect("frame commit");
        assert!(request < commit, "document request must precede frame commit");
        assert_eq!(ctx.pending_events[request].params["requestId"], *loader);
        assert_eq!(ctx.pending_events[request].params["request"]["url"], format!("{base}child.html"));
        let response = ctx.pending_events.iter().find(|event|
            event.method == "Network.responseReceived" && event.params["type"] == "Document"
        ).expect("document response");
        assert_eq!(response.params["requestId"], *loader);
        assert_eq!(response.params["response"]["url"], format!("{base}child.html"));
        assert_eq!(response.params["response"]["status"], 200);
        let body = cdp(&mut ctx, 2, "Network.getResponseBody",
            json!({"requestId": loader}), Some(&session_id)).await;
        assert_eq!(body["body"], "<html><body>child</body></html>");
        assert_eq!(body["base64Encoded"], false);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_page_frame_path_uses_the_current_cdp_contract() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let mut ctx = CdpContext::new();

    let created = cdp(
        &mut ctx,
        1,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        None,
    )
    .await;
    let page_id = created["targetId"].as_str().unwrap().to_string();
    let attached = cdp(
        &mut ctx,
        2,
        "Target.attachToTarget",
        json!({"targetId": page_id, "flatten": true}),
        None,
    )
    .await;
    let session_id = attached["sessionId"].as_str().unwrap().to_string();
    let managed_session = format!("{page_id}-session");
    cdp(
        &mut ctx,
        30,
        "Page.enable",
        json!({}),
        Some(&session_id),
    )
    .await;
    cdp(
        &mut ctx,
        31,
        "Page.enable",
        json!({}),
        Some(&managed_session),
    )
    .await;

    let initial_tree = cdp(
        &mut ctx,
        3,
        "Page.getFrameTree",
        json!({}),
        Some(&session_id),
    )
    .await;
    let initial_frame = &initial_tree["frameTree"]["frame"];
    assert_frame_contract(initial_frame);
    assert_eq!(initial_frame["loaderId"], format!("loader-blank-{page_id}"));
    assert_eq!(initial_frame["secureContextType"], "Secure");

    let navigation_event_start = ctx.pending_events.len();
    let navigated = cdp(
        &mut ctx,
        4,
        "Page.navigate",
        json!({"url": serve().await, "waitUntil": "load"}),
        Some(&session_id),
    )
    .await;
    let loader_id = navigated["loaderId"].as_str().unwrap();
    let navigation_frame = &ctx.pending_events[navigation_event_start..]
        .iter()
        .find(|event| {
            event.method == "Page.frameNavigated" && event.params["frame"]["id"] == page_id
        })
        .expect("main-frame navigation event was not emitted")
        .params["frame"];
    assert_frame_contract(navigation_frame);
    assert_eq!(navigation_frame["loaderId"], loader_id);
    assert_eq!(navigation_frame["secureContextType"], "SecureLocalhost");

    cdp(
        &mut ctx,
        5,
        "Runtime.evaluate",
        json!({"expression": "document.title", "returnByValue": true}),
        Some(&session_id),
    )
    .await;
    let loaded_tree = cdp(
        &mut ctx,
        6,
        "Page.getFrameTree",
        json!({}),
        Some(&session_id),
    )
    .await;
    let root = &loaded_tree["frameTree"];
    assert_frame_contract(&root["frame"]);
    assert_eq!(root["frame"]["loaderId"], loader_id);
    let child = &root["childFrames"][0]["frame"];
    assert_frame_contract(child);
    assert_eq!(child["parentId"], root["frame"]["id"]);
    let child_event_frame = &ctx
        .pending_events
        .iter()
        .find(|event| {
            event.method == "Page.frameNavigated" && event.params["frame"]["id"] == child["id"]
        })
        .expect("child-frame navigation event was not emitted")
        .params["frame"];
    assert_frame_contract(child_event_frame);

    cdp(
        &mut ctx,
        7,
        "Runtime.evaluate",
        json!({
            "expression": "document.elementFromPoint = () => document.getElementById('route')"
        }),
        Some(&session_id),
    )
    .await;
    cdp(
        &mut ctx,
        8,
        "Input.dispatchMouseEvent",
        json!({"type": "mousePressed", "x": 0, "y": 0, "button": "left"}),
        Some(&session_id),
    )
    .await;
    let route_event_start = ctx.pending_events.len();
    cdp(
        &mut ctx,
        9,
        "Input.dispatchMouseEvent",
        json!({"type": "mouseReleased", "x": 0, "y": 0, "button": "left"}),
        Some(&session_id),
    )
    .await;
    let route_event = &ctx.pending_events[route_event_start..]
        .iter()
        .find(|event| event.method == "Page.navigatedWithinDocument")
        .expect("same-document navigation event was not emitted")
        .params;
    assert_eq!(route_event["frameId"], page_id);
    assert_eq!(route_event["navigationType"], "historyApi");
    assert!(route_event["url"].as_str().unwrap().ends_with("/next"));
    assert!(!ctx.pending_events[route_event_start..].iter().any(|event| {
        matches!(
            event.method.as_str(),
            "Runtime.executionContextsCleared" | "Page.loadEventFired"
        )
    }));
    let routed_tree = cdp(
        &mut ctx,
        10,
        "Page.getFrameTree",
        json!({}),
        Some(&session_id),
    )
    .await;
    assert_eq!(routed_tree["frameTree"]["frame"]["loaderId"], loader_id);
    for expected_session in [&session_id, &managed_session] {
        assert_eq!(
            ctx.pending_events[route_event_start..]
                .iter()
                .filter(|event| {
                    event.method == "Page.navigatedWithinDocument"
                        && event.params["frameId"] == page_id
                        && event.session_id.as_deref() == Some(expected_session.as_str())
                })
                .count(),
            1,
            "same-document navigation was not routed exactly once to {expected_session}",
        );
    }
}
