use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::types::CdpEvent;

pub async fn handle(method: &str, params: &Value, ctx: &mut CdpContext) -> Result<Value, String> {
    match method {
        "setDiscoverTargets" => {
            ctx.pending_events.push(CdpEvent::new(
                "Target.targetCreated",
                json!({
                    "targetInfo": {
                        "targetId": "browser",
                        "type": "browser",
                        "title": "",
                        "url": "",
                        "attached": true,
                        "browserContextId": "",
                    }
                }),
            ));
            for page in &ctx.pages {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetCreated",
                    json!({
                        "targetInfo": {
                            "targetId": page.id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": false,
                            "browserContextId": page.context.id,
                        }
                    }),
                ));
            }
            Ok(json!({}))
        }
        "getTargets" => {
            let targets: Vec<Value> = ctx
                .pages
                .iter()
                .map(|page| {
                    json!({
                        "targetId": page.id,
                        "type": "page",
                        "title": page.title,
                        "url": page.url_string(),
                        "attached": true,
                        "browserContextId": page.context.id,
                    })
                })
                .collect();
            Ok(json!({ "targetInfos": targets }))
        }
        "createTarget" => {
            let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("about:blank");
            let page_id = ctx.create_page();
            let session_id = format!("{}-session", page_id);

            if let Some(page) = ctx.get_page_mut(&page_id) {
                if url == "about:blank" || url.is_empty() {
                    page.navigate_blank();
                } else {
                    let _ = page.navigate(url).await;
                }
            }

            ctx.sessions.insert(session_id.clone(), page_id.clone());

            if let Some(page) = ctx.get_page(&page_id) {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetCreated",
                    json!({
                        "targetInfo": {
                            "targetId": page_id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": false,
                            "browserContextId": page.context.id,
                        }
                    }),
                ));
            }

            if let Some(page) = ctx.get_page(&page_id) {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.attachedToTarget",
                    json!({
                        "sessionId": session_id,
                        "targetInfo": {
                            "targetId": page_id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": true,
                            "browserContextId": page.context.id,
                        },
                        "waitingForDebugger": false,
                    }),
                ));
            }

            if let Some(page) = ctx.get_page(&page_id) {
                let frame_id = page.frame_id.clone();
                let page_url = page.url_string();
                let loader_id = format!("loader-{}", uuid::Uuid::new_v4());
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();

                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.lifecycleEvent",
                    json!({
                        "frameId": frame_id,
                        "loaderId": loader_id,
                        "name": "init",
                        "timestamp": ts
                    }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Runtime.executionContextsCleared",
                    json!({}),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.frameNavigated",
                    json!({
                        "frame": {
                            "id": frame_id,
                            "loaderId": loader_id,
                            "url": page_url,
                            "domainAndRegistry": "",
                            "securityOrigin": page_url,
                            "mimeType": "text/html",
                            "adFrameStatus": { "adFrameType": "none" }
                        },
                        "type": "Navigation"
                    }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Runtime.executionContextCreated",
                    json!({
                        "context": {
                            "id": 2,
                            "origin": page_url,
                            "name": "",
                            "uniqueId": format!("ctx-nav-{}", page_id),
                            "auxData": {
                                "isDefault": true,
                                "type": "default",
                                "frameId": frame_id
                            }
                        }
                    }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.lifecycleEvent",
                    json!({
                        "frameId": frame_id,
                        "loaderId": loader_id,
                        "name": "commit",
                        "timestamp": ts
                    }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.lifecycleEvent",
                    json!({
                        "frameId": frame_id,
                        "loaderId": loader_id,
                        "name": "DOMContentLoaded",
                        "timestamp": ts
                    }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.domContentEventFired",
                    json!({ "timestamp": ts }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.lifecycleEvent",
                    json!({
                        "frameId": frame_id,
                        "loaderId": loader_id,
                        "name": "load",
                        "timestamp": ts
                    }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.loadEventFired",
                    json!({ "timestamp": ts }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.lifecycleEvent",
                    json!({
                        "frameId": frame_id,
                        "loaderId": loader_id,
                        "name": "networkIdle",
                        "timestamp": ts
                    }),
                    session_id.clone(),
                ));
                ctx.pending_events.push(CdpEvent::with_session(
                    "Page.frameStoppedLoading",
                    json!({ "frameId": frame_id }),
                    session_id.clone(),
                ));
            }

            Ok(json!({ "targetId": page_id }))
        }
        "attachToTarget" => {
            let target_id = params.get("targetId").and_then(|v| v.as_str())
                .ok_or("targetId required")?;
            let session_id = format!("{}-session", target_id);
            ctx.sessions.insert(session_id.clone(), target_id.to_string());

            if let Some(page) = ctx.get_page(target_id) {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.attachedToTarget",
                    json!({
                        "sessionId": session_id,
                        "targetInfo": {
                            "targetId": target_id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": true,
                            "browserContextId": page.context.id,
                        },
                        "waitingForDebugger": false,
                    }),
                ));
            }

            Ok(json!({ "sessionId": session_id }))
        }
        "closeTarget" => {
            let target_id = params.get("targetId").and_then(|v| v.as_str())
                .ok_or("targetId required")?;
            let session_id = format!("{}-session", target_id);

            ctx.pending_events.push(CdpEvent::new(
                "Target.detachedFromTarget",
                json!({
                    "sessionId": session_id,
                    "targetId": target_id,
                }),
            ));
            ctx.pending_events.push(CdpEvent::new(
                "Target.targetDestroyed",
                json!({ "targetId": target_id }),
            ));

            ctx.remove_page(target_id);
            Ok(json!({ "success": true }))
        }
        "setAutoAttach" => Ok(json!({})),
        "getBrowserContexts" => {
            Ok(json!({ "browserContextIds": [ctx.default_context.id] }))
        }
        "createBrowserContext" => {
            ctx.default_context.cookie_jar.clear();
            Ok(json!({ "browserContextId": ctx.default_context.id }))
        }
        "disposeBrowserContext" => {
            ctx.default_context.cookie_jar.clear();
            Ok(json!({}))
        }
        "getTargetInfo" => {
            let target_id = params.get("targetId").and_then(|v| v.as_str());
            match target_id {
                Some(id) => {
                    let page = ctx.get_page(id).ok_or("Target not found")?;
                    Ok(json!({
                        "targetInfo": {
                            "targetId": id,
                            "type": "page",
                            "title": page.title,
                            "url": page.url_string(),
                            "attached": true,
                            "browserContextId": page.context.id,
                        }
                    }))
                }
                None => {
                    Ok(json!({
                        "targetInfo": {
                            "targetId": "browser",
                            "type": "browser",
                            "title": "",
                            "url": "",
                            "attached": true,
                        }
                    }))
                }
            }
        }
        _ => Err(format!("Unknown Target method: {}", method)),
    }
}
