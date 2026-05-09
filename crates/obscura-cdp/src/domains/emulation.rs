use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "setDeviceMetricsOverride" => {
            let width = params
                .get("width")
                .and_then(|v| v.as_u64())
                .unwrap_or(1920)
                .min(u32::MAX as u64) as u32;
            let height = params
                .get("height")
                .and_then(|v| v.as_u64())
                .unwrap_or(1000)
                .min(u32::MAX as u64) as u32;
            let device_scale_factor = params
                .get("deviceScaleFactor")
                .and_then(|v| v.as_f64())
                .unwrap_or(2.0);

            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.set_viewport_metrics(width, height, device_scale_factor);
            }

            Ok(json!({}))
        }
        "clearDeviceMetricsOverride" => {
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.set_viewport_metrics(1920, 1000, 2.0);
            }
            Ok(json!({}))
        }
        "setUserAgentOverride" => {
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                let mut emulated_headers = std::collections::HashMap::new();

                if let Some(raw_ua) = params.get("userAgent").and_then(|v| v.as_str()) {
                    let ua = if raw_ua.trim().is_empty() {
                        obscura_net::DEFAULT_USER_AGENT
                    } else {
                        raw_ua
                    };
                    page.http_client.set_user_agent(ua).await;
                    emulated_headers.insert("user-agent".to_string(), ua.to_string());
                    if let Some((sec_ch_ua, full_version_list)) = client_hints_for_user_agent(ua) {
                        emulated_headers.insert("sec-ch-ua".to_string(), sec_ch_ua);
                        emulated_headers
                            .insert("sec-ch-ua-full-version-list".to_string(), full_version_list);
                    }
                }

                if let Some(accept_language) = params.get("acceptLanguage").and_then(|v| v.as_str())
                {
                    emulated_headers
                        .insert("accept-language".to_string(), accept_language.to_string());
                }

                if let Some(metadata) = params.get("userAgentMetadata").and_then(|v| v.as_object())
                {
                    if let Some(platform) = metadata.get("platform").and_then(|v| v.as_str()) {
                        emulated_headers.insert(
                            "sec-ch-ua-platform".to_string(),
                            format!("\"{}\"", platform),
                        );
                    }
                    if let Some(platform_version) =
                        metadata.get("platformVersion").and_then(|v| v.as_str())
                    {
                        emulated_headers.insert(
                            "sec-ch-ua-platform-version".to_string(),
                            format!("\"{}\"", platform_version),
                        );
                    }
                }

                if !emulated_headers.is_empty() {
                    page.http_client
                        .extra_headers
                        .write()
                        .await
                        .extend(emulated_headers.clone());

                    #[cfg(feature = "stealth")]
                    if let Some(stealth) = &page.stealth_client {
                        stealth.extra_headers.write().await.extend(emulated_headers);
                    }
                }

                page.set_user_agent_override(params.clone());
            }

            Ok(json!({}))
        }
        "setTouchEmulationEnabled"
        | "setEmulatedMedia"
        | "setTimezoneOverride"
        | "setLocaleOverride"
        | "setCPUThrottlingRate"
        | "setScriptExecutionDisabled"
        | "setFocusEmulationEnabled"
        | "setScrollbarsHidden"
        | "setDefaultBackgroundColorOverride" => Ok(json!({})),
        _ => Err(format!("Unknown Emulation method: {}", method)),
    }
}

fn client_hints_for_user_agent(ua: &str) -> Option<(String, String)> {
    let full_version = ua
        .split("Chrome/")
        .nth(1)
        .or_else(|| ua.split("Chromium/").nth(1))?
        .split_whitespace()
        .next()
        .unwrap_or("124.0.0.0");
    let major = full_version.split('.').next().unwrap_or("124");
    Some((
        format!(
            "\"Google Chrome\";v=\"{}\", \"Not.A/Brand\";v=\"8\", \"Chromium\";v=\"{}\"",
            major, major
        ),
        format!(
            "\"Google Chrome\";v=\"{}\", \"Not.A/Brand\";v=\"8.0.0.0\", \"Chromium\";v=\"{}\"",
            full_version, full_version
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_user_agent_override_accepts_playwright_context_params() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert("session-1".to_string(), page_id);

        let result = handle(
            "setUserAgentOverride",
            &json!({
                "userAgent": "Playwright-Test-UA/1.0",
                "acceptLanguage": "en-US",
                "platform": "MacIntel",
                "userAgentMetadata": {
                    "platform": "macOS",
                    "platformVersion": "14.0.0"
                }
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("setUserAgentOverride should be accepted");

        assert_eq!(result, json!({}));

        let page = ctx
            .get_session_page(&session_id)
            .expect("session should still point at the page");
        assert_eq!(
            page.http_client.user_agent.read().await.as_str(),
            "Playwright-Test-UA/1.0"
        );
        let headers = page.http_client.extra_headers.read().await;
        assert_eq!(
            headers.get("accept-language").map(String::as_str),
            Some("en-US")
        );
        assert_eq!(
            headers.get("sec-ch-ua").map(String::as_str),
            None,
            "non-Chrome custom UAs should not fabricate client hints"
        );
        assert_eq!(
            headers.get("sec-ch-ua-platform").map(String::as_str),
            Some("\"macOS\"")
        );
        assert!(page.user_agent_override.is_some());
    }

    #[tokio::test]
    async fn set_user_agent_override_uses_default_for_blank_user_agent() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert("session-1".to_string(), page_id);

        handle(
            "setUserAgentOverride",
            &json!({
                "userAgent": "",
                "acceptLanguage": "en-US,en;q=0.9"
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("blank setUserAgentOverride should be accepted");

        let page = ctx.get_session_page(&session_id).unwrap();
        assert_eq!(
            page.http_client.user_agent.read().await.as_str(),
            obscura_net::DEFAULT_USER_AGENT
        );
        let headers = page.http_client.extra_headers.read().await;
        assert_eq!(
            headers.get("user-agent").map(String::as_str),
            Some(obscura_net::DEFAULT_USER_AGENT)
        );
    }
}
