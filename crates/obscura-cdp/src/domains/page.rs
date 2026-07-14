use obscura_browser::lifecycle::WaitUntil;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::types::CdpEvent;
use crate::util::url_is_file_scheme;

/// Emit the post-navigation event stream into `ctx.pending_events`. Shared
/// by both the in-process `do_navigate` path and the spawned path in
/// `server::process_navigation`, so the recent goto-returns-Response /
/// per-isolated-world fixes don't have to be duplicated.
pub fn emit_navigation_events(
    ctx: &mut CdpContext,
    session_id: &Option<String>,
    frame_id: &str,
    loader_id: &str,
    page_url: &str,
    page_id: &str,
    network_events: &[obscura_browser::NetworkEvent],
    wait_until: WaitUntil,
    reached_network_idle: bool,
) {
    let es = session_id.clone();
    let ts = timestamp();

    // Real Chrome uses the navigation's loaderId as the main document's
    // request id, and Puppeteer/Playwright identify the navigation response
    // via `requestId === loaderId && type === "Document"` (issue #189).
    let nav_request_ids: Vec<String> = {
        let mut nav_seen = false;
        network_events.iter().map(|ev| {
            if !nav_seen && ev.resource_type == "Document" && ev.url == page_url {
                nav_seen = true;
                loader_id.to_string()
            } else {
                ev.request_id.clone()
            }
        }).collect()
    };
    let nav_idx: Option<usize> = network_events
        .iter()
        .position(|ev| ev.resource_type == "Document" && ev.url == page_url);

    // The main resource's body is stored under its internal request id, but the
    // client sees it as `loader_id` (the requestId we report above). Alias it so
    // Network.getResponseBody(loaderId) resolves, which is the only way a client
    // navigating straight to an image/PDF/other resource can read the main body
    // (issue #340). Also read the real Content-Type so frameNavigated reports the
    // actual mime instead of a hardcoded text/html.
    let mut nav_mime = "text/html".to_string();
    if let Some(idx) = nav_idx {
        let internal_id = &network_events[idx].request_id;
        if let Some(ct) = network_events[idx].response_headers.get("content-type") {
            // Strip any `; charset=...` parameter; frameNavigated wants the essence.
            nav_mime = ct.split(';').next().unwrap_or(ct).trim().to_string();
        }
        if internal_id != loader_id {
            if let Some(page) = ctx.get_page_mut(page_id) {
                page.alias_response_body(internal_id, loader_id);
            }
        }
    }

    // Playwright needs `Network.requestWillBeSent` for the main document to
    // arrive BEFORE `Page.frameNavigated` (issue #190).
    if let Some(idx) = nav_idx {
        let net_event = &network_events[idx];
        let rid = &nav_request_ids[idx];
        ctx.pending_events.push(CdpEvent {
            method: "Network.requestWillBeSent".into(),
            params: json!({"requestId": rid, "loaderId": loader_id, "documentURL": page_url, "request": {"url": net_event.url, "method": net_event.method, "headers": net_event.headers}, "timestamp": net_event.timestamp, "wallTime": net_event.timestamp, "initiator": {"type": "other"}, "type": net_event.resource_type, "frameId": frame_id}),
            session_id: es.clone(),
        });
    }

    // executionContextsCleared invalidates every prior context id, so a
    // Runtime.evaluate / callFunctionOn targeting a pre-navigation context
    // must be rejected (Chrome: "Cannot find context with specified id"). The
    // default world (id 2) and isolated worlds are re-registered below as their
    // executionContextCreated events are emitted. Issue #407: previously this
    // set was insert-only, so stale ids kept validating and grew unbounded.
    ctx.valid_context_ids.clear();
    let mut phase1 = vec![
        CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "init", "timestamp": ts}), session_id: es.clone() },
        CdpEvent { method: "Runtime.executionContextsCleared".into(), params: json!({}), session_id: es.clone() },
        CdpEvent { method: "Page.frameNavigated".into(), params: json!({"frame": {"id": frame_id, "loaderId": loader_id, "url": page_url, "domainAndRegistry": "", "securityOrigin": page_url, "mimeType": nav_mime, "adFrameStatus": {"adFrameType": "none"}}, "type": "Navigation"}), session_id: es.clone() },
        CdpEvent { method: "Runtime.executionContextCreated".into(), params: json!({"context": {"id": 2, "origin": page_url, "name": "", "uniqueId": format!("ctx-nav-{}", page_id), "auxData": {"isDefault": true, "type": "default", "frameId": frame_id}}}), session_id: es.clone() },
    ];
    // The default world is re-created as context id 2; re-register it. Isolated
    // worlds register themselves via next_isolated_context in the loop below.
    ctx.valid_context_ids.insert(2);
    let world_names: Vec<String> = if ctx.isolated_worlds.is_empty() {
        vec!["__puppeteer_utility_world__24.40.0".to_string()]
    } else {
        ctx.isolated_worlds.clone()
    };
    // Issue #192: fresh, monotonically increasing executionContextId per re-create.
    for world_name in &world_names {
        let world_ctx_id = ctx.next_isolated_context();
        phase1.push(CdpEvent {
            method: "Runtime.executionContextCreated".into(),
            params: json!({"context": {"id": world_ctx_id, "origin": page_url, "name": world_name, "uniqueId": format!("ctx-isolated-nav-{}-{}", page_id, world_ctx_id), "auxData": {"isDefault": false, "type": "isolated", "frameId": frame_id}}}),
            session_id: es.clone(),
        });
    }
    phase1.push(CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "commit", "timestamp": ts}), session_id: es.clone() });
    ctx.pending_events.extend(phase1);

    if ctx.fetch_intercept.enabled {
        for (i, net_event) in network_events.iter().enumerate() {
            let rid = &nav_request_ids[i];
            ctx.pending_events.push(CdpEvent {
                method: "Fetch.requestPaused".into(),
                params: json!({
                    "requestId": rid,
                    "request": {
                        "url": net_event.url,
                        "method": net_event.method,
                        "headers": net_event.headers,
                    },
                    "frameId": frame_id,
                    "resourceType": net_event.resource_type,
                    "networkId": rid,
                }),
                session_id: es.clone(),
            });
        }
    }

    for (i, net_event) in network_events.iter().enumerate() {
        let rid = &nav_request_ids[i];
        if Some(i) != nav_idx {
            ctx.pending_events.push(CdpEvent {
                method: "Network.requestWillBeSent".into(),
                params: json!({"requestId": rid, "loaderId": loader_id, "documentURL": page_url, "request": {"url": net_event.url, "method": net_event.method, "headers": net_event.headers}, "timestamp": net_event.timestamp, "wallTime": net_event.timestamp, "initiator": {"type": "other"}, "type": net_event.resource_type, "frameId": frame_id}),
                session_id: es.clone(),
            });
        }
        ctx.pending_events.push(CdpEvent {
            method: "Network.responseReceived".into(),
            params: json!({"requestId": rid, "loaderId": loader_id, "timestamp": net_event.timestamp, "type": net_event.resource_type, "response": {"url": net_event.url, "status": net_event.status, "statusText": "", "headers": &*net_event.response_headers, "mimeType": net_event.response_headers.get("content-type").cloned().unwrap_or_default()}, "frameId": frame_id}),
            session_id: es.clone(),
        });
        ctx.pending_events.push(CdpEvent {
            method: "Network.loadingFinished".into(),
            params: json!({"requestId": rid, "timestamp": net_event.timestamp, "encodedDataLength": net_event.body_size}),
            session_id: es.clone(),
        });
    }

    let mut phase3 = vec![
        CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "DOMContentLoaded", "timestamp": ts}), session_id: es.clone() },
        CdpEvent { method: "Page.domContentEventFired".into(), params: json!({"timestamp": ts}), session_id: es.clone() },
        CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "load", "timestamp": ts}), session_id: es.clone() },
        CdpEvent { method: "Page.loadEventFired".into(), params: json!({"timestamp": ts}), session_id: es.clone() },
    ];
    if reached_network_idle || matches!(wait_until, WaitUntil::Load | WaitUntil::DomContentLoaded) {
        let idle_ts = timestamp();
        phase3.push(CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "networkIdle", "timestamp": idle_ts}), session_id: es.clone() });
    }
    phase3.push(CdpEvent { method: "Page.frameStoppedLoading".into(), params: json!({"frameId": frame_id}), session_id: es });
    ctx.pending_events.extend(phase3);

    // Target.targetInfoChanged: strict CDP clients (browser-use, and
    // Puppeteer/Playwright `page.url()` tracking) cache the TargetInfo from
    // attachedToTarget and only refresh it on this event. Without it they keep
    // reporting the pre-navigation url/title (about:blank) and never see the
    // loaded page. Emit it browser-level (no sessionId) with the new url/title.
    let (tic_title, tic_ctx) = ctx
        .get_page(page_id)
        .map(|p| (p.title.clone(), p.context.id.clone()))
        .unwrap_or_default();
    ctx.pending_events.push(CdpEvent::new(
        "Target.targetInfoChanged",
        json!({
            "targetInfo": {
                "targetId": page_id,
                "type": "page",
                "title": tic_title,
                "url": page_url,
                "attached": true,
                "canAccessOpener": false,
                "browserContextId": tic_ctx,
            }
        }),
    ));
}

/// Parse the `waitUntil` argument that Puppeteer/Playwright pass on
/// `Page.navigate`.
pub fn parse_wait_until(params: &Value) -> WaitUntil {
    params
        .get("waitUntil")
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(WaitUntil::from_str(s))
            } else if let Some(arr) = v.as_array() {
                arr.iter()
                    .filter_map(|item| item.as_str())
                    .map(WaitUntil::from_str)
                    .max_by_key(|w| match w {
                        WaitUntil::DomContentLoaded => 0,
                        WaitUntil::Load => 1,
                        WaitUntil::NetworkIdle2 => 2,
                        WaitUntil::NetworkIdle0 => 3,
                    })
            } else {
                None
            }
        })
        // Puppeteer and Playwright drive navigation via `Page.navigate`
        // without a server-side waitUntil — they wait for `Page.lifecycleEvent`
        // on the client side. Defaulting the server to `Load` means we run
        // every parser/deferred/async script on JS-heavy pages before
        // emitting `load`, which on sites like github.com / reddit.com
        // pushes nav past 25s and clients time out at 15s. Real Chrome
        // streams `DOMContentLoaded` as soon as the parser is done; we
        // batch our event emission at the end of navigation, so the
        // closest we can get is to default to `DomContentLoaded` and skip
        // the full-load wait. CLI callers that pass `--wait-until load`
        // (or `networkidle*`) are unaffected; they get the old behaviour.
        .unwrap_or(WaitUntil::DomContentLoaded)
}

async fn do_navigate(
    url: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    let wait_until = parse_wait_until(params);

    // Block CDP-initiated file:// navigation by default.
    // Anyone who can reach the CDP port (default localhost,
    // but Docker images bind 0.0.0.0) could otherwise read
    // any file the obscura process can read. Opt in via
    // `obscura serve --allow-file-access` when local-HTML
    // testing is the intended workflow.
    if url_is_file_scheme(url) && !ctx.default_context.allow_file_access {
        return Err(
            "Page.navigate to file:// is disabled. Restart with `obscura serve --allow-file-access` to enable.".to_string()
        );
    }

    let preload_scripts: Vec<String> = ctx.preload_scripts.iter().map(|(_, s)| s.clone()).collect();

    let (frame_id, loader_id, network_events, page_url, page_id, reached_network_idle) = {
        let page = ctx.get_session_page_mut(session_id).ok_or("No page for session")?;
        let frame_id = page.frame_id.clone();
        let loader_id = format!("loader-{}", uuid::Uuid::new_v4());

        // Preloads (addBinding shims, addScriptToEvaluateOnNewDocument sources)
        // must run BEFORE the page's own scripts (CDP contract). Hand them to
        // the page so navigate_single can inject them at the right point.
        page.set_preload_scripts(preload_scripts);

        let nav_method = params.get("__method").and_then(|v| v.as_str()).unwrap_or("GET");
        let nav_body = params.get("__body").and_then(|v| v.as_str()).unwrap_or("");
        if nav_method == "POST" && !nav_body.is_empty() {
            page.navigate_with_wait_post(url, wait_until, nav_method, nav_body).await.map_err(|e| e.to_string())?;
        } else {
            page.navigate_with_wait(url, wait_until).await.map_err(|e| e.to_string())?;
        }

        let reached_network_idle = page.lifecycle.is_network_idle();
        // Fold in script-initiated requests (fetch/XHR/dynamic resource) so they
        // emit as Network events alongside static subresources (#406).
        page.sync_js_network_events();
        let network_events: Vec<_> = page.network_events.drain(..).collect();
        let page_url = page.url_string();
        let page_id = page.id.clone();
        (frame_id, loader_id, network_events, page_url, page_id, reached_network_idle)
    };

    emit_navigation_events(
        ctx,
        session_id,
        &frame_id,
        &loader_id,
        &page_url,
        &page_id,
        &network_events,
        wait_until,
        reached_network_idle,
    );

    Ok(json!({
        "frameId": frame_id,
        "loaderId": loader_id,
    }))
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => Ok(json!({})),
        "navigate" => {
            let url = params.get("url").and_then(|v| v.as_str())
                .ok_or("url required")?;
            do_navigate(url, params, ctx, session_id).await
        }
        "reload" => {
            let current_url = ctx.get_session_page(session_id)
                .map(|p| p.url_string())
                .unwrap_or_else(|| "about:blank".to_string());
            let reload_params = json!({
                "waitUntil": params.get("waitUntil").cloned().unwrap_or(json!("load"))
            });
            do_navigate(&current_url, &reload_params, ctx, session_id).await
        }
        "getFrameTree" => {
            let page = ctx.get_session_page(session_id).ok_or("No page for session")?;
            Ok(json!({
                "frameTree": {
                    "frame": {
                        "id": page.frame_id,
                        "loaderId": "initial-loader",
                        "url": page.url_string(),
                        "domainAndRegistry": "",
                        "securityOrigin": page.url_string(),
                        "mimeType": "text/html",
                        "adFrameStatus": { "adFrameType": "none" },
                    },
                    "childFrames": [],
                }
            }))
        }
        "createIsolatedWorld" => {
            let (frame_id_param, world_name, page_url, page_id) = {
                let page = ctx.get_session_page(session_id).ok_or("No page for session")?;
                (
                    params.get("frameId").and_then(|v| v.as_str())
                        .unwrap_or(&page.frame_id).to_string(),
                    params.get("worldName").and_then(|v| v.as_str())
                        .unwrap_or("").to_string(),
                    page.url_string(),
                    page.id.clone(),
                )
            };
            // Track this world so Page.navigate can re-emit a context for it
            // post-navigation. Without this, Playwright (and Puppeteer)
            // hang in any operation that uses the utility world — including
            // page.title() — because their utility world is gone after
            // Runtime.executionContextsCleared and never re-created.
            if !world_name.is_empty() && !ctx.isolated_worlds.contains(&world_name) {
                ctx.isolated_worlds.push(world_name.clone());
            }
            // Issue #192: every isolated world emission gets a fresh id from
            // the monotonic counter and is registered as a valid contextId.
            // Reusing id 100 across navigations made Playwright's bookkeeping
            // diverge (it expected 101 on the second nav) and Runtime.evaluate
            // failed with "Cannot find context with specified id: 101".
            let context_id = ctx.next_isolated_context();

            ctx.pending_events.push(CdpEvent {
                method: "Runtime.executionContextCreated".to_string(),
                params: json!({
                    "context": {
                        "id": context_id,
                        "origin": page_url,
                        "name": world_name,
                        "uniqueId": format!("ctx-isolated-{}-{}", page_id, context_id),
                        "auxData": {
                            "isDefault": false,
                            "type": "isolated",
                            "frameId": frame_id_param,
                        }
                    }
                }),
                session_id: session_id.clone(),
            });

            Ok(json!({ "executionContextId": context_id }))
        }
        "setLifecycleEventsEnabled" => Ok(json!({})),
        "addScriptToEvaluateOnNewDocument" => {
            let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");
            ctx.preload_counter += 1;
            let identifier = format!("{}", ctx.preload_counter);
            if !source.is_empty() {
                ctx.preload_scripts.push((identifier.clone(), source.to_string()));
            }
            Ok(json!({ "identifier": identifier }))
        }
        "removeScriptToEvaluateOnNewDocument" => {
            let identifier = params.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
            ctx.preload_scripts.retain(|(id, _)| id != identifier);
            Ok(json!({}))
        }
        "setInterceptFileChooserDialog" => Ok(json!({})),
        // Obscura does not download files to disk, so there is no behavior to
        // configure; ack it so clients that set it do not warn (issue #340).
        "setDownloadBehavior" => Ok(json!({})),
        "getLayoutMetrics" => {
            // Obscura has no visual layout engine, so we return a fixed
            // 1280x720 viewport (Chrome's default) and try to derive the
            // content height from document.documentElement.scrollHeight.
            // Playwright calls this before every page.screenshot() and
            // would otherwise fail with "Unknown Page method".
            let width = 1280.0_f64;
            let height = 720.0_f64;
            let content_height = ctx
                .get_session_page_mut(session_id)
                .map(|p| p.evaluate("document.documentElement && document.documentElement.scrollHeight"))
                .and_then(|v| v.as_f64())
                .filter(|n| *n > 0.0)
                .unwrap_or(height);
            let layout_viewport = json!({
                "pageX": 0, "pageY": 0,
                "clientWidth": width, "clientHeight": height,
            });
            let visual_viewport = json!({
                "offsetX": 0.0, "offsetY": 0.0,
                "pageX": 0.0, "pageY": 0.0,
                "clientWidth": width, "clientHeight": height,
                "scale": 1.0, "zoom": 1.0,
            });
            let content_size = json!({
                "x": 0.0, "y": 0.0,
                "width": width, "height": content_height,
            });
            Ok(json!({
                "layoutViewport": layout_viewport,
                "visualViewport": visual_viewport,
                "contentSize": content_size,
                "cssLayoutViewport": layout_viewport,
                "cssVisualViewport": visual_viewport,
                "cssContentSize": content_size,
            }))
        }
        "getNavigationHistory" => {
            let page = ctx.get_session_page(session_id).ok_or("No page for session")?;
            // Synthesize an entry for the current page when history is empty
            // (initial about:blank, never-navigated targets). Puppeteer's
            // goBack reads `currentIndex` and `entries[currentIndex-1]`;
            // an empty entries[] used to make every back/forward fail.
            let entries: Vec<Value> = if page.history.is_empty() {
                vec![json!({
                    "id": 0,
                    "url": page.url_string(),
                    "userTypedURL": page.url_string(),
                    "title": page.title,
                    "transitionType": "typed",
                })]
            } else {
                page.history.iter().enumerate().map(|(i, url)| json!({
                    "id": i as u64,
                    "url": url,
                    "userTypedURL": url,
                    "title": if i == page.history_index { page.title.clone() } else { String::new() },
                    "transitionType": "typed",
                })).collect()
            };
            Ok(json!({
                "currentIndex": if page.history.is_empty() { 0 } else { page.history_index },
                "entries": entries,
            }))
        }
        "navigateToHistoryEntry" => {
            let entry_id = params.get("entryId").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let target_url = {
                let page = ctx.get_session_page_mut(session_id).ok_or("No page for session")?;
                let url = page.history.get(entry_id).cloned();
                if url.is_some() {
                    page.set_history_index(entry_id);
                }
                url
            };
            if let Some(url) = target_url {
                // Stash + restore history so push_history doesn't clobber
                // the cursor we just moved.
                let stash = {
                    let page = ctx.get_session_page_mut(session_id).ok_or("No page for session")?;
                    (page.history.clone(), page.history_index)
                };
                let (frame_id, page_id, network_events, page_url, reached_idle) = {
                    let page = ctx.get_session_page_mut(session_id).ok_or("No page for session")?;
                    page.navigate_with_wait(&url, WaitUntil::DomContentLoaded).await.map_err(|e| e.to_string())?;
                    page.history = stash.0;
                    page.history_index = stash.1;
                    (
                        page.frame_id.clone(),
                        page.id.clone(),
                        page.network_events.drain(..).collect::<Vec<_>>(),
                        page.url_string(),
                        page.lifecycle.is_network_idle(),
                    )
                };
                let loader_id = format!("loader-{}", uuid::Uuid::new_v4());
                emit_navigation_events(
                    ctx, session_id,
                    &frame_id, &loader_id, &page_url, &page_id,
                    &network_events, WaitUntil::DomContentLoaded, reached_idle,
                );
            }
            Ok(json!({}))
        }
        "resetNavigationHistory" => {
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.history.clear();
                page.history_index = 0;
            }
            Ok(json!({}))
        }
        "printToPDF" => {
            // Obscura has no layout/rendering engine, so PDF generation is
            // intentionally not implemented. Returning a distinct, descriptive
            // error (rather than the generic "Unknown Page method" fallback)
            // tells Playwright/Puppeteer/headless_chrome clients exactly why
            // the call failed and what to do instead.
            Err(
                "Page.printToPDF is not supported by Obscura: no layout engine. \
                 Use Runtime.evaluate (e.g. page.evaluate) to extract the rendered \
                 HTML, then render to PDF in your client (wkhtmltopdf, weasyprint, \
                 a separate headless Chromium pipeline, etc.)."
                    .to_string(),
            )
        }
        "captureScreenshot" | "captureSnapshot" => {
            // Same story as printToPDF: rasterising a page needs a layout and
            // paint pipeline that Obscura intentionally does not have. Reply
            // with a clear error so clients can fail fast instead of waiting
            // on the generic "Unknown Page method" reply.
            Err(format!(
                "Page.{method} is not supported by Obscura: no layout or paint engine. \
                 For visual snapshots, drive a real headless Chromium for the \
                 screenshot leg of your pipeline and use Obscura for the scraping leg."
            ))
        }
        _ => Err(format!("Unknown Page method: {}", method)),
    }
}

fn timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::CdpContext;

    #[tokio::test]
    async fn get_layout_metrics_returns_chrome_default_viewport() {
        let mut ctx = CdpContext::new();
        let result = handle("getLayoutMetrics", &json!({}), &mut ctx, &None)
            .await
            .expect("getLayoutMetrics should succeed without a session");

        // CDP spec requires three top-level shapes; Playwright's screenshot
        // path reads contentSize.width/height to size the capture. Without
        // them the screenshot call panics with "cannot read property of
        // undefined".
        for key in [
            "layoutViewport",
            "visualViewport",
            "contentSize",
            "cssLayoutViewport",
            "cssVisualViewport",
            "cssContentSize",
        ] {
            assert!(result.get(key).is_some(), "missing key: {key}");
        }

        let layout = &result["layoutViewport"];
        assert_eq!(layout["clientWidth"].as_f64(), Some(1280.0));
        assert_eq!(layout["clientHeight"].as_f64(), Some(720.0));

        let visual = &result["visualViewport"];
        assert_eq!(visual["scale"].as_f64(), Some(1.0));
        assert_eq!(visual["clientWidth"].as_f64(), Some(1280.0));

        let content = &result["contentSize"];
        assert_eq!(content["width"].as_f64(), Some(1280.0));
        // Without a live page the content height falls back to the viewport.
        assert_eq!(content["height"].as_f64(), Some(720.0));
    }

    #[tokio::test]
    async fn unknown_page_method_still_errors() {
        let mut ctx = CdpContext::new();
        let err = handle("notARealMethod", &json!({}), &mut ctx, &None)
            .await
            .expect_err("unknown methods must surface as errors");
        assert!(err.contains("Unknown Page method"));
    }

    #[tokio::test]
    async fn print_to_pdf_returns_descriptive_unsupported_error() {
        // Regression for #53: Page.printToPDF must be handled explicitly so
        // Playwright clients receive a descriptive error rather than the
        // generic "Unknown Page method" fallback.
        let mut ctx = CdpContext::new();
        let err = handle("printToPDF", &json!({}), &mut ctx, &None)
            .await
            .expect_err("printToPDF must error until a real renderer exists");
        assert!(
            !err.contains("Unknown Page method"),
            "printToPDF must NOT fall through to the catch-all: {err}"
        );
        assert!(
            err.contains("not supported by Obscura"),
            "error must clearly state PDF is unsupported: {err}"
        );
        // Direct user to a workaround so the message is actionable.
        assert!(
            err.to_lowercase().contains("evaluate")
                || err.to_lowercase().contains("html"),
            "error must point to a workaround: {err}"
        );
    }

    /// Regression for #45: same idea as printToPDF for captureScreenshot.
    /// Playwright's `page.screenshot()` calls Page.captureScreenshot via CDP;
    /// without an explicit arm, clients see "Unknown Page method" and have
    /// no idea why their screenshot request failed.
    #[tokio::test]
    async fn capture_screenshot_returns_descriptive_unsupported_error() {
        let mut ctx = CdpContext::new();
        let err = handle("captureScreenshot", &json!({}), &mut ctx, &None)
            .await
            .expect_err("captureScreenshot must error until a real paint exists");
        assert!(
            !err.contains("Unknown Page method"),
            "captureScreenshot must NOT fall through to the catch-all: {err}"
        );
        assert!(
            err.contains("not supported by Obscura"),
            "error must clearly state screenshot is unsupported: {err}"
        );
        // Same for the MHTML snapshot sibling method.
        let err2 = handle("captureSnapshot", &json!({}), &mut ctx, &None)
            .await
            .expect_err("captureSnapshot must error until a real renderer exists");
        assert!(
            !err2.contains("Unknown Page method"),
            "captureSnapshot must NOT fall through: {err2}"
        );
    }

    #[tokio::test]
    async fn navigation_emits_target_info_changed_with_url_and_title() {
        // Strict CDP clients (browser-use, Puppeteer/Playwright `page.url()`)
        // refresh a target's url/title only on Target.targetInfoChanged. A
        // navigation must emit it with the post-nav url/title, otherwise those
        // clients stay stuck on the pre-nav about:blank.
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{}-session", page_id);
        ctx.sessions.insert(session_id.clone(), page_id.clone());

        let params = json!({
            "url": "data:text/html,<title>Hello</title><button>B</button>",
            "waitUntil": "load",
        });
        handle("navigate", &params, &mut ctx, &Some(session_id.clone()))
            .await
            .expect("navigate should succeed");

        let evt = ctx
            .pending_events
            .iter()
            .find(|e| e.method == "Target.targetInfoChanged")
            .expect("navigation must emit Target.targetInfoChanged");
        // Browser-level event (no sessionId) so the root connection's
        // targetInfoChanged handler receives it.
        assert!(
            evt.session_id.is_none(),
            "targetInfoChanged must be browser-level (no sessionId)"
        );
        let info = evt.params["targetInfo"].clone();

        // The payload must carry the live post-navigation url/title and the
        // canAccessOpener field strict clients require on every TargetInfo.
        let (exp_url, exp_title) = {
            let page = ctx.get_page(&page_id).expect("page exists");
            (page.url_string(), page.title.clone())
        };
        assert_eq!(info["targetId"], json!(page_id));
        assert_eq!(info["type"], "page");
        assert_eq!(info["url"], json!(exp_url));
        assert_eq!(info["title"], json!(exp_title));
        assert!(
            info["url"].as_str().unwrap_or_default().starts_with("data:"),
            "url should reflect the navigated page, got {}",
            info["url"]
        );
        assert_eq!(info["canAccessOpener"], json!(false));
    }
}
