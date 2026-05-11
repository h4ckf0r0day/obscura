use obscura_browser::lifecycle::WaitUntil;
use serde_json::{json, Value};

use crate::dispatch::CdpContext;
use crate::types::CdpEvent;

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

            let wait_until = params.get("waitUntil")
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
                .unwrap_or(WaitUntil::Load);

            let preload_scripts: Vec<String> = ctx.preload_scripts.iter().map(|(_, s)| s.clone()).collect();

            let (frame_id, loader_id, network_events, page_url, page_id, reached_network_idle) = {
                let page = ctx.get_session_page_mut(session_id).ok_or("No page for session")?;
                let frame_id = page.frame_id.clone();
                let loader_id = format!("loader-{}", uuid::Uuid::new_v4());

                let nav_method = params.get("__method").and_then(|v| v.as_str()).unwrap_or("GET");
                let nav_body = params.get("__body").and_then(|v| v.as_str()).unwrap_or("");
                if nav_method == "POST" && !nav_body.is_empty() {
                    page.navigate_with_wait_post(url, wait_until, nav_method, nav_body).await.map_err(|e| e.to_string())?;
                } else {
                    page.navigate_with_wait(url, wait_until).await.map_err(|e| e.to_string())?;
                }

                for source in &preload_scripts {
                    if let Err(e) = page.execute_preload_script(source) {
                        tracing::debug!("Preload script error: {}", e);
                    }
                }

                let reached_network_idle = page.lifecycle.is_network_idle();
                let network_events: Vec<_> = page.network_events.drain(..).collect();
                let page_url = page.url_string();
                let page_id = page.id.clone();
                (frame_id, loader_id, network_events, page_url, page_id, reached_network_idle)
            };

            let es = session_id.clone();
            let ts = timestamp();

            let mut phase1 = vec![
                CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "init", "timestamp": ts}), session_id: es.clone() },
                CdpEvent { method: "Runtime.executionContextsCleared".into(), params: json!({}), session_id: es.clone() },
                CdpEvent { method: "Page.frameNavigated".into(), params: json!({"frame": {"id": frame_id, "loaderId": loader_id, "url": page_url, "domainAndRegistry": "", "securityOrigin": page_url, "mimeType": "text/html", "adFrameStatus": {"adFrameType": "none"}}, "type": "Navigation"}), session_id: es.clone() },
                CdpEvent { method: "Runtime.executionContextCreated".into(), params: json!({"context": {"id": 2, "origin": page_url, "name": "", "uniqueId": format!("ctx-nav-{}", page_id), "auxData": {"isDefault": true, "type": "default", "frameId": frame_id}}}), session_id: es.clone() },
            ];
            // Re-emit each isolated world the client previously registered
            // via Page.createIsolatedWorld. Without this, Playwright's
            // utility-world handle becomes stale after navigation and
            // every subsequent evaluate() (including page.title()) hangs.
            // Fallback to the legacy hardcoded Puppeteer name so older
            // Puppeteer clients that don't call createIsolatedWorld
            // continue to work.
            let world_names: Vec<String> = if ctx.isolated_worlds.is_empty() {
                vec!["__puppeteer_utility_world__24.40.0".to_string()]
            } else {
                ctx.isolated_worlds.clone()
            };
            for (idx, world_name) in world_names.iter().enumerate() {
                let world_ctx_id = 100 + idx as u32;
                phase1.push(CdpEvent {
                    method: "Runtime.executionContextCreated".into(),
                    params: json!({"context": {"id": world_ctx_id, "origin": page_url, "name": world_name, "uniqueId": format!("ctx-isolated-nav-{}-{}", page_id, idx), "auxData": {"isDefault": false, "type": "isolated", "frameId": frame_id}}}),
                    session_id: es.clone(),
                });
            }
            phase1.push(CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "commit", "timestamp": ts}), session_id: es.clone() });
            ctx.pending_events.extend(phase1);

            if ctx.fetch_intercept.enabled {
                for net_event in &network_events {
                    ctx.pending_events.push(CdpEvent {
                        method: "Fetch.requestPaused".into(),
                        params: json!({
                            "requestId": net_event.request_id,
                            "request": {
                                "url": net_event.url,
                                "method": net_event.method,
                                "headers": net_event.headers,
                            },
                            "frameId": frame_id,
                            "resourceType": net_event.resource_type,
                            "networkId": net_event.request_id,
                        }),
                        session_id: es.clone(),
                    });
                }
            }

            for net_event in &network_events {
                ctx.pending_events.push(CdpEvent {
                    method: "Network.requestWillBeSent".into(),
                    params: json!({"requestId": net_event.request_id, "loaderId": loader_id, "documentURL": page_url, "request": {"url": net_event.url, "method": net_event.method, "headers": net_event.headers}, "timestamp": net_event.timestamp, "wallTime": net_event.timestamp, "initiator": {"type": "other"}, "type": net_event.resource_type, "frameId": frame_id}),
                    session_id: es.clone(),
                });
                ctx.pending_events.push(CdpEvent {
                    method: "Network.responseReceived".into(),
                    params: json!({"requestId": net_event.request_id, "loaderId": loader_id, "timestamp": net_event.timestamp, "type": net_event.resource_type, "response": {"url": net_event.url, "status": net_event.status, "statusText": "", "headers": &*net_event.response_headers, "mimeType": net_event.response_headers.get("content-type").cloned().unwrap_or_default()}, "frameId": frame_id}),
                    session_id: es.clone(),
                });
                ctx.pending_events.push(CdpEvent {
                    method: "Network.loadingFinished".into(),
                    params: json!({"requestId": net_event.request_id, "timestamp": net_event.timestamp, "encodedDataLength": net_event.body_size}),
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

            Ok(json!({
                "frameId": frame_id,
                "loaderId": loader_id,
            }))
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
            let page = ctx.get_session_page(session_id).ok_or("No page for session")?;
            let frame_id_param = params.get("frameId").and_then(|v| v.as_str())
                .unwrap_or(&page.frame_id).to_string();
            let world_name = params.get("worldName").and_then(|v| v.as_str())
                .unwrap_or("").to_string();
            let page_url = page.url_string();
            let page_id = page.id.clone();
            let context_id = 100;
            // Track this world so Page.navigate can re-emit a context for it
            // post-navigation. Without this, Playwright (and Puppeteer)
            // hang in any operation that uses the utility world — including
            // page.title() — because their utility world is gone after
            // Runtime.executionContextsCleared and never re-created.
            if !world_name.is_empty() && !ctx.isolated_worlds.contains(&world_name) {
                ctx.isolated_worlds.push(world_name.clone());
            }

            ctx.pending_events.push(CdpEvent {
                method: "Runtime.executionContextCreated".to_string(),
                params: json!({
                    "context": {
                        "id": context_id,
                        "origin": page_url,
                        "name": world_name,
                        "uniqueId": format!("ctx-isolated-{}", page_id),
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
            Ok(json!({
                "currentIndex": 0,
                "entries": [{
                    "id": 0,
                    "url": page.url_string(),
                    "userTypedURL": page.url_string(),
                    "title": page.title,
                    "transitionType": "typed",
                }]
            }))
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
}
