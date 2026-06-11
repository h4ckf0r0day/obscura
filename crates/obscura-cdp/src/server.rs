use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
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
        connection_id: u64,
        reply_tx: mpsc::UnboundedSender<String>,
    },
    ConnectionClosed {
        connection_id: u64,
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

            let processor_handle =
                tokio::task::spawn_local(cdp_processor(msg_rx, proxy, stealth, user_agent));

            let mut next_connection_id = 0u64;

            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        info!("New connection from {}", peer_addr);
                        next_connection_id = next_connection_id.wrapping_add(1);
                        let connection_id = next_connection_id;
                        let tx = msg_tx.clone();
                        tokio::task::spawn_local(async move {
                            if let Err(e) = handle_connection(stream, port, connection_id, tx).await
                            {
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
    let mut intercept_rx: Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>> =
        Some(irx);
    let mut intercepted_paused: HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    > = HashMap::new();
    let mut event_sinks: Vec<(u64, mpsc::UnboundedSender<String>)> = Vec::new();
    let mut active_connections: HashSet<u64> = HashSet::new();
    let mut page_tick = tokio::time::interval(tokio::time::Duration::from_millis(50));
    page_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let has_irx = intercept_rx.is_some();

        tokio::select! {
            _ = page_tick.tick() => {
                pump_loaded_pages(&mut ctx).await;
            }
            Some(intercepted) = async {
                if let Some(ref mut irx) = intercept_rx {
                    irx.recv().await
                } else {
                    std::future::pending().await
                }
            }, if has_irx => {
                emit_intercepted_request(&ctx, &mut event_sinks, intercepted, &mut intercepted_paused);
            }
            Some(msg) = rx.recv() => {
                match msg {
                    ServerMessage::NewConnection { connection_id, reply_tx } => {
                        active_connections.insert(connection_id);
                        event_sinks.push((connection_id, reply_tx.clone()));
                        log_cdp_state(&ctx, "client_connected", active_connections.len()).await;
                        let _ = reply_tx.send(
                            json!({"__init": true})
                                .to_string(),
                        );
                    }
                    ServerMessage::ConnectionClosed { connection_id } => {
                        active_connections.remove(&connection_id);
                        event_sinks.retain(|(sink_connection_id, _)| *sink_connection_id != connection_id);
                        log_cdp_state(&ctx, "client_disconnected", active_connections.len()).await;
                        if active_connections.is_empty() {
                            cleanup_after_all_clients_disconnected(
                                &mut ctx,
                                &mut intercepted_paused,
                                "last_client_disconnected",
                            ).await;
                            log_cdp_state(&ctx, "client_cleanup_finished", active_connections.len()).await;
                        }
                    }
                    ServerMessage::Cdp(cdp_msg) => {
                        let is_navigation = cdp_msg.text.contains("Page.navigate");
                        let has_interception = ctx.fetch_intercept.enabled;

                        if is_navigation && has_interception {
                            process_with_interception(
                                &cdp_msg.text, &mut ctx, &cdp_msg.reply_tx, &mut rx,
                                &mut intercept_rx, &mut intercepted_paused,
                                &mut event_sinks, &mut active_connections,
                            ).await;
                        } else {
                            let handled_fetch_resolution = cdp_msg.text.contains("Fetch.")
                                && handle_fetch_resolution(&cdp_msg.text, &cdp_msg.reply_tx, &mut intercepted_paused);
                            if !handled_fetch_resolution {
                                process_cdp_message(&cdp_msg.text, &mut ctx, &cdp_msg.reply_tx).await;
                            }
                        }
                    }
                }
            }
            else => break,
        }
    }
}

async fn pump_loaded_pages(ctx: &mut CdpContext) {
    for page in ctx.pages.iter_mut() {
        page.pump_event_loop_for(tokio::time::Duration::from_millis(25))
            .await;
    }
}

fn handle_fetch_resolution(
    text: &str,
    reply_tx: &mpsc::UnboundedSender<String>,
    intercepted_paused: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    >,
) -> bool {
    if let Ok(req) = serde_json::from_str::<CdpRequest>(text) {
        let method = req.method.as_str();
        if !matches!(
            method,
            "Fetch.continueRequest" | "Fetch.fulfillRequest" | "Fetch.failRequest"
        ) {
            return false;
        }
        let request_id = req
            .params
            .get("requestId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        tracing::info!(
            "INTERCEPTION resolution: {} for {}, paused_count={}",
            method,
            request_id,
            intercepted_paused.len()
        );

        if let Some(resolver) = intercepted_paused.remove(request_id) {
            tracing::info!("INTERCEPTION resolved: {}", request_id);
            let resolution = match method {
                "Fetch.continueRequest" => obscura_js::ops::InterceptResolution::Continue {
                    url: req
                        .params
                        .get("url")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    method: req
                        .params
                        .get("method")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    headers: parse_cdp_headers(req.params.get("headers")),
                    body: req
                        .params
                        .get("postData")
                        .and_then(|v| v.as_str())
                        .map(decode_cdp_post_data),
                },
                "Fetch.fulfillRequest" => {
                    let status = req
                        .params
                        .get("responseCode")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(200) as u16;
                    let raw_body = req
                        .params
                        .get("body")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let body = decode_base64(raw_body);
                    let headers = req
                        .params
                        .get("responseHeaders")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|h| {
                                    Some((
                                        h.get("name")?.as_str()?.to_string(),
                                        h.get("value")?.as_str()?.to_string(),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    obscura_js::ops::InterceptResolution::Fulfill {
                        status,
                        headers,
                        body,
                    }
                }
                "Fetch.failRequest" => {
                    let reason = req
                        .params
                        .get("errorReason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Failed")
                        .to_string();
                    obscura_js::ops::InterceptResolution::Fail { reason }
                }
                _ => return false,
            };
            let _ = resolver.send(resolution);
            let resp = crate::types::CdpResponse::success(req.id, json!({}), req.session_id);
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = reply_tx.send(json);
            }
            return true;
        }
    }

    false
}

fn parse_cdp_headers(value: Option<&Value>) -> Option<HashMap<String, String>> {
    let value = value?;
    if let Some(entries) = value.as_array() {
        let headers = entries
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("name")?.as_str()?.to_string(),
                    entry.get("value")?.as_str()?.to_string(),
                ))
            })
            .collect::<HashMap<_, _>>();
        return Some(headers);
    }

    value.as_object().map(|map| {
        map.iter()
            .filter_map(|(name, value)| {
                value
                    .as_str()
                    .map(|value| (name.clone(), value.to_string()))
            })
            .collect()
    })
}

fn emit_intercepted_request(
    ctx: &CdpContext,
    event_sinks: &mut Vec<(u64, mpsc::UnboundedSender<String>)>,
    intercepted: obscura_js::ops::InterceptedRequest,
    intercepted_paused: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    >,
) {
    let Some((session_id, frame_id, document_url)) =
        current_intercept_target(ctx, intercepted.page_id.as_deref(), &intercepted.page_url)
    else {
        let _ = intercepted
            .resolver
            .send(obscura_js::ops::InterceptResolution::Continue {
                url: None,
                method: None,
                headers: None,
                body: None,
            });
        return;
    };

    let request_id = intercepted.request_id.clone();
    let resource_type = intercepted.resource_type.clone();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let request = intercepted_request_payload(&intercepted);
    let response_rx = intercepted.response_rx;

    let network_event = json!({
        "method": "Network.requestWillBeSent",
        "params": {
            "requestId": request_id,
            "loaderId": "",
            "documentURL": document_url,
            "request": request.clone(),
            "timestamp": ts,
            "wallTime": ts,
            "initiator": {"type": "script"},
            "type": intercepted.resource_type,
            "frameId": frame_id,
        },
        "sessionId": session_id,
    });
    let network_sent = broadcast_json(event_sinks, network_event.to_string());
    let should_pause = intercepted.pause && ctx.fetch_intercept.should_pause_url(&intercepted.url);
    let paused_sent = if should_pause {
        let paused_event = json!({
            "method": "Fetch.requestPaused",
            "params": {
                "requestId": request_id,
                "request": request,
                "frameId": frame_id,
                "resourceType": intercepted.resource_type,
                "networkId": request_id,
                "responseErrorReason": null,
                "responseStatusCode": null,
                "responseHeaders": null,
            },
            "sessionId": session_id,
        });
        broadcast_json(event_sinks, paused_event.to_string())
    } else {
        false
    };
    let sent = network_sent || paused_sent;
    if sent {
        let response_sinks = event_sinks
            .iter()
            .map(|(_, sink)| sink.clone())
            .collect::<Vec<_>>();
        spawn_intercepted_response_events(
            response_sinks,
            ctx.network_response_bodies.clone(),
            Some(session_id),
            frame_id,
            String::new(),
            request_id.clone(),
            resource_type.clone(),
            response_rx,
        );
        if paused_sent {
            intercepted_paused.insert(request_id, intercepted.resolver);
        } else {
            let _ = intercepted
                .resolver
                .send(obscura_js::ops::InterceptResolution::Continue {
                    url: None,
                    method: None,
                    headers: None,
                    body: None,
                });
        }
    } else {
        let _ = intercepted
            .resolver
            .send(obscura_js::ops::InterceptResolution::Continue {
                url: None,
                method: None,
                headers: None,
                body: None,
            });
    }
}

fn spawn_intercepted_response_events(
    sinks: Vec<mpsc::UnboundedSender<String>>,
    response_bodies: std::sync::Arc<
        tokio::sync::Mutex<HashMap<String, dispatch::NetworkResponseBody>>,
    >,
    session_id: Option<String>,
    frame_id: String,
    loader_id: String,
    request_id: String,
    resource_type: String,
    response_rx: tokio::sync::oneshot::Receiver<obscura_js::ops::InterceptedResponse>,
) {
    tokio::task::spawn_local(async move {
        let Ok(response) = response_rx.await else {
            return;
        };

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let mime_type = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone())
            .unwrap_or_default();

        response_bodies.lock().await.insert(
            request_id.clone(),
            dispatch::NetworkResponseBody {
                body: response.body.clone(),
                base64_encoded: response.base64_encoded,
            },
        );

        let response_event = json!({
            "method": "Network.responseReceived",
            "params": {
                "requestId": request_id,
                "loaderId": loader_id,
                "timestamp": ts,
                "type": resource_type,
                "response": {
                    "url": response.url,
                    "status": response.status,
                    "statusText": "",
                    "headers": response.headers,
                    "mimeType": mime_type,
                    "encodedDataLength": response.encoded_data_length,
                },
                "frameId": frame_id,
            },
            "sessionId": session_id,
        });
        let loading_event = json!({
            "method": "Network.loadingFinished",
            "params": {
                "requestId": request_id,
                "timestamp": ts,
                "encodedDataLength": response.encoded_data_length,
            },
            "sessionId": session_id,
        });

        let response_message = response_event.to_string();
        let loading_message = loading_event.to_string();
        for sink in sinks {
            let _ = sink.send(response_message.clone());
            let _ = sink.send(loading_message.clone());
        }
    });
}

fn intercepted_request_payload(
    intercepted: &obscura_js::ops::InterceptedRequest,
) -> serde_json::Value {
    let mut request = json!({
        "url": intercepted.url,
        "method": intercepted.method,
        "headers": intercepted.headers,
        "initialPriority": "High",
        "referrerPolicy": "strict-origin-when-cross-origin",
    });
    if !intercepted.body.is_empty() {
        request["postData"] = json!(intercepted.body);
        request["hasPostData"] = json!(true);
        request["postDataEntries"] = json!([{
            "bytes": BASE64.encode(intercepted.body.as_bytes()),
        }]);
    }
    request
}

fn current_intercept_target(
    ctx: &CdpContext,
    source_page_id: Option<&str>,
    source_url: &str,
) -> Option<(String, String, String)> {
    if let Some(source_page_id) = source_page_id {
        if let Some((session_id, page_id)) = ctx
            .sessions
            .iter()
            .find(|(_, page_id)| page_id.as_str() == source_page_id)
        {
            let page = ctx.get_page(page_id)?;
            return Some((session_id.clone(), page.frame_id.clone(), page.url_string()));
        }
    }

    if !source_url.is_empty() && source_url != "about:blank" {
        if let Some((session_id, page)) = ctx.sessions.iter().find_map(|(session_id, page_id)| {
            let page = ctx.get_page(page_id)?;
            (page.url_string() == source_url).then_some((session_id, page))
        }) {
            return Some((session_id.clone(), page.frame_id.clone(), page.url_string()));
        }
    }

    ctx.sessions.iter().find_map(|(session_id, page_id)| {
        let page = ctx.get_page(page_id)?;
        Some((session_id.clone(), page.frame_id.clone(), page.url_string()))
    })
}

fn broadcast_json(
    event_sinks: &mut Vec<(u64, mpsc::UnboundedSender<String>)>,
    message: String,
) -> bool {
    let mut sent = false;
    event_sinks.retain(|(_, tx)| {
        let ok = tx.send(message.clone()).is_ok();
        sent |= ok;
        ok
    });
    sent
}

async fn maybe_pause_navigation_request(
    ctx: &mut CdpContext,
    rx: &mut mpsc::UnboundedReceiver<ServerMessage>,
    intercept_rx: &mut Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>>,
    intercepted_paused: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    >,
    event_sinks: &mut Vec<(u64, mpsc::UnboundedSender<String>)>,
    active_connections: &mut HashSet<u64>,
    reply_tx: &mpsc::UnboundedSender<String>,
    session_id: &Option<String>,
    frame_id: &str,
    loader_id: &str,
    url: &str,
) -> Option<obscura_js::ops::InterceptResolution> {
    if !ctx.fetch_intercept.enabled || !ctx.fetch_intercept.should_pause_url(url) {
        return None;
    }

    let request_id = ctx.fetch_intercept.next_request_id();
    let (resolution_tx, mut resolution_rx) = tokio::sync::oneshot::channel();
    intercepted_paused.insert(request_id.clone(), resolution_tx);

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let request = json!({
        "url": url,
        "method": "GET",
        "headers": {},
        "initialPriority": "High",
        "referrerPolicy": "strict-origin-when-cross-origin",
    });

    let network_event = json!({
        "method": "Network.requestWillBeSent",
        "params": {
            "requestId": request_id.clone(),
            "loaderId": loader_id,
            "documentURL": url,
            "request": request.clone(),
            "timestamp": ts,
            "wallTime": ts,
            "initiator": {"type": "other"},
            "type": "Document",
            "frameId": frame_id,
        },
        "sessionId": session_id,
    });
    let paused_event = json!({
        "method": "Fetch.requestPaused",
        "params": {
            "requestId": request_id.clone(),
            "request": request,
            "frameId": frame_id,
            "resourceType": "Document",
            "networkId": request_id.clone(),
            "responseErrorReason": null,
            "responseStatusCode": null,
            "responseHeaders": null,
        },
        "sessionId": session_id,
    });
    let _ = reply_tx.send(network_event.to_string());
    let _ = reply_tx.send(paused_event.to_string());

    let timeout = tokio::time::sleep(tokio::time::Duration::from_secs(30));
    tokio::pin!(timeout);

    loop {
        let has_irx = intercept_rx.is_some();

        tokio::select! {
            result = &mut resolution_rx => {
                return Some(result.unwrap_or(
                    obscura_js::ops::InterceptResolution::Continue {
                        url: None,
                        method: None,
                        headers: None,
                        body: None,
                    },
                ));
            }
            _ = &mut timeout => {
                intercepted_paused.remove(&request_id);
                tracing::warn!(
                    "Timed out waiting for Fetch resolution for navigation request {}",
                    request_id
                );
                return Some(obscura_js::ops::InterceptResolution::Continue {
                    url: None,
                    method: None,
                    headers: None,
                    body: None,
                });
            }
            Some(intercepted) = async {
                if let Some(ref mut irx) = intercept_rx {
                    irx.recv().await
                } else {
                    std::future::pending().await
                }
            }, if has_irx => {
                emit_intercepted_request(ctx, event_sinks, intercepted, intercepted_paused);
            }
            Some(msg) = rx.recv() => {
                match msg {
                    ServerMessage::NewConnection { connection_id, reply_tx: new_tx } => {
                        active_connections.insert(connection_id);
                        event_sinks.push((connection_id, new_tx.clone()));
                        log_cdp_state(ctx, "client_connected_during_navigation_pause", active_connections.len()).await;
                        let _ = new_tx.send(json!({"__init": true}).to_string());
                    }
                    ServerMessage::ConnectionClosed { connection_id } => {
                        active_connections.remove(&connection_id);
                        event_sinks.retain(|(sink_connection_id, _)| *sink_connection_id != connection_id);
                        if active_connections.is_empty() {
                            cleanup_after_all_clients_disconnected(
                                ctx,
                                intercepted_paused,
                                "last_client_disconnected_during_navigation_pause",
                            ).await;
                            return Some(obscura_js::ops::InterceptResolution::Fail {
                                reason: "Client disconnected".to_string(),
                            });
                        }
                    }
                    ServerMessage::Cdp(msg) => {
                        let handled_fetch_resolution = msg.text.contains("Fetch.")
                            && handle_fetch_resolution(&msg.text, &msg.reply_tx, intercepted_paused);
                        if !handled_fetch_resolution {
                            process_cdp_message(&msg.text, ctx, &msg.reply_tx).await;
                        }
                    }
                }
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
    intercepted_paused: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    >,
    event_sinks: &mut Vec<(u64, mpsc::UnboundedSender<String>)>,
    active_connections: &mut HashSet<u64>,
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
    let page_index = match page_index {
        Some(idx) => idx,
        None => {
            process_cdp_message(text, ctx, reply_tx).await;
            return;
        }
    };

    let url = req.params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let wait_until = req
        .params
        .get("waitUntil")
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

    let page_for_events = match ctx.pages.get(page_index) {
        Some(page) => page,
        None => {
            process_cdp_message(text, ctx, reply_tx).await;
            return;
        }
    };
    let session_for_events = req.session_id.clone();
    let frame_id = page_for_events.frame_id.clone();
    let loader_id = format!("loader-{}", uuid::Uuid::new_v4());
    let mut nav_url = url.to_string();
    let mut nav_method = "GET".to_string();
    let mut nav_body = String::new();

    if let Some(resolution) = maybe_pause_navigation_request(
        ctx,
        rx,
        intercept_rx,
        intercepted_paused,
        event_sinks,
        active_connections,
        reply_tx,
        &session_for_events,
        &frame_id,
        &loader_id,
        &nav_url,
    )
    .await
    {
        match resolution {
            obscura_js::ops::InterceptResolution::Continue {
                url, method, body, ..
            } => {
                if let Some(url) = url {
                    nav_url = url;
                }
                if let Some(method) = method {
                    nav_method = method;
                }
                if let Some(body) = body {
                    nav_body = body;
                }
            }
            obscura_js::ops::InterceptResolution::Fail { reason } => {
                let response = crate::types::CdpResponse::error(
                    req.id,
                    -32000,
                    format!("Navigation request aborted: {}", reason),
                    req.session_id.clone(),
                );
                if let Ok(json) = serde_json::to_string(&response) {
                    let _ = reply_tx.send(json);
                }
                return;
            }
            obscura_js::ops::InterceptResolution::Fulfill { .. } => {
                let response = crate::types::CdpResponse::error(
                    req.id,
                    -32000,
                    "Fetch.fulfillRequest for main document navigation is not supported yet"
                        .to_string(),
                    req.session_id.clone(),
                );
                if let Ok(json) = serde_json::to_string(&response) {
                    let _ = reply_tx.send(json);
                }
                return;
            }
        }
    }

    let mut page = ctx.pages.remove(page_index);

    if let Some(tx) = &ctx.intercept_tx {
        page.set_intercept_tx(tx.clone(), ctx.fetch_intercept.patterns.clone());
    }

    let (nav_done_tx, mut nav_done_rx) =
        mpsc::channel::<(obscura_browser::Page, Result<(), String>)>(1);
    let url_owned = nav_url.clone();
    let method_owned = nav_method.clone();
    let body_owned = nav_body.clone();

    tokio::task::spawn_local(async move {
        let result = if method_owned == "POST" && !body_owned.is_empty() {
            page.navigate_with_wait_post(&url_owned, wait_until, &method_owned, &body_owned)
                .await
        } else {
            page.navigate_with_wait(&url_owned, wait_until).await
        }
        .map_err(|e| e.to_string());
        for source in &preload_scripts {
            if let Err(e) = page.execute_preload_script(source) {
                tracing::debug!("Preload script error: {}", e);
            }
        }
        let _ = nav_done_tx.send((page, result)).await;
    });

    let mut navigate_result: Result<(), String> = Ok(());
    let mut page_back: Option<obscura_browser::Page> = None;

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
                let request_id = intercepted.request_id.clone();
                let resource_type = intercepted.resource_type.clone();
                let request_payload = intercepted_request_payload(&intercepted);
                let response_rx = intercepted.response_rx;
                let rws_event = json!({
                    "method": "Network.requestWillBeSent",
                    "params": {
                        "requestId": request_id,
                        "loaderId": "",
                        "documentURL": "",
                        "request": request_payload.clone(),
                        "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64(),
                        "wallTime": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64(),
                        "initiator": {"type": "script"},
                        "type": resource_type,
                        "frameId": frame_id,
                    },
            "sessionId": session_for_events,
                });
                let _ = reply_tx.send(rws_event.to_string());

                spawn_intercepted_response_events(
                    vec![reply_tx.clone()],
                    ctx.network_response_bodies.clone(),
                    session_for_events.clone(),
                    frame_id.clone(),
                    loader_id.clone(),
                    request_id.clone(),
                    resource_type.clone(),
                    response_rx,
                );
                if intercepted.pause && ctx.fetch_intercept.should_pause_url(&intercepted.url) {
                    let event_json = json!({
                        "method": "Fetch.requestPaused",
                        "params": {
                            "requestId": request_id,
                            "request": request_payload,
                            "frameId": frame_id,
                            "resourceType": resource_type,
                            "networkId": request_id,
                            "responseErrorReason": null,
                            "responseStatusCode": null,
                            "responseHeaders": null,
                        },
                        "sessionId": session_for_events,
                    });
                    let event_str = event_json.to_string();
                    tracing::info!("INTERCEPTION event JSON: {}", &event_str[..event_str.len().min(300)]);
                    let _ = reply_tx.send(event_str);
                    intercepted_paused.insert(request_id, intercepted.resolver);
                } else {
                    let _ = intercepted
                        .resolver
                        .send(obscura_js::ops::InterceptResolution::Continue {
                            url: None,
                            method: None,
                            headers: None,
                            body: None,
                        });
                }
                yield_after_navigation_intercept().await;
            }
            Some(msg) = rx.recv() => {
                tracing::info!("INTERCEPTION select: received CDP message during navigation");
                match msg {
                    ServerMessage::NewConnection { connection_id, reply_tx: new_tx } => {
                        active_connections.insert(connection_id);
                        event_sinks.push((connection_id, new_tx.clone()));
                        let pid = ctx.create_page();
                        let sid = format!("{}-session", pid);
                        ctx.sessions.insert(sid.clone(), pid.clone());
                        let _ = new_tx.send(json!({"__init": true, "pageId": pid, "sessionId": sid}).to_string());
                    }
                    ServerMessage::ConnectionClosed { connection_id } => {
                        active_connections.remove(&connection_id);
                        event_sinks.retain(|(sink_connection_id, _)| *sink_connection_id != connection_id);
                        if active_connections.is_empty() {
                            cleanup_after_all_clients_disconnected(
                                ctx,
                                intercepted_paused,
                                "last_client_disconnected_during_navigation",
                            ).await;
                        }
                    }
                    ServerMessage::Cdp(msg) => {
                        let handled_fetch_resolution = msg.text.contains("Fetch.")
                            && handle_fetch_resolution(&msg.text, &msg.reply_tx, intercepted_paused);
                        if !handled_fetch_resolution {
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

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let es = session_for_events;

    let mut navigation_events = vec![
        crate::types::CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "init", "timestamp": ts}),
            session_id: es.clone(),
        },
        crate::types::CdpEvent {
            method: "Runtime.executionContextsCleared".into(),
            params: json!({}),
            session_id: es.clone(),
        },
        crate::types::CdpEvent {
            method: "Page.frameNavigated".into(),
            params: json!({"frame": {"id": frame_id, "loaderId": loader_id, "url": page_url, "domainAndRegistry": "", "securityOrigin": page_url, "mimeType": "text/html", "adFrameStatus": {"adFrameType": "none"}}, "type": "Navigation"}),
            session_id: es.clone(),
        },
        crate::types::CdpEvent {
            method: "Runtime.executionContextCreated".into(),
            params: json!({"context": {"id": 2, "origin": page_url, "name": "", "uniqueId": format!("ctx-nav-{}", page_id_for_events), "auxData": {"isDefault": true, "type": "default", "frameId": frame_id}}}),
            session_id: es.clone(),
        },
    ];
    let world_names: Vec<String> = if ctx.isolated_worlds.is_empty() {
        vec!["__puppeteer_utility_world__24.40.0".to_string()]
    } else {
        ctx.isolated_worlds.clone()
    };
    for (idx, world_name) in world_names.iter().enumerate() {
        let world_ctx_id = 100 + idx as u32;
        navigation_events.push(crate::types::CdpEvent {
            method: "Runtime.executionContextCreated".into(),
            params: json!({"context": {"id": world_ctx_id, "origin": page_url, "name": world_name, "uniqueId": format!("ctx-isolated-nav-{}-{}", page_id_for_events, idx), "auxData": {"isDefault": false, "type": "isolated", "frameId": frame_id}}}),
            session_id: es.clone(),
        });
    }
    navigation_events.push(
        crate::types::CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "commit", "timestamp": ts}),
            session_id: es.clone(),
        },
    );
    for event in navigation_events {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = reply_tx.send(json);
        }
    }

    for net_event in &network_events {
        for event in [
            crate::types::CdpEvent {
                method: "Network.requestWillBeSent".into(),
                params: json!({"requestId": net_event.request_id, "loaderId": loader_id, "documentURL": page_url, "request": {"url": net_event.url, "method": net_event.method, "headers": net_event.headers}, "timestamp": net_event.timestamp, "wallTime": net_event.timestamp, "initiator": {"type": "other"}, "type": net_event.resource_type, "frameId": frame_id}),
                session_id: es.clone(),
            },
            crate::types::CdpEvent {
                method: "Network.responseReceived".into(),
                params: json!({"requestId": net_event.request_id, "loaderId": loader_id, "timestamp": net_event.timestamp, "type": net_event.resource_type, "response": {"url": net_event.url, "status": net_event.status, "statusText": "", "headers": &*net_event.response_headers, "mimeType": ""}, "frameId": frame_id}),
                session_id: es.clone(),
            },
            crate::types::CdpEvent {
                method: "Network.loadingFinished".into(),
                params: json!({"requestId": net_event.request_id, "timestamp": net_event.timestamp, "encodedDataLength": net_event.body_size}),
                session_id: es.clone(),
            },
        ] {
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = reply_tx.send(json);
            }
        }
    }

    for event in [
        crate::types::CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "DOMContentLoaded", "timestamp": ts}),
            session_id: es.clone(),
        },
        crate::types::CdpEvent {
            method: "Page.domContentEventFired".into(),
            params: json!({"timestamp": ts}),
            session_id: es.clone(),
        },
        crate::types::CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "load", "timestamp": ts}),
            session_id: es.clone(),
        },
        crate::types::CdpEvent {
            method: "Page.loadEventFired".into(),
            params: json!({"timestamp": ts}),
            session_id: es.clone(),
        },
    ] {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = reply_tx.send(json);
        }
    }
    if reached_network_idle {
        let idle_event = crate::types::CdpEvent {
            method: "Page.lifecycleEvent".into(),
            params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "networkIdle", "timestamp": ts}),
            session_id: es.clone(),
        };
        if let Ok(json) = serde_json::to_string(&idle_event) {
            let _ = reply_tx.send(json);
        }
    }
    let stop_event = crate::types::CdpEvent {
        method: "Page.frameStoppedLoading".into(),
        params: json!({"frameId": frame_id}),
        session_id: es,
    };
    if let Ok(json) = serde_json::to_string(&stop_event) {
        let _ = reply_tx.send(json);
    }
}

async fn yield_after_navigation_intercept() {
    tokio::task::yield_now().await;
}

async fn cleanup_after_all_clients_disconnected(
    ctx: &mut CdpContext,
    intercepted_paused: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    >,
    reason: &str,
) {
    let pages_before = ctx.pages.len();
    let sessions_before = ctx.sessions.len();
    let pending_events_before = ctx.pending_events.len();
    let response_bodies_before = ctx.network_response_bodies.lock().await.len();
    let rss_before_kib = current_rss_kib();

    for mut page in ctx.pages.drain(..) {
        page.suspend_js();
    }
    ctx.sessions.clear();
    ctx.pending_events.clear();
    ctx.preload_scripts.clear();
    ctx.isolated_worlds.clear();
    ctx.default_context.cookie_jar.clear();
    ctx.fetch_intercept.enabled = false;
    ctx.fetch_intercept.patterns.clear();
    for (_, paused) in ctx.fetch_intercept.paused.drain() {
        let _ = paused
            .resolver
            .send(crate::domains::fetch::FetchResolution::Continue {
                url: None,
                method: None,
                headers: None,
                post_data: None,
            });
    }
    for (_, resolver) in intercepted_paused.drain() {
        let _ = resolver.send(obscura_js::ops::InterceptResolution::Continue {
            url: None,
            method: None,
            headers: None,
            body: None,
        });
    }
    ctx.network_response_bodies.lock().await.clear();

    tracing::info!(
        target: "obscura::cdp_state",
        reason,
        pages_before,
        sessions_before,
        pending_events_before,
        response_bodies_before,
        rss_before_kib = ?rss_before_kib,
        rss_after_kib = ?current_rss_kib(),
        "Cleaned CDP client state after disconnect"
    );
}

async fn log_cdp_state(ctx: &CdpContext, event: &str, active_cdp_clients: usize) {
    let response_body_count = ctx.network_response_bodies.lock().await.len();
    tracing::info!(
        target: "obscura::cdp_state",
        event,
        active_cdp_clients,
        active_pages = ctx.pages.len(),
        active_sessions = ctx.sessions.len(),
        pending_events = ctx.pending_events.len(),
        preload_scripts = ctx.preload_scripts.len(),
        isolated_worlds = ctx.isolated_worlds.len(),
        fetch_intercept_enabled = ctx.fetch_intercept.enabled,
        fetch_paused = ctx.fetch_intercept.paused.len(),
        response_body_count,
        rss_kib = ?current_rss_kib(),
        "CDP state snapshot"
    );
}

fn current_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
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

    tracing::debug!(
        "CDP: {} (id={}, s={:?})",
        req.method,
        req.id,
        req.session_id
    );

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
        tracing::info!(
            "JS-triggered nav: {} {} (body: {} bytes)",
            nav_method,
            nav_url,
            nav_body.len()
        );
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
        if chunk.len() > 2 {
            out.push((b[1] << 4) | (b[2] >> 2));
        }
        if chunk.len() > 3 {
            out.push((b[2] << 6) | b[3]);
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn decode_cdp_post_data(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let looks_base64 = input.len() % 4 == 0
        && input
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'));
    if !looks_base64 {
        return input.to_string();
    }

    let decoded = decode_base64(input);
    let printable = decoded
        .bytes()
        .all(|b| matches!(b, b'\n' | b'\r' | b'\t') || (0x20..=0x7e).contains(&b));
    if printable {
        decoded
    } else {
        input.to_string()
    }
}

fn fast_path_response(text: &str) -> Option<String> {
    let req: CdpRequest = serde_json::from_str(text).ok()?;

    let result = match req.method.as_str() {
        "Network.enable"
        | "Network.setCacheDisabled"
        | "Network.setRequestInterception"
        | "Page.enable"
        | "Page.setLifecycleEventsEnabled"
        | "Page.setInterceptFileChooserDialog"
        | "Runtime.runIfWaitingForDebugger"
        | "Runtime.discardConsoleEntries"
        | "Performance.enable"
        | "Log.enable"
        | "Security.enable"
        | "CSS.enable"
        | "Accessibility.enable"
        | "ServiceWorker.enable"
        | "Inspector.enable"
        | "Debugger.enable"
        | "Profiler.enable"
        | "HeapProfiler.enable"
        | "Overlay.enable"
        | "Storage.enable"
        | "Target.setAutoAttach" => Some(json!({})),
        "Browser.getVersion" => Some(json!({
            "protocolVersion": "1.3",
            "product": "Obscura/0.1.0",
            "revision": "0",
            "userAgent": obscura_net::DEFAULT_USER_AGENT,
            "jsVersion": "V8",
        })),
        "Browser.setDownloadBehavior" | "Browser.getWindowBounds" => Some(json!({})),
        _ => None,
    };

    if let Some(value) = result {
        let resp = crate::types::CdpResponse::success(req.id, value, req.session_id);
        serde_json::to_string(&resp).ok()
    } else {
        None
    }
}

fn check_pending_navigation(
    ctx: &CdpContext,
    session_id: &Option<String>,
) -> Option<(String, String, String)> {
    let page_id = session_id.as_ref().and_then(|sid| ctx.sessions.get(sid))?;
    let page = ctx.pages.iter().find(|p| &p.id == page_id)?;
    page.take_pending_navigation()
}

async fn handle_connection(
    stream: TcpStream,
    port: u16,
    connection_id: u64,
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
        } else if line.contains("/json/list")
            || line.contains("/json\r\n")
            || line.contains("/json HTTP")
        {
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
        connection_id,
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
    let _ = msg_tx.send(ServerMessage::ConnectionClosed { connection_id });
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
            "User-Agent": obscura_net::DEFAULT_USER_AGENT,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, Instant};
    use url::Url;

    #[test]
    fn current_intercept_target_prefers_source_page_id() {
        let mut ctx = CdpContext::new();
        let first_page = ctx.create_page();
        let second_page = ctx.create_page();
        ctx.sessions
            .insert("first-session".to_string(), first_page.clone());
        ctx.sessions
            .insert("second-session".to_string(), second_page.clone());

        ctx.get_page_mut(&first_page).unwrap().url =
            Some(Url::parse("https://www.instagram.com/first/").unwrap());
        ctx.get_page_mut(&second_page).unwrap().url =
            Some(Url::parse("https://www.instagram.com/second/").unwrap());

        let (session_id, frame_id, document_url) = current_intercept_target(
            &ctx,
            Some(&second_page),
            "https://www.instagram.com/second/",
        )
        .expect("source page should resolve to its session");

        assert_eq!(session_id, "second-session");
        assert_eq!(frame_id, second_page);
        assert_eq!(document_url, "https://www.instagram.com/second/");
    }

    #[test]
    fn current_intercept_target_can_match_source_url() {
        let mut ctx = CdpContext::new();
        let first_page = ctx.create_page();
        let second_page = ctx.create_page();
        ctx.sessions
            .insert("first-session".to_string(), first_page.clone());
        ctx.sessions
            .insert("second-session".to_string(), second_page.clone());

        ctx.get_page_mut(&first_page).unwrap().url =
            Some(Url::parse("https://www.instagram.com/first/").unwrap());
        ctx.get_page_mut(&second_page).unwrap().url =
            Some(Url::parse("https://www.instagram.com/second/").unwrap());

        let (session_id, frame_id, _) =
            current_intercept_target(&ctx, None, "https://www.instagram.com/second/")
                .expect("source URL should resolve to its session");

        assert_eq!(session_id, "second-session");
        assert_eq!(frame_id, second_page);
    }

    #[test]
    fn handle_fetch_resolution_applies_continue_request_overrides() {
        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel();
        let (resolution_tx, mut resolution_rx) = tokio::sync::oneshot::channel();
        let mut paused = HashMap::new();
        paused.insert("req-1".to_string(), resolution_tx);

        let handled = handle_fetch_resolution(
            &json!({
                "id": 9,
                "method": "Fetch.continueRequest",
                "params": {
                    "requestId": "req-1",
                    "url": "https://www.instagram.com/graphql/query/?cursor=injected",
                    "method": "POST",
                    "headers": [
                        {"name": "content-type", "value": "application/json"},
                        {"name": "x-injected", "value": "1"}
                    ],
                    "postData": "eyJjdXJzb3IiOiJpbmplY3RlZCJ9"
                },
                "sessionId": "session-1"
            })
            .to_string(),
            &reply_tx,
            &mut paused,
        );

        assert!(handled);
        assert!(paused.is_empty());
        match resolution_rx.try_recv().expect("resolution should be sent") {
            obscura_js::ops::InterceptResolution::Continue {
                url,
                method,
                headers,
                body,
            } => {
                assert_eq!(
                    url.as_deref(),
                    Some("https://www.instagram.com/graphql/query/?cursor=injected")
                );
                assert_eq!(method.as_deref(), Some("POST"));
                let headers = headers.expect("headers should be forwarded");
                assert_eq!(
                    headers.get("content-type").map(String::as_str),
                    Some("application/json")
                );
                assert_eq!(headers.get("x-injected").map(String::as_str), Some("1"));
                assert_eq!(body.as_deref(), Some("{\"cursor\":\"injected\"}"));
            }
            other => panic!("expected Continue resolution, got {other:?}"),
        }

        let response = reply_rx.try_recv().expect("CDP response should be sent");
        assert!(response.contains("\"id\":9"));
    }

    #[tokio::test]
    async fn navigation_pause_emits_fetch_request_paused_and_waits_for_continue() {
        let mut ctx = CdpContext::new();
        ctx.fetch_intercept.enabled = true;
        ctx.fetch_intercept.patterns.push("*".to_string());

        let (server_tx, mut server_rx) = mpsc::unbounded_channel::<ServerMessage>();
        let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();
        let mut intercept_rx = None;
        let mut intercepted_paused = HashMap::new();
        let mut event_sinks = Vec::new();
        let mut active_connections = HashSet::new();
        let session_id = Some("session-1".to_string());

        let pause = maybe_pause_navigation_request(
            &mut ctx,
            &mut server_rx,
            &mut intercept_rx,
            &mut intercepted_paused,
            &mut event_sinks,
            &mut active_connections,
            &reply_tx,
            &session_id,
            "page-1",
            "loader-1",
            "file:///tmp/form.html",
        );

        let reply_for_continue = reply_tx.clone();
        let driver = async {
            let network_event = reply_rx
                .recv()
                .await
                .expect("Network.requestWillBeSent should be emitted");
            assert!(network_event.contains("\"method\":\"Network.requestWillBeSent\""));

            let paused_event = reply_rx
                .recv()
                .await
                .expect("Fetch.requestPaused should be emitted");
            assert!(paused_event.contains("\"method\":\"Fetch.requestPaused\""));
            assert!(paused_event.contains("\"resourceType\":\"Document\""));

            server_tx
                .send(ServerMessage::Cdp(CdpMessage {
                    text: json!({
                        "id": 42,
                        "method": "Fetch.continueRequest",
                        "params": {"requestId": "interception-1"},
                        "sessionId": "session-1"
                    })
                    .to_string(),
                    reply_tx: reply_for_continue,
                }))
                .expect("continue request should be queued");

            let continue_response = reply_rx
                .recv()
                .await
                .expect("Fetch.continueRequest response should be emitted");
            assert!(continue_response.contains("\"id\":42"));
        };

        let (resolution, _) = tokio::join!(pause, driver);

        assert!(matches!(
            resolution,
            Some(obscura_js::ops::InterceptResolution::Continue { .. })
        ));
    }

    #[tokio::test]
    async fn navigation_intercept_yield_does_not_add_fixed_latency() {
        let started = Instant::now();

        yield_after_navigation_intercept().await;

        assert!(
            started.elapsed() < Duration::from_millis(10),
            "navigation interception should yield without fixed wall-clock delay"
        );
    }

    #[tokio::test]
    async fn disconnect_cleanup_clears_client_state() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        ctx.sessions.insert("session-1".to_string(), page_id);
        ctx.pending_events.push(crate::types::CdpEvent::new(
            "Target.targetCreated",
            json!({}),
        ));
        ctx.preload_scripts
            .push(("script-1".to_string(), "window.__x = 1".to_string()));
        ctx.isolated_worlds.push("utility".to_string());
        ctx.fetch_intercept.enabled = true;
        ctx.fetch_intercept.patterns.push("*".to_string());
        ctx.network_response_bodies.lock().await.insert(
            "request-1".to_string(),
            dispatch::NetworkResponseBody {
                body: "large response".to_string(),
                base64_encoded: false,
            },
        );

        let mut intercepted_paused = HashMap::new();
        cleanup_after_all_clients_disconnected(&mut ctx, &mut intercepted_paused, "test").await;

        assert!(ctx.pages.is_empty());
        assert!(ctx.sessions.is_empty());
        assert!(ctx.pending_events.is_empty());
        assert!(ctx.preload_scripts.is_empty());
        assert!(ctx.isolated_worlds.is_empty());
        assert!(!ctx.fetch_intercept.enabled);
        assert!(ctx.fetch_intercept.patterns.is_empty());
        assert!(ctx.network_response_bodies.lock().await.is_empty());
    }

    #[test]
    fn decode_cdp_post_data_preserves_non_base64_bodies() {
        assert_eq!(
            decode_cdp_post_data("{\"cursor\":\"already-plain\"}"),
            "{\"cursor\":\"already-plain\"}"
        );
    }

    #[test]
    fn intercepted_request_payload_includes_chromium_post_data_entries() {
        let (resolver, _resolution_rx) = tokio::sync::oneshot::channel();
        let (_response_tx, response_rx) = tokio::sync::oneshot::channel();
        let request = obscura_js::ops::InterceptedRequest {
            page_id: Some("page-1".to_string()),
            page_url: "https://www.instagram.com/nba/".to_string(),
            request_id: "request-1".to_string(),
            url: "https://www.instagram.com/graphql/query".to_string(),
            method: "POST".to_string(),
            headers: HashMap::new(),
            body: "variables=%7B%22after%22%3A%22cursor-a%22%7D".to_string(),
            resource_type: "Fetch".to_string(),
            pause: true,
            resolver,
            response_rx,
        };

        let payload = intercepted_request_payload(&request);

        assert_eq!(payload["hasPostData"], json!(true));
        assert_eq!(
            payload["postData"],
            json!("variables=%7B%22after%22%3A%22cursor-a%22%7D")
        );
        assert_eq!(
            payload["postDataEntries"],
            json!([{
                "bytes": BASE64.encode("variables=%7B%22after%22%3A%22cursor-a%22%7D")
            }])
        );
    }
}
