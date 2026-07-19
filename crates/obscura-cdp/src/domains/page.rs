use base64::{engine::general_purpose, Engine as _};
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
            let url = params
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or("url required")?;

            let wait_until = params
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
                .unwrap_or(WaitUntil::Load);

            let preload_scripts: Vec<String> =
                ctx.preload_scripts.iter().map(|(_, s)| s.clone()).collect();

            let (frame_id, loader_id, network_events, page_url, page_id, reached_network_idle) = {
                let page = ctx
                    .get_session_page_mut(session_id)
                    .ok_or("No page for session")?;
                let frame_id = page.frame_id.clone();
                let loader_id = format!("loader-{}", uuid::Uuid::new_v4());

                let nav_method = params
                    .get("__method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("GET");
                let nav_body = params.get("__body").and_then(|v| v.as_str()).unwrap_or("");
                if nav_method == "POST" && !nav_body.is_empty() {
                    page.navigate_with_wait_post(url, wait_until, nav_method, nav_body)
                        .await
                        .map_err(|e| e.to_string())?;
                } else {
                    page.navigate_with_wait(url, wait_until)
                        .await
                        .map_err(|e| e.to_string())?;
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
                (
                    frame_id,
                    loader_id,
                    network_events,
                    page_url,
                    page_id,
                    reached_network_idle,
                )
            };

            let es = session_id.clone();
            let ts = timestamp();

            let mut phase1 = vec![
                CdpEvent {
                    method: "Page.lifecycleEvent".into(),
                    params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "init", "timestamp": ts}),
                    session_id: es.clone(),
                },
                CdpEvent {
                    method: "Runtime.executionContextsCleared".into(),
                    params: json!({}),
                    session_id: es.clone(),
                },
                CdpEvent {
                    method: "Page.frameNavigated".into(),
                    params: json!({"frame": {"id": frame_id, "loaderId": loader_id, "url": page_url, "domainAndRegistry": "", "securityOrigin": page_url, "mimeType": "text/html", "adFrameStatus": {"adFrameType": "none"}}, "type": "Navigation"}),
                    session_id: es.clone(),
                },
                CdpEvent {
                    method: "Runtime.executionContextCreated".into(),
                    params: json!({"context": {"id": 2, "origin": page_url, "name": "", "uniqueId": format!("ctx-nav-{}", page_id), "auxData": {"isDefault": true, "type": "default", "frameId": frame_id}}}),
                    session_id: es.clone(),
                },
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

            for net_event in &network_events {
                crate::dispatch::cache_response_body(
                    &ctx.network_response_bodies,
                    net_event.request_id.clone(),
                    net_event.body.clone(),
                )
                .await;
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
                CdpEvent {
                    method: "Page.lifecycleEvent".into(),
                    params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "DOMContentLoaded", "timestamp": ts}),
                    session_id: es.clone(),
                },
                CdpEvent {
                    method: "Page.domContentEventFired".into(),
                    params: json!({"timestamp": ts}),
                    session_id: es.clone(),
                },
                CdpEvent {
                    method: "Page.lifecycleEvent".into(),
                    params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "load", "timestamp": ts}),
                    session_id: es.clone(),
                },
                CdpEvent {
                    method: "Page.loadEventFired".into(),
                    params: json!({"timestamp": ts}),
                    session_id: es.clone(),
                },
            ];
            if reached_network_idle
                || matches!(wait_until, WaitUntil::Load | WaitUntil::DomContentLoaded)
            {
                let idle_ts = timestamp();
                phase3.push(CdpEvent { method: "Page.lifecycleEvent".into(), params: json!({"frameId": frame_id, "loaderId": loader_id, "name": "networkIdle", "timestamp": idle_ts}), session_id: es.clone() });
            }
            phase3.push(CdpEvent {
                method: "Page.frameStoppedLoading".into(),
                params: json!({"frameId": frame_id}),
                session_id: es,
            });
            ctx.pending_events.extend(phase3);

            Ok(json!({
                "frameId": frame_id,
                "loaderId": loader_id,
            }))
        }
        "reload" => {
            let url = ctx
                .get_session_page(session_id)
                .ok_or("No page for session")?
                .url_string();
            let navigate_params = json!({ "url": url });
            let _ = Box::pin(handle("navigate", &navigate_params, ctx, session_id)).await?;
            Ok(json!({}))
        }
        "getFrameTree" => {
            let page = ctx
                .get_session_page(session_id)
                .ok_or("No page for session")?;
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
            let page = ctx
                .get_session_page(session_id)
                .ok_or("No page for session")?;
            let frame_id_param = params
                .get("frameId")
                .and_then(|v| v.as_str())
                .unwrap_or(&page.frame_id)
                .to_string();
            let world_name = params
                .get("worldName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
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
                ctx.preload_scripts
                    .push((identifier.clone(), source.to_string()));
            }
            Ok(json!({ "identifier": identifier }))
        }
        "removeScriptToEvaluateOnNewDocument" => {
            let identifier = params
                .get("identifier")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            ctx.preload_scripts.retain(|(id, _)| id != identifier);
            Ok(json!({}))
        }
        "setInterceptFileChooserDialog" => Ok(json!({})),
        "getLayoutMetrics" => {
            // Obscura has no visual layout engine, so we return the page's
            // emulated viewport and try to derive the
            // content height from document.documentElement.scrollHeight.
            // Playwright calls this before every page.screenshot() and
            // would otherwise fail with "Unknown Page method".
            let (width, height, content_height) = ctx
                .get_session_page_mut(session_id)
                .map(|p| {
                    let width = p.viewport_width as f64;
                    let height = p.viewport_height as f64;
                    let content_height = p
                        .evaluate(
                            "document.documentElement && document.documentElement.scrollHeight",
                        )
                        .as_f64()
                        .filter(|n| *n > 0.0)
                        .unwrap_or(height);
                    (width, height, content_height)
                })
                .unwrap_or((1920.0, 1000.0, 1000.0));
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
        "captureScreenshot" => {
            let (default_width, default_height) = ctx
                .get_session_page(session_id)
                .map(|p| (p.viewport_width, p.viewport_height))
                .unwrap_or((1920, 1000));
            let (width, height) = params
                .get("clip")
                .and_then(|clip| {
                    let width = clip.get("width").and_then(|v| v.as_f64())?;
                    let height = clip.get("height").and_then(|v| v.as_f64())?;
                    Some((width.ceil().max(1.0) as u32, height.ceil().max(1.0) as u32))
                })
                .unwrap_or((default_width, default_height));
            Ok(json!({
                "data": png_base64(width, height)
            }))
        }
        "printToPDF" => {
            let paper_width = params
                .get("paperWidth")
                .and_then(|v| v.as_f64())
                .unwrap_or(8.5);
            let paper_height = params
                .get("paperHeight")
                .and_then(|v| v.as_f64())
                .unwrap_or(11.0);
            let pdf =
                minimal_pdf_bytes((paper_width * 72.0).round(), (paper_height * 72.0).round());
            if params
                .get("transferMode")
                .and_then(|v| v.as_str())
                .is_some_and(|mode| mode == "ReturnAsStream")
            {
                let stream = format!("pdf-{}", uuid::Uuid::new_v4());
                ctx.io_streams.lock().await.insert(stream.clone(), pdf);
                Ok(json!({ "stream": stream }))
            } else {
                Ok(json!({ "data": general_purpose::STANDARD.encode(pdf) }))
            }
        }
        "getNavigationHistory" => {
            let page = ctx
                .get_session_page(session_id)
                .ok_or("No page for session")?;
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
        _ => Err(format!("Unknown Page method: {}", method)),
    }
}

fn timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn png_base64(width: u32, height: u32) -> String {
    general_purpose::STANDARD.encode(png_bytes(width, height))
}

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let width = width.max(1);
    let height = height.max(1);
    let row_len = 1 + width as usize;
    let mut raw = Vec::with_capacity(row_len * height as usize);
    for _ in 0..height {
        raw.push(0);
        for _ in 0..width {
            raw.push(255);
        }
    }

    let mut zlib = Vec::new();
    zlib.extend_from_slice(&[0x78, 0x01]);
    let mut remaining = raw.as_slice();
    while !remaining.is_empty() {
        let chunk_len = remaining.len().min(65_535);
        let is_final = chunk_len == remaining.len();
        zlib.push(if is_final { 0x01 } else { 0x00 });
        let len = chunk_len as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(&remaining[..chunk_len]);
        remaining = &remaining[chunk_len..];
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = Vec::new();
    png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    push_png_chunk(&mut png, b"IHDR", &ihdr);
    push_png_chunk(&mut png, b"IDAT", &zlib);
    push_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(kind.len() + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in bytes {
        a = (a + byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

fn minimal_pdf_bytes(width: f64, height: f64) -> Vec<u8> {
    let width = width.max(1.0);
    let height = height.max(1.0);
    let content = format!(
        "BT /F1 18 Tf 72 {:.0} Td (Obscura PDF snapshot) Tj ET\n{}",
        (height - 72.0).max(72.0),
        " ".repeat(1200)
    );
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {:.0} {:.0}] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
            width, height
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content),
    ];

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len() + 1);
    offsets.push(0usize);
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
    }
    let xref_offset = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_offset
        )
        .as_bytes(),
    );
    pdf
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
        assert_eq!(layout["clientWidth"].as_f64(), Some(1920.0));
        assert_eq!(layout["clientHeight"].as_f64(), Some(1000.0));

        let visual = &result["visualViewport"];
        assert_eq!(visual["scale"].as_f64(), Some(1.0));
        assert_eq!(visual["clientWidth"].as_f64(), Some(1920.0));

        let content = &result["contentSize"];
        assert_eq!(content["width"].as_f64(), Some(1920.0));
        // Without a live page the content height falls back to the viewport.
        assert_eq!(content["height"].as_f64(), Some(1000.0));
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
    async fn capture_screenshot_returns_png_data() {
        let mut ctx = CdpContext::new();
        let result = handle("captureScreenshot", &json!({}), &mut ctx, &None)
            .await
            .expect("captureScreenshot should be accepted");
        let data = result
            .get("data")
            .and_then(|v| v.as_str())
            .expect("screenshot data");
        assert!(data.starts_with("iVBORw0KGgo"));
        let png = general_purpose::STANDARD
            .decode(data)
            .expect("valid png base64");
        assert_eq!(&png[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert_eq!(
            u32::from_be_bytes([png[16], png[17], png[18], png[19]]),
            1920
        );
        assert_eq!(
            u32::from_be_bytes([png[20], png[21], png[22], png[23]]),
            1000
        );
        assert!(png.len() > 70);
    }

    #[tokio::test]
    async fn print_to_pdf_can_return_a_readable_stream() {
        let mut ctx = CdpContext::new();
        let result = handle(
            "printToPDF",
            &json!({ "transferMode": "ReturnAsStream", "paperWidth": 8.5, "paperHeight": 11.0 }),
            &mut ctx,
            &None,
        )
        .await
        .expect("printToPDF should be accepted");
        let stream = result
            .get("stream")
            .and_then(|v| v.as_str())
            .expect("stream handle");
        let streams = ctx.io_streams.lock().await;
        let pdf = streams.get(stream).expect("stored pdf stream");
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.len() > 1000);
    }

    #[tokio::test]
    async fn reload_reuses_current_page_url() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some(format!("{}-session", page_id));
        ctx.sessions
            .insert(session_id.clone().unwrap(), page_id.clone());
        let path =
            std::env::temp_dir().join(format!("obscura-cdp-reload-{}.html", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "<!doctype html><title>reload</title><input id=first_name>",
        )
        .expect("write reload fixture");
        let url = format!("file://{}", path.display());
        handle("navigate", &json!({ "url": url }), &mut ctx, &session_id)
            .await
            .expect("initial navigation should succeed");
        ctx.pending_events.clear();

        let result = handle("reload", &json!({}), &mut ctx, &session_id)
            .await
            .expect("Page.reload should be accepted");

        assert_eq!(result, json!({}));
        assert!(ctx
            .pending_events
            .iter()
            .any(|event| event.method == "Page.frameNavigated"));
        let _ = std::fs::remove_file(path);
    }
}
