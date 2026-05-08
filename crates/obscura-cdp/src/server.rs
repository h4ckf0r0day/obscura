use std::collections::HashMap;
use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::dispatch::{self, CdpContext};
use crate::types::CdpRequest;

struct CdpMessage {
    text: String,
    reply_tx: mpsc::UnboundedSender<String>,
}

enum ServerMessage {
    Cdp(CdpMessage),
    NewConnection {
        reply_tx: mpsc::UnboundedSender<String>,
    },
}

pub async fn start(port: u16) -> anyhow::Result<()> {
    start_with_options(port, None, false).await
}

pub async fn start_with_options(
    port: u16,
    proxy: Option<String>,
    stealth: bool,
) -> anyhow::Result<()> {
    start_with_full_options(port, proxy, stealth, None).await
}

pub async fn start_with_full_options(
    port: u16,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(&addr).await?;

    info!("Obscura CDP server listening on ws://127.0.0.1:{}", port);
    info!(
        "DevTools endpoint: ws://127.0.0.1:{}/devtools/browser",
        port
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ServerMessage>();

            let _processor_handle = tokio::task::spawn_local(cdp_processor(msg_rx, proxy, stealth, user_agent));

            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        info!("New connection from {}", peer_addr);
                        let tx = msg_tx.clone();
                        tokio::task::spawn_local(async move {
                            if let Err(e) = handle_connection(stream, port, tx).await {
                                if !format!("{}", e).contains("close") {
                                    error!("Connection error from {}: {}", peer_addr, e);
                                }
                            }
                        });
                    }
                    Err(e) => error!("Accept error: {}", e),
                }
            }
        })
        .await
}

async fn cdp_processor(
    mut rx: mpsc::UnboundedReceiver<ServerMessage>,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
) {
    let mut ctx = CdpContext::new_with_full_options(proxy, stealth, user_agent);
    let (itx, irx) = mpsc::unbounded_channel::<obscura_js::ops::InterceptedRequest>();
    ctx.intercept_tx = Some(itx);
    let mut intercept_rx: Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>> = Some(irx);
    let mut intercepted_paused: HashMap<String, tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>> = HashMap::new();

    while let Some(msg) = rx.recv().await {
        match msg {
            ServerMessage::NewConnection { reply_tx } => {
                let _ = reply_tx.send(
                    json!({"__init": true})
                        .to_string(),
                );
            }
            ServerMessage::Cdp(cdp_msg) => {
                let is_navigation = cdp_msg.text.contains("Page.navigate");
                let has_interception = ctx.fetch_intercept.enabled;

                if is_navigation && has_interception {
                    process_with_interception(
                        &cdp_msg.text, &mut ctx, &cdp_msg.reply_tx, &mut rx,
                        &mut intercept_rx, &mut intercepted_paused,
                    ).await;
                } else {
                    if cdp_msg.text.contains("Fetch.") {
                        handle_fetch_resolution(&cdp_msg.text, &mut ctx, &cdp_msg.reply_tx, &mut intercepted_paused);
                    }
                    process_cdp_message(&cdp_msg.text, &mut ctx, &cdp_msg.reply_tx).await;
                }
            }
        }

    }
}

fn handle_fetch_resolution(
    text: &str,
    _ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
    intercepted_paused: &mut HashMap<String, tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>>,
) {
    if let Ok(req) = serde_json::from_str::<CdpRequest>(text) {
        let method = req.method.as_str();
        let request_id = req.params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
        tracing::info!("INTERCEPTION resolution: {} for {}, paused_count={}", method, request_id, intercepted_paused.len());

        if let Some(resolver) = intercepted_paused.remove(request_id) {
            tracing::info!("INTERCEPTION resolved: {}", request_id);
            let resolution = match method {
                "Fetch.continueRequest" => obscura_js::ops::InterceptResolution::Continue {
                    url: None, method: None, headers: None, body: None,
                },
                "Fetch.fulfillRequest" => {
                    let status = req.params.get("responseCode").and_then(|v| v.as_u64()).unwrap_or(200) as u16;
                    let raw_body = req.params.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let body = decode_base64(raw_body);
                    let headers = req.params.get("responseHeaders")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|h| {
                            Some((h.get("name")?.as_str()?.to_string(), h.get("value")?.as_str()?.to_string()))
                        }).collect())
                        .unwrap_or_default();
                    obscura_js::ops::InterceptResolution::Fulfill { status, headers, body }
                }
                "Fetch.failRequest" => {
                    let reason = req.params.get("errorReason").and_then(|v| v.as_str()).unwrap_or("Failed").to_string();
                    obscura_js::ops::InterceptResolution::Fail { reason }
                }
                _ => return,
            };
            let _ = resolver.send(resolution);
            let resp = crate::types::CdpResponse::success(req.id, json!({}), req.session_id);
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = reply_tx.send(json);
            }
        }
    }
}

async fn process_with_interception(
    text: &str,
    ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
    rx: &mut mpsc::UnboundedReceiver<ServerMessage>,
    intercept_rx: &mut Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>>,
    intercepted_paused: &mut HashMap<String, tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>>,
) {
    let req: CdpRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            warn!("Invalid CDP: {}", e);
            return;
        }
    };

    tracing::info!("INTERCEPTION navigate: {} (id={})", req.method, req.id);

    let session_id = &req.session_id;
    let page_id = session_id
        .as_ref()
        .and_then(|sid| ctx.sessions.get(sid))
        .cloned();

    let page_id = match page_id {
        Some(id) => id,
        None => {
            process_cdp_message(text, ctx, reply_tx).await;
            return;
        }
    };

    let page_index = ctx.pages.iter().position(|p| p.id == page_id);
    let mut page = match page_index {
        Some(idx) => ctx.pages.remove(idx),
        None => {
            process_cdp_message(text, ctx, reply_tx).await;
            return;
        }
    };

    let url = req.params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let wait_until = req.params.get("waitUntil")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(obscura_browser::WaitUntil::from_str(s))
            } else if let Some(arr) = v.as_array() {
                arr.iter()
                    .filter_map(|item| item.as_str())
                    .map(obscura_browser::WaitUntil::from_str)
                    .max_by_key(|w| match w {
                        obscura_browser::WaitUntil::DomContentLoaded => 0,
                        obscura_browser::WaitUntil::Load => 1,
                        obscura_browser::WaitUntil::NetworkIdle2 => 2,
                        obscura_browser::WaitUntil::NetworkIdle0 => 3,
                    })
            } else {
                None
            }
        })
        .unwrap_or(obscura_browser::WaitUntil::Load);

    let preload_scripts: Vec<String> = ctx.preload_scripts.iter().map(|(_, s)| s.clone()).collect();

    if let Some(tx) = &ctx.intercept_tx {
        page.set_intercept_tx(tx.clone());
    }

    let session_for_events = req.session_id.clone();
    let frame_id = page.frame_id.clone();
    let loader_id = format!("loader-{}", uuid::Uuid::new_v4());

    let (nav_done_tx, mut nav_done_rx) = mpsc::channel::<(obscura_browser::Page, Result<(), String>)>(1);
    let url_owned = url.to_string();

    tokio::task::spawn_local(async move {
        let result = page.navigate_with_wait(&url_owned, wait_until).await.map_err(|e| e.to_string());
        for source in &preload_scripts {
            if let Err(e) = page.execute_preload_script(source) {
                tracing::debug!("Preload script error: {}", e);
            }
        }
        let _ = nav_done_tx.send((page, result)).await;
    });

    let navigate_result: Result<(), String>;
    let page_back: Option<obscura_browser::Page>;

    loop {
        let has_irx = intercept_rx.is_some();

        tokio::select! {
            Some((returned_page, result)) = nav_done_rx.recv() => {
                page_back = Some(returned_page);
                navigate_result = result;
                break;
            }
            Some(intercepted) = async {
                if let Some(ref mut irx) = intercept_rx {
                    irx.recv().await
                } else {
                    std::future::pending().await
                }
            }, if has_irx => {
                tracing::info!("INTERCEPTION: requestPaused for {} {} (sending to client)", intercepted.method, intercepted.url);
                let rws_event = json!({
                    "method": "Network.requestWillBeSent",
                    "params": {
                        "requestId": intercepted.request_id,
                        "loaderId": "",
                        "documentURL": "",
                        "request": {
                            "url": intercepted.url,
                            "method": intercepted.method,
                            "headers": intercepted.headers,
                            "initialPriority": "High",
                            "referrerPolicy": "strict-origin-when-cross-origin",
                        },
                        "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64(),
                        "wallTime": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64(),
                        "initiator": {"type": "script"},
                        "type": intercepted.resource_type,
                        "frameId": frame_id,
                    },
                    "sessionId": session_for_events,
                });
                let _ = reply_tx.send(rws_event.to_string());

                let event_json = json!({
                    "method": "Fetch.requestPaused",
                    "params": {
                        "requestId": intercepted.request_id,
                        "request": {
                            "url": intercepted.url,
                            "method": intercepted.method,
                            "headers": intercepted.headers,
                            "initialPriority": "High",
                            "referrerPolicy": "strict-origin-when-cross-origin",
                        },
                        "frameId": frame_id,
                        "resourceType": intercepted.resource_type,
                        "networkId": intercepted.request_id,
                        "responseErrorReason": null,
                        "responseStatusCode": null,
                        "responseHeaders": null,
                    },
                    "sessionId": session_for_events,
                });
                let event_str = event_json.to_string();
                tracing::info!("INTERCEPTION event JSON: {}", &event_str[..event_str.len().min(300)]);
                let _ = reply_tx.send(event_str);
                intercepted_paused.insert(intercepted.request_id.clone(), intercepted.resolver);
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
            Some(msg) = rx.recv() => {
                tracing::info!("INTERCEPTION select: received CDP message during navigation");
                match msg {
                    ServerMessage::NewConnection { reply_tx: new_tx } => {
                        let pid = ctx.create_page();
                        let sid = format!("{}-session", pid);
                        ctx.sessions.insert(sid.clone(), pid.clone());
                        let _ = new_tx.send(json!({"__init": true, "pageId": pid, "sessionId": sid}).to_string());
                    }
                    ServerMessage::Cdp(msg) => {
                        if msg.text.contains("Fetch.") {
                            handle_fetch_resolution(&msg.text, ctx, &msg.reply_tx, intercepted_paused);
                        } else {
                            process_cdp_message(&msg.text, ctx, &msg.reply_tx).await;
                        }
                    }
                }
            }
        }
    }

    let mut page = page_back.expect("navigation task should return the page");

    let network_events: Vec<_> = page.network_events.drain(..).collect();
    let page_url = page.url_string();
    let page_id_for_events = page.id.clone();
    let reached_network_idle = page.lifecycle.is_network_idle();

    ctx.pages.push(page);

    let response = match navigate_result {
        Ok(()) => crate::types::CdpResponse::success(
            req.id,
            json!({"frameId": frame_id, "loaderId": loader_id}),
            req.session_id.clone(),
        ),
        Err(e) => crate::types::CdpResponse::error(req.id, -32000, e, req.session_id.clone()),
    };

    if let Ok(json) = serde_json::to_string(&response) {
        let _ = reply_tx.send(json);
    }

    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64();
    let es = session_for_events;

    for event in [
        crate::types::CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "init", "timestamp": ts}), session_id: es.clone() },
        crate::types::CdpEvent { method: "Runtime.executionContextsCleared".into(), params: json!({}), session_id: es.clone() },
        crate::types::CdpEvent { method: "Page.frameNavigated".into(), params: json!({"frame": {"id": frame_id, "loaderId": loader_id, "url": page_url, "domainAndRegistry": "", "securityOrigin": page_url, "mimeType": "text/html", "adFrameStatus": {"adFrameType": "none"}}, "type": "Navigation"}), session_id: es.clone() },
        crate::types::CdpEvent { method: "Runtime.executionContextCreated".into(), params: json!({"context": {"id": 2, "origin": page_url, "name": "", "uniqueId": format!("ctx-nav-{}", page_id_for_events), "auxData": {"isDefault": true, "type": "default", "frameId": frame_id}}}), session_id: es.clone() },
        crate::types::CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "commit", "timestamp": ts}), session_id: es.clone() },
    ] {
        if let Ok(json) = serde_json::to_string(&event) { let _ = reply_tx.send(json); }
    }

    for net_event in &network_events {
        for event in [
            crate::types::CdpEvent { method: "Network.requestWillBeSent".into(), params: json!({"requestId": net_event.request_id, "loaderId": loader_id, "documentURL": page_url, "request": {"url": net_event.url, "method": net_event.method, "headers": net_event.headers}, "timestamp": net_event.timestamp, "wallTime": net_event.timestamp, "initiator": {"type": "other"}, "type": net_event.resource_type, "frameId": frame_id}), session_id: es.clone() },
            crate::types::CdpEvent { method: "Network.responseReceived".into(), params: json!({"requestId": net_event.request_id, "loaderId": loader_id, "timestamp": net_event.timestamp, "type": net_event.resource_type, "response": {"url": net_event.url, "status": net_event.status, "statusText": "", "headers": &*net_event.response_headers, "mimeType": ""}, "frameId": frame_id}), session_id: es.clone() },
            crate::types::CdpEvent { method: "Network.loadingFinished".into(), params: json!({"requestId": net_event.request_id, "timestamp": net_event.timestamp, "encodedDataLength": net_event.body_size}), session_id: es.clone() },
        ] {
            if let Ok(json) = serde_json::to_string(&event) { let _ = reply_tx.send(json); }
        }
    }

    for event in [
        crate::types::CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "DOMContentLoaded", "timestamp": ts}), session_id: es.clone() },
        crate::types::CdpEvent { method: "Page.domContentEventFired".into(), params: json!({"timestamp": ts}), session_id: es.clone() },
        crate::types::CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "load", "timestamp": ts}), session_id: es.clone() },
        crate::types::CdpEvent { method: "Page.loadEventFired".into(), params: json!({"timestamp": ts}), session_id: es.clone() },
    ] {
        if let Ok(json) = serde_json::to_string(&event) { let _ = reply_tx.send(json); }
    }
    if reached_network_idle {
        let idle_event = crate::types::CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "networkIdle", "timestamp": ts}), session_id: es.clone() };
        if let Ok(json) = serde_json::to_string(&idle_event) { let _ = reply_tx.send(json); }
    }
    let stop_event = crate::types::CdpEvent { method: "Page.frameStoppedLoading".into(), params: json!({"frameId": frame_id}), session_id: es };
    if let Ok(json) = serde_json::to_string(&stop_event) { let _ = reply_tx.send(json); }
}

async fn process_cdp_message(
    text: &str,
    ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
) {
    let req: CdpRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            warn!("Invalid CDP: {}: {}", e, &text[..text.len().min(200)]);
            return;
        }
    };

    tracing::debug!("CDP: {} (id={}, s={:?})", req.method, req.id, req.session_id);

    let response = dispatch::dispatch(&req, ctx).await;

    // Chromium CDP semantics: events emitted as a side-effect of a command
    // (e.g. Target.targetCreated + Target.attachedToTarget from
    // Target.createTarget) MUST arrive BEFORE the command's response.
    // Playwright awaits the response and immediately reads state wired up
    // by those events; if the response lands first, accessing
    // Target._page errors with "Cannot read properties of undefined".
    for event in ctx.pending_events.drain(..) {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = reply_tx.send(json);
        }
    }

    if let Ok(json) = serde_json::to_string(&response) {
        let _ = reply_tx.send(json);
    }

    if let Some((nav_url, nav_method, nav_body)) = check_pending_navigation(ctx, &req.session_id) {
        tracing::info!("JS-triggered nav: {} {} (body: {} bytes)", nav_method, nav_url, nav_body.len());
        let nav_req = CdpRequest {
            id: 0,
            method: "Page.navigate".to_string(),
            params: json!({"url": nav_url, "__method": nav_method, "__body": nav_body}),
            session_id: req.session_id.clone(),
        };
        let _ = dispatch::dispatch(&nav_req, ctx).await;
        for event in ctx.pending_events.drain(..) {
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = reply_tx.send(json);
            }
        }
    }
}

fn decode_base64(input: &str) -> String {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter_map(val).collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
            chunk.get(3).copied().unwrap_or(0),
        ];
        out.push((b[0] << 2) | (b[1] >> 4));
        if chunk.len() > 2 { out.push((b[1] << 4) | (b[2] >> 2)); }
        if chunk.len() > 3 { out.push((b[2] << 6) | b[3]); }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn fast_path_response(text: &str) -> Option<String> {
    let req: CdpRequest = serde_json::from_str(text).ok()?;

    let result = match req.method.as_str() {
        "Network.enable" | "Network.setCacheDisabled" | "Network.setRequestInterception" |
        "Page.enable" | "Page.setLifecycleEventsEnabled" | "Page.setInterceptFileChooserDialog" |
        "Runtime.runIfWaitingForDebugger" | "Runtime.discardConsoleEntries" |
        "Performance.enable" | "Log.enable" | "Security.enable" |
        "Emulation.setDeviceMetricsOverride" | "Emulation.setTouchEmulationEnabled" |
        "CSS.enable" | "Accessibility.enable" | "ServiceWorker.enable" |
        "Inspector.enable" | "Debugger.enable" | "Profiler.enable" |
        "HeapProfiler.enable" | "Overlay.enable" | "Storage.enable" |
        "Target.setAutoAttach" => {
            Some(json!({}))
        }
        "Browser.getVersion" => {
            Some(json!({
                "protocolVersion": "1.3",
                "product": "Obscura/0.1.0",
                "revision": "0",
                "userAgent": "Obscura/0.1.0",
                "jsVersion": "V8",
            }))
        }
        "Browser.setDownloadBehavior" | "Browser.getWindowBounds" => {
            Some(json!({}))
        }
        _ => None,
    };

    if let Some(value) = result {
        let resp = crate::types::CdpResponse::success(req.id, value, req.session_id);
        serde_json::to_string(&resp).ok()
    } else {
        None
    }
}

fn check_pending_navigation(ctx: &CdpContext, session_id: &Option<String>) -> Option<(String, String, String)> {
    let page_id = session_id
        .as_ref()
        .and_then(|sid| ctx.sessions.get(sid))?;
    let page = ctx.pages.iter().find(|p| &p.id == page_id)?;
    page.take_pending_navigation()
}

async fn handle_connection(
    stream: TcpStream,
    port: u16,
    msg_tx: mpsc::UnboundedSender<ServerMessage>,
) -> anyhow::Result<()> {
    let mut buf = [0u8; 4];
    stream.peek(&mut buf).await?;

    if &buf == b"GET " {
        let mut peek_buf = [0u8; 1024];
        let n = stream.peek(&mut peek_buf).await?;
        let line = String::from_utf8_lossy(&peek_buf[..n]);

        if line.contains("/json/version") {
            return handle_http_json(stream, port, "version").await;
        } else if line.contains("/json/list") || line.contains("/json\r\n") || line.contains("/json HTTP") {
            return handle_http_json(stream, port, "list").await;
        } else if line.contains("/json/protocol") {
            return handle_http_json(stream, port, "protocol").await;
        }
    }

    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    info!("WebSocket connected");
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();

    let _ = msg_tx.send(ServerMessage::NewConnection {
        reply_tx: reply_tx.clone(),
    });
    if let Some(init_msg) = reply_rx.recv().await {
        tracing::debug!("Connection init: {}", &init_msg[..init_msg.len().min(100)]);
    }

    let send_task = tokio::task::spawn_local(async move {
        while let Some(msg) = reply_rx.recv().await {
            if msg.contains("\"__init\"") {
                continue;
            }
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("WS read error: {}", e);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                if text.contains("\"Browser.close\"") {
                    if let Ok(req) = serde_json::from_str::<CdpRequest>(&text) {
                        let resp = crate::types::CdpResponse::success(req.id, json!({}), None);
                        if let Ok(json) = serde_json::to_string(&resp) {
                            let _ = reply_tx.send(json);
                        }
                    }
                    break;
                }

                if let Some(resp) = fast_path_response(&text) {
                    let _ = reply_tx.send(resp);
                } else {
                    let _ = msg_tx.send(ServerMessage::Cdp(CdpMessage {
                        text: text.to_string(),
                        reply_tx: reply_tx.clone(),
                    }));
                }
            }
            Message::Close(_) => {
                info!("WS closed by client");
                break;
            }
            _ => {}
        }
    }

    send_task.abort();
    Ok(())
}

async fn handle_http_json(stream: TcpStream, port: u16, endpoint: &str) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = stream;
    let mut buf = vec![0u8; 4096];
    let _ = stream.read(&mut buf).await?;

    let body = match endpoint {
        "version" => serde_json::to_string_pretty(&json!({
            "Browser": "Obscura/0.1.0",
            "Protocol-Version": "1.3",
            "User-Agent": "Obscura/0.1.0 (Headless Browser)",
            "V8-Version": "N/A",
            "WebKit-Version": "N/A",
            "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/browser", port),
        }))?,
        "list" => serde_json::to_string_pretty(&json!([{
            "description": "",
            "devtoolsFrontendUrl": "",
            "id": "page-1",
            "title": "",
            "type": "page",
            "url": "about:blank",
            "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/page/page-1", port),
        }]))?,
        "protocol" => {
            serde_json::to_string_pretty(&json!({ "version": { "major": "1", "minor": "3" } }))?
        }
        _ => "{}".to_string(),
    };

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body,
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}