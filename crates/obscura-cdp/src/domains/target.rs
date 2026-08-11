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
            // Chrome floods every current target on setDiscoverTargets, and
            // Puppeteer's TargetManager populates its view purely from these
            // targetCreated events (it does not re-poll getTargets). With
            // issue #544's global registry, the flood must cover every live
            // page in the process (not just this connection's), or a second
            // client would never learn about pages another connection owns.
            ctx.sync_registry();
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
            for target in ctx.registry.all() {
                let attached = ctx
                    .sessions
                    .values()
                    .any(|page_id| *page_id == target.target_id);
                ctx.pending_events.push(CdpEvent::new(
                    "Target.targetCreated",
                    json!({
                        "targetInfo": {
                            "targetId": target.target_id,
                            "type": "page",
                            "title": target.title,
                            "url": target.url,
                            "attached": attached,
                            "canAccessOpener": false,
                            "browserContextId": target.browser_context_id,
                        }
                    }),
                ));
            }
            Ok(json!({}))
        }
        "getTargets" => {
            // Refresh this connection's own pages first: the shared registry
            // is a mirror, and the live Page objects are the freshest source
            // for url/title after any navigation (issue #544).
            ctx.sync_registry();
            let targets: Vec<Value> = ctx
                .registry
                .all()
                .into_iter()
                .map(|target| {
                    // attached means this caller can route a session to the
                    // target (it owns the page and has a session for it).
                    let attached = ctx
                        .sessions
                        .values()
                        .any(|page_id| *page_id == target.target_id);
                    json!({
                        "targetId": target.target_id,
                        "type": "page",
                        "title": target.title,
                        "url": target.url,
                        "attached": attached,
                        "canAccessOpener": false,
                        "browserContextId": target.browser_context_id,
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
            // createTarget may have navigated the page: refresh its global
            // target entry so getTargets and /json/list report the real url.
            ctx.sync_registry();

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
            // Puppeteer's browser-target createCDPSession attaches the
            // implicit "browser" target via Target.attachToTarget (not
            // attachToBrowserTarget). Chrome accepts targetId "browser"; the
            // registry only knows pages, so special-case it exactly like
            // attachToBrowserTarget.
            if target_id == "browser" {
                let session_id = "browser-session".to_string();
                ctx.sessions.insert(session_id.clone(), "browser".to_string());
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
                return Ok(json!({ "sessionId": session_id }));
            }
            // Issue #544 makes targets globally visible: Target.getTargets
            // lists every live page in the process, so attachToTarget must
            // accept any target the caller can see, not only pages this
            // connection owns. A page created by another connection used to
            // be listed but rejected here with "Target not found" (Hermes
            // repro). Resolve through the shared registry; the session stays
            // per-connection (sessions are connection-local), only the lookup
            // is global.
            ctx.sync_registry();
            let registry_target = ctx
                .registry
                .all()
                .into_iter()
                .find(|t| t.target_id == target_id)
                .ok_or_else(|| "Target not found".to_string())?;
            let session_id = ctx.next_target_session(target_id);
            ctx.sessions
                .insert(session_id.clone(), target_id.to_string());

            // Prefer the live Page when this connection owns it (freshest
            // url/title); for a remote target fall back to the registry
            // mirror, which the owning connection keeps current.
            let (title, url, browser_context_id) = match ctx.get_page(target_id) {
                Some(page) => (
                    page.title.clone(),
                    page.url_string(),
                    page.context.id.clone(),
                ),
                None => (
                    registry_target.title,
                    registry_target.url,
                    registry_target.browser_context_id,
                ),
            };
            let params = json!({
                "sessionId": session_id,
                "targetInfo": {
                    "targetId": target_id,
                    "type": "page",
                    "title": title,
                    "url": url,
                    "attached": true,
                    "canAccessOpener": false,
                    "browserContextId": browser_context_id,
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

            Ok(json!({ "sessionId": session_id }))
        }
        "closeTarget" => {
            let target_id = params
                .get("targetId")
                .and_then(|v| v.as_str())
                .ok_or("targetId required")?;
            // Like attachToTarget, closeTarget is browser-global in Chrome:
            // any connection may close any target listed by getTargets,
            // owned by this connection or not. Resolve through the shared
            // registry first so unknown targets surface a protocol error
            // instead of emitting destroy events for phantom pages.
            ctx.sync_registry();
            if !ctx
                .registry
                .all()
                .iter()
                .any(|t| t.target_id == target_id)
            {
                return Err("Target not found".to_string());
            }
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

            if ctx.get_page(target_id).is_some() {
                // Owned by this connection: full local teardown. `remove_page`
                // drops the live Page, its registry entry, and this
                // connection's session routes to it.
                ctx.remove_page(target_id);
            } else {
                // Remote-owned page: the live `Page` lives on its owner's
                // thread (thread-per-connection #430) and cannot be torn down
                // from here. Tombstone the id so the owner's next
                // sync_registry (getTargets, navigation, createTarget) cannot
                // resurrect the closed target. Also drop this connection's
                // own session routes to it.
                let removed_sessions: Vec<String> = ctx
                    .sessions
                    .iter()
                    .filter(|(_, page_id)| page_id.as_str() == target_id)
                    .map(|(session_id, _)| session_id.clone())
                    .collect();
                for session_id in &removed_sessions {
                    ctx.sessions.remove(session_id);
                    #[cfg(feature = "render")]
                    ctx.screencasts.remove(session_id);
                }
                ctx.registry.mark_closed(&[target_id.to_string()]);
            }
            Ok(json!({ "success": true }))
        }
        "setAutoAttach" => {
            // The BROWSER-level call (no parent session) auto-attaches the
            // current targets (flatten: true). Puppeteer's ChromeTargetManager
            // only surfaces targets that arrive with a session
            // (#attachedTargetsByTargetId feeds browser.pages()), so a
            // connection whose own pages are never announced cannot list or
            // drive them. Attach THIS connection's own pages, reusing the
            // managed session where one exists.
            //
            // Remote pages are deliberately NOT auto-attached: their live
            // `Page` lives on the owner's thread (thread-per-connection #430),
            // so a phantom session would fail the first time the client tried
            // to initialize the Page (Page.getFrameTree etc.). They remain
            // visible via Target.getTargets and /json/list and stay attachable
            // through an explicit attachToTarget (metadata-only, as documented
            // there).
            //
            // A SESSION-scoped call (parent session set) auto-attaches only
            // child targets of that session's page. Obscura has none (one
            // page per session; frames are not separate CDP targets), so emit
            // nothing, and crucially do NOT re-flood: puppeteer answers every
            // attachedToTarget with a session-scoped setAutoAttach, and a
            // re-flood would answer each with more attachedToTarget events, an
            // unbounded event storm.
            if parent_session_id.is_none()
                && params.get("autoAttach").and_then(|v| v.as_bool()).unwrap_or(false)
            {
                let own_pages: Vec<String> = ctx.pages.iter().map(|p| p.id.clone()).collect();
                for page_id in own_pages {
                    let existing: Option<String> = ctx
                        .sessions
                        .iter()
                        .find(|(_, pid)| pid.as_str() == page_id)
                        .map(|(sid, _)| sid.clone());
                    let session_id =
                        existing.unwrap_or_else(|| ctx.next_target_session(&page_id));
                    ctx.sessions.insert(session_id.clone(), page_id.clone());
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
                }
            }
            Ok(json!({}))
        }
        // No multi-target lifecycle to manage: obscura runs one page per session.
        // Ack these so Chrome-shaped clients that call them do not warn (issue #340).
        "detachFromTarget" => {
            if let Some(session_id) = params.get("sessionId").and_then(Value::as_str) {
                ctx.sessions.remove(session_id);
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

    /// Issue #544: targets are globally visible. A page created (and
    /// navigated) by one connection must show up in another connection's
    /// Target.getTargets with its current url, and a freshly created page
    /// must not stay shadowed by a stale about:blank entry.
    #[tokio::test]
    async fn get_targets_lists_pages_from_other_connections() {
        let registry = crate::registry::TargetRegistry::default();
        let mut owner = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry.clone(),
        );
        let mut observer = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry,
        );

        let page_id = owner.create_page();
        // createTarget wires a managed session for the new page; mirror that
        // here so the owner connection lists the page as attached.
        let session_id = format!("{page_id}-session");
        owner.sessions.insert(session_id, page_id.clone());
        // Simulate a completed navigation without touching the network: the
        // shared registry must pick up the new url/title on the next
        // getTargets, not keep reporting the initial about:blank.
        {
            let page = owner.get_page_mut(&page_id).unwrap();
            page.url = Some(url::Url::parse("https://example.com/").unwrap());
            page.title = "Example".into();
        }

        // The owner connection lists the page as attached (it holds the
        // session); the observer lists it as a remote, unattached target.
        let owner_targets = handle("getTargets", &json!({}), &mut owner, &None)
            .await
            .unwrap();
        assert_eq!(owner_targets["targetInfos"][0]["targetId"], page_id);
        assert_eq!(owner_targets["targetInfos"][0]["attached"], true);
        assert_eq!(
            owner_targets["targetInfos"][0]["url"],
            "https://example.com/",
            "getTargets must report the navigated url, not about:blank"
        );

        let observer_targets = handle("getTargets", &json!({}), &mut observer, &None)
            .await
            .expect("observer getTargets should succeed");
        assert_eq!(observer_targets["targetInfos"][0]["targetId"], page_id);
        assert_eq!(
            observer_targets["targetInfos"][0]["url"],
            "https://example.com/",
            "observer must see the navigated url, not a stale about:blank"
        );
        assert_eq!(observer_targets["targetInfos"][0]["attached"], false);
        assert_eq!(
            observer_targets["targetInfos"][0]["type"],
            "page",
            "every listed target must carry type page"
        );
    }

    /// Issue #544: setDiscoverTargets floods every live target, not just this
    /// connection's own pages. Puppeteer's TargetManager builds its view
    /// from these targetCreated events and never re-polls getTargets.
    #[tokio::test]
    async fn set_discover_targets_floods_remote_targets() {
        let registry = crate::registry::TargetRegistry::default();
        let mut owner = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry.clone(),
        );
        let mut observer = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry,
        );

        let page_id = owner.create_page();
        let session_id = format!("{page_id}-session");
        owner.sessions.insert(session_id, page_id.clone());
        {
            let page = owner.get_page_mut(&page_id).unwrap();
            page.title = "Discovered".into();
        }
        // The owner pushes its page's metadata into the shared registry.
        owner.sync_registry();

        handle("setDiscoverTargets", &json!({}), &mut observer, &None)
            .await
            .expect("setDiscoverTargets should succeed");

        let created: Vec<&crate::types::CdpEvent> = observer
            .pending_events
            .iter()
            .filter(|e| e.method == "Target.targetCreated")
            .collect();
        let page_event = created
            .iter()
            .find(|e| e.params["targetInfo"]["targetId"] == page_id)
            .expect("observer must be flooded with the remote page's targetCreated");
        assert_eq!(page_event.params["targetInfo"]["title"], "Discovered");
        assert_eq!(page_event.params["targetInfo"]["attached"], false);
        assert_eq!(page_event.params["targetInfo"]["type"], "page");

        // The owner's own flood marks its page attached.
        owner.pending_events.clear();
        handle("setDiscoverTargets", &json!({}), &mut owner, &None)
            .await
            .expect("owner setDiscoverTargets should succeed");
        let owner_event = owner
            .pending_events
            .iter()
            .find(|e| {
                e.method == "Target.targetCreated"
                    && e.params["targetInfo"]["targetId"] == page_id
            })
            .expect("owner must be flooded with its own page");
        assert_eq!(owner_event.params["targetInfo"]["attached"], true);
    }

    /// Issue #544: setAutoAttach (browser level) announces this connection's
    /// OWN pages with a session. Puppeteer's browser.pages() only surfaces
    /// session-bearing targets. Remote pages stay discoverable via getTargets
    /// and attachable via explicit attachToTarget, but are NOT auto-attached:
    /// their live Page lives on the owner's thread (#430), and a phantom
    /// session would fail when puppeteer initializes the Page.
    #[tokio::test]
    async fn set_auto_attach_attaches_own_pages_only() {
        let registry = crate::registry::TargetRegistry::default();
        let mut owner = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry.clone(),
        );
        let mut observer = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry,
        );

        let page_id = owner.create_page();
        let session_id = format!("{page_id}-session");
        owner.sessions.insert(session_id, page_id.clone());
        owner.sync_registry();

        // Owner: its own page is announced, reusing the managed session
        // instead of minting a duplicate for the same page.
        handle(
            "setAutoAttach",
            &json!({ "autoAttach": true, "flatten": true }),
            &mut owner,
            &None,
        )
        .await
        .expect("owner setAutoAttach should succeed");
        let owner_attached = owner
            .pending_events
            .iter()
            .find(|e| e.method == "Target.attachedToTarget")
            .expect("owner must receive attachedToTarget for its page");
        assert_eq!(
            owner_attached.params["sessionId"],
            format!("{page_id}-session"),
            "owner's auto-attach must reuse the managed session"
        );
        assert_eq!(owner_attached.params["waitingForDebugger"], false);
        assert_eq!(owner_attached.params["targetInfo"]["attached"], true);

        // autoAttach:false must not announce anything (Chrome semantics).
        owner.pending_events.clear();
        handle(
            "setAutoAttach",
            &json!({ "autoAttach": false, "flatten": true }),
            &mut owner,
            &None,
        )
        .await
        .expect("setAutoAttach with autoAttach:false should succeed");
        assert!(
            owner.pending_events.is_empty(),
            "autoAttach:false must not flood attachedToTarget, got: {:?}",
            owner.pending_events
        );

        // Observer: the remote page must NOT be auto-attached (no phantom
        // session that would fail on first drive attempt).
        handle(
            "setAutoAttach",
            &json!({ "autoAttach": true, "flatten": true }),
            &mut observer,
            &None,
        )
        .await
        .expect("observer setAutoAttach should succeed");
        assert!(
            observer.pending_events.is_empty(),
            "remote pages must not be auto-attached, got: {:?}",
            observer.pending_events
        );
        assert!(
            !observer.sessions.values().any(|pid| pid == &page_id),
            "no phantom session may route to the remote page"
        );

        // A SESSION-scoped setAutoAttach (puppeteer sends one for every
        // attachedToTarget it receives) auto-attaches only child targets;
        // obscura has none, so it must not re-flood the registry: that
        // would answer each flood with more attachedToTarget events forever.
        observer.pending_events.clear();
        handle(
            "setAutoAttach",
            &json!({ "autoAttach": true, "flatten": true }),
            &mut observer,
            &Some("page-9-session".to_string()),
        )
        .await
        .expect("session-scoped setAutoAttach should succeed");
        assert!(
            observer.pending_events.is_empty(),
            "session-scoped setAutoAttach must not re-flood, got: {:?}",
            observer.pending_events
        );
    }

    /// Issue #544: when the owning connection goes away its pages must leave
    /// the shared registry, so they stop shadowing live targets elsewhere.
    #[tokio::test]
    async fn dropped_connection_unregisters_its_pages() {
        let registry = crate::registry::TargetRegistry::default();
        let page_id = {
            let mut owner = CdpContext::new_with_shared_context_and_registry(
                CdpContext::new().default_context.clone(),
                registry.clone(),
            );
            let created = handle(
                "createTarget",
                &json!({"url": "about:blank"}),
                &mut owner,
                &None,
            )
            .await
            .expect("createTarget should succeed");
            created["targetId"].as_str().unwrap().to_string()
        };
        // `owner` dropped at end of block → its page must be gone globally.
        assert!(
            registry.all().is_empty(),
            "dropped connection must unregister its pages, got: {:?}",
            registry.all()
        );
        let _ = page_id;
    }

    /// Issue #544 follow-up: any target listed by getTargets must be
    /// attachable. A page created (and owned) by connection A must accept
    /// attachToTarget from a second connection with a fresh session id,
    /// instead of the old per-connection "Target not found" (Hermes repro).
    #[tokio::test]
    async fn attach_to_target_accepts_targets_from_other_connections() {
        let registry = crate::registry::TargetRegistry::default();
        let mut owner = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry.clone(),
        );
        let mut observer = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry,
        );

        // Connection A creates a page via the real createTarget path, which
        // also wires its managed session, mirroring the repro.
        let created = handle(
            "createTarget",
            &json!({"url": "about:blank"}),
            &mut owner,
            &None,
        )
        .await
        .expect("createTarget should succeed");
        let page_id = created["targetId"].as_str().unwrap().to_string();

        // Connection B sees the page globally…
        let targets = handle("getTargets", &json!({}), &mut observer, &None)
            .await
            .expect("observer getTargets should succeed");
        assert_eq!(targets["targetInfos"][0]["targetId"], page_id);

        // …and must be able to attach to it, not fail with Target not found.
        let attached = handle(
            "attachToTarget",
            &json!({"targetId": page_id, "flatten": true}),
            &mut observer,
            &None,
        )
        .await
        .expect("attachToTarget must accept targets listed by getTargets");
        let session_id = attached["sessionId"].as_str().unwrap();
        assert!(!session_id.is_empty());
        assert_eq!(
            observer.sessions.get(session_id).map(String::as_str),
            Some(page_id.as_str()),
            "the attaching connection must hold its own session route"
        );

        // The attachedToTarget event carries the page metadata from the
        // registry mirror, since the observer does not own the page.
        let evt = observer
            .pending_events
            .iter()
            .find(|e| e.method == "Target.attachedToTarget")
            .expect("attachedToTarget event must be emitted");
        assert_eq!(evt.params["targetInfo"]["targetId"], page_id);
        assert_eq!(evt.params["targetInfo"]["url"], "about:blank");
        assert_eq!(evt.params["targetInfo"]["type"], "page");
    }

    /// A target that is not registered anywhere must still be rejected, so
    /// attachToTarget does not hand out sessions for phantom ids.
    #[tokio::test]
    async fn attach_to_target_unknown_target_still_errors() {
        let mut ctx = CdpContext::new();
        let err = handle(
            "attachToTarget",
            &json!({"targetId": "page-999"}),
            &mut ctx,
            &None,
        )
        .await
        .expect_err("unknown targets must still error");
        assert_eq!(err, "Target not found");
    }

    /// Issue #544 follow-up: closeTarget is browser-global in Chrome, so a
    /// connection may close a page owned by another connection. The target
    /// must leave the shared registry (its global entry), matching what any
    /// other connection's getTargets would report.
    #[tokio::test]
    async fn close_target_removes_targets_owned_by_other_connections() {
        let registry = crate::registry::TargetRegistry::default();
        let mut owner = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry.clone(),
        );
        let mut observer = CdpContext::new_with_shared_context_and_registry(
            CdpContext::new().default_context.clone(),
            registry.clone(),
        );

        let created = handle(
            "createTarget",
            &json!({"url": "about:blank"}),
            &mut owner,
            &None,
        )
        .await
        .expect("createTarget should succeed");
        let page_id = created["targetId"].as_str().unwrap().to_string();

        // Connection B closes connection A's page: allowed, matching Chrome.
        let closed = handle(
            "closeTarget",
            &json!({"targetId": page_id}),
            &mut observer,
            &None,
        )
        .await
        .expect("closing another connection's target should succeed");
        assert_eq!(closed["success"], true);
        assert!(
            !registry.all().iter().any(|t| t.target_id == page_id),
            "closed target must leave the global registry"
        );

        // The owner still holds the live Page, so its next sync (getTargets)
        // must NOT resurrect the tombstoned target: it has to stay closed for
        // every connection until the owner really drops it.
        let owner_targets = handle("getTargets", &json!({}), &mut owner, &None)
            .await
            .expect("owner getTargets should succeed");
        let owner_listed = owner_targets["targetInfos"]
            .as_array()
            .expect("targetInfos must be an array");
        assert!(
            !owner_listed.iter().any(|t| t["targetId"] == page_id),
            "owner's sync must not resurrect the remotely closed target, got: {owner_targets}"
        );
        // The owner's sync also drops the closed page for real, reclaiming
        // the Page (and its isolate) and clearing the tombstone instead of
        // holding both for the life of the connection.
        assert!(
            owner.get_page(&page_id).is_none(),
            "owner must drop the remotely closed page from its own pages"
        );
        assert!(
            !registry.is_closed(&page_id),
            "dropping the closed page must clear its tombstone"
        );

        // …but a page the owner creates afterwards is unaffected.
        let second = handle(
            "createTarget",
            &json!({"url": "about:blank"}),
            &mut owner,
            &None,
        )
        .await
        .expect("owner createTarget after remote close should succeed");
        let second_id = second["targetId"].as_str().unwrap().to_string();
        assert_ne!(second_id, page_id);
        assert!(
            registry.all().iter().any(|t| t.target_id == second_id),
            "a new page created by the owner must still register globally"
        );
    }

    /// Closing a target that was never registered must surface a protocol
    /// error instead of acking and emitting destroy events for a phantom.
    #[tokio::test]
    async fn close_target_unknown_target_errors() {
        let mut ctx = CdpContext::new();
        let err = handle(
            "closeTarget",
            &json!({"targetId": "page-999"}),
            &mut ctx,
            &None,
        )
        .await
        .expect_err("unknown targets must error");
        assert_eq!(err, "Target not found");
    }

    /// Puppeteer's browser-target createCDPSession sends
    /// Target.attachToTarget {targetId: "browser"} (not
    /// attachToBrowserTarget). Chrome accepts it; the registry only knows
    /// pages, so the browser target must be special-cased.
    #[tokio::test]
    async fn attach_to_target_accepts_browser_target_id() {
        let mut ctx = CdpContext::new();
        let result = handle(
            "attachToTarget",
            &json!({ "targetId": "browser", "flatten": true }),
            &mut ctx,
            &None,
        )
        .await
        .expect("attachToTarget on the browser target should succeed");

        assert_eq!(result["sessionId"], "browser-session");
        assert_eq!(
            ctx.sessions.get("browser-session").map(String::as_str),
            Some("browser")
        );
        let evt = ctx
            .pending_events
            .iter()
            .find(|e| e.method == "Target.attachedToTarget")
            .expect("attachedToTarget event must be emitted");
        assert_eq!(evt.params["targetInfo"]["type"], "browser");
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

        handle(
            "detachFromTarget",
            &json!({"sessionId": session_id}),
            &mut ctx,
            &parent_session,
        )
        .await
        .expect("detach should succeed");
        assert!(!ctx.sessions.contains_key(&session_id));
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
