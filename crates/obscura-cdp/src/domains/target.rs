use std::collections::HashSet;

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
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("about:blank");
            let page_id = ctx.create_page();
            let session_id = ctx.create_session(&page_id);

            if let Some(page) = ctx.get_page_mut(&page_id) {
                if url == "about:blank" || url.is_empty() {
                    page.navigate_blank();
                } else {
                    let _ = page.navigate(url).await;
                }
            }

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

            Ok(json!({ "targetId": page_id }))
        }
        "attachToBrowserTarget" => {
            // Playwright calls this on connect to obtain a session for the
            // implicit "browser" target. Returning Unknown method aborts
            // the connect handshake before any user code runs.
            let session_id = "browser-session".to_string();
            ctx.sessions
                .insert(session_id.clone(), "browser".to_string());

            ctx.pending_events.push(CdpEvent::new(
                "Target.attachedToTarget",
                json!({
                    "sessionId": session_id,
                    "targetInfo": {
                        "targetId": "browser",
                        "type": "browser",
                        "title": "",
                        "url": "",
                        "attached": true,
                        "browserContextId": "",
                    },
                    "waitingForDebugger": false,
                }),
            ));

            Ok(json!({ "sessionId": session_id }))
        }
        "attachToTarget" => {
            let target_id = params
                .get("targetId")
                .and_then(|v| v.as_str())
                .ok_or("targetId required")?;
            let session_id = ctx.create_session(target_id);

            Ok(json!({ "sessionId": session_id }))
        }
        "closeTarget" => {
            let target_id = params
                .get("targetId")
                .and_then(|v| v.as_str())
                .ok_or("targetId required")?;
            let session_ids = ctx
                .sessions
                .iter()
                .filter_map(|(session_id, attached_target)| {
                    (attached_target == target_id).then_some(session_id.clone())
                })
                .collect::<Vec<_>>();
            for session_id in session_ids {
                ctx.sessions.remove(&session_id);
                ctx.pending_events.push(CdpEvent::new(
                    "Target.detachedFromTarget",
                    json!({
                        "sessionId": session_id,
                        "targetId": target_id,
                    }),
                ));
            }
            ctx.pending_events.push(CdpEvent::new(
                "Target.targetDestroyed",
                json!({ "targetId": target_id }),
            ));

            ctx.remove_page(target_id);
            Ok(json!({ "success": true }))
        }
        "setAutoAttach" => Ok(json!({})),
        "getBrowserContexts" => Ok(json!({ "browserContextIds": [ctx.default_context.id] })),
        "createBrowserContext" => {
            ctx.default_context.cookie_jar.clear();
            Ok(json!({ "browserContextId": ctx.default_context.id }))
        }
        "disposeBrowserContext" => {
            let browser_context_id = params
                .get("browserContextId")
                .and_then(|v| v.as_str())
                .unwrap_or(&ctx.default_context.id);
            let mut removed_page_ids = HashSet::new();
            let pages_before = ctx.pages.len();

            let mut retained_pages = Vec::with_capacity(ctx.pages.len());
            for mut page in ctx.pages.drain(..) {
                if page.context.id == browser_context_id {
                    removed_page_ids.insert(page.id.clone());
                    page.suspend_js();
                } else {
                    retained_pages.push(page);
                }
            }
            ctx.pages = retained_pages;

            let detached_sessions = ctx
                .sessions
                .iter()
                .filter_map(|(session_id, page_id)| {
                    removed_page_ids
                        .contains(page_id)
                        .then_some((session_id.clone(), page_id.clone()))
                })
                .collect::<Vec<_>>();
            for (session_id, page_id) in &detached_sessions {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.detachedFromTarget",
                    json!({
                        "sessionId": session_id,
                        "targetId": page_id,
                    }),
                ));
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetDestroyed",
                    json!({ "targetId": page_id }),
                ));
            }
            ctx.sessions
                .retain(|_, page_id| !removed_page_ids.contains(page_id));
            ctx.default_context.cookie_jar.clear();
            ctx.network_response_bodies.lock().await.clear();
            ctx.fetch_intercept.enabled = false;
            ctx.fetch_intercept.patterns.clear();
            for (_, paused) in ctx.fetch_intercept.paused.drain() {
                let _ = paused
                    .resolver
                    .send(super::fetch::FetchResolution::Continue {
                        url: None,
                        method: None,
                        headers: None,
                        post_data: None,
                    });
            }
            tracing::info!(
                target: "obscura::cdp_state",
                browser_context_id,
                pages_before,
                pages_after = ctx.pages.len(),
                response_bodies_cleared = true,
                "Disposed browser context"
            );
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
                None => Ok(json!({
                    "targetInfo": {
                        "targetId": "browser",
                        "type": "browser",
                        "title": "",
                        "url": "",
                        "attached": true,
                    }
                })),
            }
        }
        _ => Err(format!("Unknown Target method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn attach_to_browser_target_returns_session_id() {
        let mut ctx = CdpContext::new();
        let result = handle("attachToBrowserTarget", &json!({}), &mut ctx)
            .await
            .expect("attachToBrowserTarget should succeed");

        assert_eq!(result["sessionId"], "browser-session");
        assert_eq!(
            ctx.sessions.get("browser-session").map(String::as_str),
            Some("browser")
        );

        // Playwright/Puppeteer expect a Target.attachedToTarget event before
        // they finish wiring up the session — without it the connect promise
        // hangs.
        let attached_evt = ctx
            .pending_events
            .iter()
            .find(|e| e.method == "Target.attachedToTarget")
            .expect("attachedToTarget event must be emitted");
        assert_eq!(attached_evt.params["sessionId"], "browser-session");
        assert_eq!(attached_evt.params["targetInfo"]["type"], "browser");
    }

    #[tokio::test]
    async fn unknown_target_method_still_errors() {
        let mut ctx = CdpContext::new();
        let err = handle("notARealMethod", &json!({}), &mut ctx)
            .await
            .expect_err("unknown methods must surface as errors");
        assert!(err.contains("Unknown Target method"));
    }

    #[tokio::test]
    async fn repeated_target_attachments_receive_distinct_session_ids() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let first = handle("attachToTarget", &json!({"targetId": page_id}), &mut ctx)
            .await
            .expect("first attachment should succeed");
        let second = handle("attachToTarget", &json!({"targetId": page_id}), &mut ctx)
            .await
            .expect("second attachment should succeed");

        assert_ne!(first["sessionId"], second["sessionId"]);
        assert_eq!(ctx.sessions.len(), 2);
    }

    #[tokio::test]
    async fn dispose_browser_context_removes_pages_sessions_and_cached_bodies() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        ctx.sessions
            .insert("browser-session".to_string(), "browser".to_string());
        ctx.sessions
            .insert("page-session".to_string(), page_id.clone());
        ctx.network_response_bodies.lock().await.insert(
            "request-1".to_string(),
            crate::dispatch::NetworkResponseBody {
                body: b"large response".to_vec(),
            },
        );

        handle(
            "disposeBrowserContext",
            &json!({"browserContextId": ctx.default_context.id}),
            &mut ctx,
        )
        .await
        .expect("disposeBrowserContext should succeed");

        assert!(ctx.pages.is_empty());
        assert_eq!(
            ctx.sessions.get("browser-session").map(String::as_str),
            Some("browser")
        );
        assert!(!ctx.sessions.contains_key("page-session"));
        assert!(ctx.network_response_bodies.lock().await.is_empty());
        assert!(ctx
            .pending_events
            .iter()
            .any(|event| event.method == "Target.targetDestroyed"
                && event.params["targetId"] == page_id));
    }
}
