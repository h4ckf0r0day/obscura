use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::types::CdpEvent;
use crate::util::url_is_file_scheme;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    parent_session_id: &Option<String>,
) -> Result<Value, String> {
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
                        "canAccessOpener": false,
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
                            "canAccessOpener": false,
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
                        "canAccessOpener": false,
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
            let context_id = params.get("browserContextId").and_then(|v| v.as_str());
            let context = match context_id {
                Some(id) => ctx
                    .browser_context(id)
                    .ok_or_else(|| format!("Browser context not found: {}", id))?,
                None => &ctx.default_context,
            };

            // Same gate as Page.navigate (GHSA-q55h-vfv9-qcr5). Without this,
            // a CDP client can call Target.createTarget {url:"file:///etc/passwd"}
            // and then Runtime.evaluate the body off the created target,
            // bypassing the page-domain check entirely.
            if url_is_file_scheme(url) && !context.allow_file_access {
                return Err(
                    "Target.createTarget to file:// is disabled. Restart with `obscura serve --allow-file-access` to enable.".to_string()
                );
            }

            let page_id = ctx.create_page_in_context(context_id)?;
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
                            "canAccessOpener": false,
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
                            "canAccessOpener": false,
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
                        "canAccessOpener": false,
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
            if ctx.get_page(target_id).is_none() {
                return Err("Target not found".to_string());
            }
            let session_id = ctx.next_target_session(target_id);
            ctx.sessions
                .insert(session_id.clone(), target_id.to_string());

            if let Some(page) = ctx.get_page(target_id) {
                let params = json!({
                    "sessionId": session_id,
                    "targetInfo": {
                        "targetId": target_id,
                        "type": "page",
                        "title": page.title,
                        "url": page.url_string(),
                        "attached": true,
                        "canAccessOpener": false,
                        "browserContextId": page.context.id,
                    },
                    "waitingForDebugger": false,
                });
                let event = match parent_session_id {
                    Some(parent_session_id) => CdpEvent::with_session(
                        "Target.attachedToTarget",
                        params,
                        parent_session_id.clone(),
                    ),
                    None => CdpEvent::new("Target.attachedToTarget", params),
                };
                ctx.pending_events.push(event);
            }

            Ok(json!({ "sessionId": session_id }))
        }
        "closeTarget" => {
            let target_id = params
                .get("targetId")
                .and_then(|v| v.as_str())
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
        // No multi-target lifecycle to manage: obscura runs one page per session.
        // Ack these so Chrome-shaped clients that call them do not warn (issue #340).
        "detachFromTarget" => {
            if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                let page_id = ctx.detach_session(session_id);
                if let Some(page_id) = page_id {
                    ctx.refresh_runtime_event_collection(&page_id);
                }
                #[cfg(feature = "render")]
                ctx.screencasts.remove(session_id);
            }
            Ok(json!({}))
        }
        "activateTarget" => Ok(json!({})),
        "getBrowserContexts" => {
            let mut ids: Vec<&String> = ctx.browser_contexts.keys().collect();
            ids.sort();
            Ok(json!({ "browserContextIds": ids }))
        }
        "createBrowserContext" => {
            let id = ctx.create_browser_context();
            Ok(json!({ "browserContextId": id }))
        }
        "disposeBrowserContext" => {
            let context_id = params
                .get("browserContextId")
                .and_then(|v| v.as_str())
                .ok_or("browserContextId required")?;
            let sessions: Vec<(String, String)> = ctx
                .sessions
                .iter()
                .filter_map(|(session_id, page_id)| {
                    ctx.get_page(page_id)
                        .filter(|page| page.context.id == context_id)
                        .map(|_| (session_id.clone(), page_id.clone()))
                })
                .collect();
            let page_ids = ctx.dispose_browser_context(context_id)?;
            for (session_id, page_id) in sessions {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.detachedFromTarget",
                    json!({ "sessionId": session_id, "targetId": page_id }),
                ));
            }
            for page_id in page_ids {
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetDestroyed",
                    json!({ "targetId": page_id }),
                ));
            }
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
                            "canAccessOpener": false,
                            "browserContextId": page.context.id,
                        }
                    }))
                }
                None => {
                    // canAccessOpener is required on every TargetInfo per the
                    // CDP spec. Strict clients (chromiumoxide) panic if it's
                    // missing. The browser target itself has no opener.
                    Ok(json!({
                        "targetInfo": {
                            "targetId": "browser",
                            "type": "browser",
                            "title": "",
                            "url": "",
                            "attached": true,
                            "canAccessOpener": false,
                        }
                    }))
                }
            }
        }
        _ => Err(format!("Unknown Target method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn browser_contexts_are_real_and_do_not_clear_default_cookies() {
        let mut ctx = CdpContext::new();
        ctx.default_context.cookie_jar.set_cookie(
            "sid=default",
            &url::Url::parse("https://example.com").unwrap(),
        );

        let created = handle("createBrowserContext", &json!({}), &mut ctx, &None)
            .await
            .expect("context creation should succeed");
        let context_id = created["browserContextId"].as_str().unwrap();
        assert_ne!(context_id, "default");
        assert!(ctx
            .browser_context(context_id)
            .unwrap()
            .cookie_jar
            .get_all_cookies()
            .is_empty());
        assert_eq!(ctx.default_context.cookie_jar.get_all_cookies().len(), 1);

        let listed = handle("getBrowserContexts", &json!({}), &mut ctx, &None)
            .await
            .expect("context listing should succeed");
        assert_eq!(listed["browserContextIds"], json!([context_id]));
    }

    #[tokio::test]
    async fn disposing_context_removes_only_its_pages() {
        let mut ctx = CdpContext::new();
        let context_id = ctx.create_browser_context();
        let isolated_page = ctx.create_page_in_context(Some(&context_id)).unwrap();
        let default_page = ctx.create_page();

        handle(
            "disposeBrowserContext",
            &json!({"browserContextId": context_id}),
            &mut ctx,
            &None,
        )
        .await
        .expect("context disposal should succeed");

        assert!(ctx.get_page(&isolated_page).is_none());
        assert!(ctx.get_page(&default_page).is_some());
        assert!(ctx.browser_contexts.is_empty());
    }

    #[tokio::test]
    async fn attach_to_browser_target_returns_session_id() {
        let mut ctx = CdpContext::new();
        let result = handle("attachToBrowserTarget", &json!({}), &mut ctx, &None)
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
    async fn explicit_page_attachment_is_unique_and_scoped_to_its_parent_session() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let managed_session = format!("{page_id}-session");
        ctx.sessions
            .insert(managed_session.clone(), page_id.clone());
        let parent_session = Some("browser-session".to_string());

        let first = handle(
            "attachToTarget",
            &json!({"targetId": page_id, "flatten": true}),
            &mut ctx,
            &parent_session,
        )
        .await
        .expect("first explicit attachment should succeed");
        let first_session = first["sessionId"].as_str().unwrap().to_string();

        assert_ne!(first_session, managed_session);
        assert_eq!(
            ctx.sessions.get(&first_session).map(String::as_str),
            Some(page_id.as_str())
        );
        let first_event = ctx.pending_events.last().unwrap();
        assert_eq!(first_event.method, "Target.attachedToTarget");
        assert_eq!(first_event.session_id.as_deref(), Some("browser-session"));
        assert_eq!(first_event.params["sessionId"], first_session);

        let second = handle(
            "attachToTarget",
            &json!({"targetId": page_id, "flatten": true}),
            &mut ctx,
            &parent_session,
        )
        .await
        .expect("second explicit attachment should succeed");
        assert_ne!(second["sessionId"], first["sessionId"]);
    }

    #[tokio::test]
    async fn detaching_explicit_session_removes_its_page_route() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let parent_session = Some("browser-session".to_string());
        let attached = handle(
            "attachToTarget",
            &json!({"targetId": page_id}),
            &mut ctx,
            &parent_session,
        )
        .await
        .unwrap();
        let session_id = attached["sessionId"].as_str().unwrap().to_string();
        ctx.page_enabled_sessions.insert(session_id.clone());
        ctx.lifecycle_enabled_sessions.insert(session_id.clone());
        ctx.network_enabled_sessions.insert(session_id.clone());
        ctx.runtime_enabled_sessions.insert(session_id.clone());
        ctx.page_announced_frames_by_session
            .insert(session_id.clone(), vec!["frame-1".into()]);
        ctx.announced_frames
            .insert(page_id.clone(), vec!["frame-1".into()]);

        handle(
            "detachFromTarget",
            &json!({"sessionId": session_id}),
            &mut ctx,
            &parent_session,
        )
        .await
        .expect("detach should succeed");
        assert!(!ctx.sessions.contains_key(&session_id));
        assert!(!ctx.page_enabled_sessions.contains(&session_id));
        assert!(!ctx.lifecycle_enabled_sessions.contains(&session_id));
        assert!(!ctx.network_enabled_sessions.contains(&session_id));
        assert!(!ctx.runtime_enabled_sessions.contains(&session_id));
        assert!(!ctx.page_announced_frames_by_session.contains_key(&session_id));
        assert!(ctx.announced_frames.contains_key(&page_id));
    }

    #[tokio::test]
    async fn closing_target_removes_every_session_subscription() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = "explicit-session".to_string();
        ctx.sessions.insert(session_id.clone(), page_id.clone());
        ctx.page_enabled_sessions.insert(session_id.clone());
        ctx.lifecycle_enabled_sessions.insert(session_id.clone());
        ctx.network_enabled_sessions.insert(session_id.clone());
        ctx.runtime_enabled_sessions.insert(session_id.clone());
        ctx.page_announced_frames_by_session
            .insert(session_id.clone(), vec!["frame-1".into()]);
        ctx.announced_frames
            .insert(page_id.clone(), vec!["frame-1".into()]);

        handle(
            "closeTarget",
            &json!({"targetId": page_id}),
            &mut ctx,
            &None,
        )
        .await
        .expect("target close should succeed");

        assert!(!ctx.sessions.contains_key(&session_id));
        assert!(!ctx.page_enabled_sessions.contains(&session_id));
        assert!(!ctx.lifecycle_enabled_sessions.contains(&session_id));
        assert!(!ctx.network_enabled_sessions.contains(&session_id));
        assert!(!ctx.runtime_enabled_sessions.contains(&session_id));
        assert!(!ctx.page_announced_frames_by_session.contains_key(&session_id));
        assert!(!ctx.announced_frames.contains_key(&page_id));
    }

    #[tokio::test]
    async fn unknown_target_method_still_errors() {
        let mut ctx = CdpContext::new();
        let err = handle("notARealMethod", &json!({}), &mut ctx, &None)
            .await
            .expect_err("unknown methods must surface as errors");
        assert!(err.contains("Unknown Target method"));
    }

    /// Regression for #122 item 5: every TargetInfo payload must carry the
    /// `canAccessOpener` field. The browser-target branch of getTargetInfo
    /// (no targetId passed → no page) used to omit it; strict CDP clients
    /// like chromiumoxide panic when the field is missing.
    #[tokio::test]
    async fn get_target_info_browser_target_includes_can_access_opener() {
        let mut ctx = CdpContext::new();
        // No targetId → falls through to the browser-target branch.
        let result = handle("getTargetInfo", &json!({}), &mut ctx, &None)
            .await
            .expect("getTargetInfo with no targetId must return browser info");

        let info = &result["targetInfo"];
        assert_eq!(info["type"], "browser", "must be the browser target");
        assert!(
            info.get("canAccessOpener").is_some(),
            "canAccessOpener must be present on every TargetInfo, got: {result}"
        );
        assert_eq!(info["canAccessOpener"], false);
    }
}
