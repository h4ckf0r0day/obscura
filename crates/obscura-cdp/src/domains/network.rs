use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => Ok(json!({})),
        "setExtraHTTPHeaders" => {
            let headers = params.get("headers").and_then(|v| v.as_object());
            if let Some(page) = ctx.get_session_page(session_id) {
                if let Some(headers) = headers {
                    let header_map: std::collections::HashMap<String, String> = headers
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect();
                    page.http_client.set_extra_headers(header_map).await;
                }
            }
            Ok(json!({}))
        }
        "setUserAgentOverride" => {
            let ua = params
                .get("userAgent")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Some(page) = ctx.get_session_page(session_id) {
                page.http_client.set_user_agent(ua).await;
            }
            Ok(json!({}))
        }
        "getCookies" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let cookies = page.context.cookie_jar.get_all_cookies();
            let cdp_cookies: Vec<Value> = cookies
                .iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "value": c.value,
                        "domain": c.domain,
                        "path": c.path,
                        "expires": -1,
                        "size": c.name.len() + c.value.len(),
                        "httpOnly": c.http_only,
                        "secure": c.secure,
                        "session": true,
                        "sameParty": false,
                        "sourceScheme": "Secure",
                        "sourcePort": 443,
                    })
                })
                .collect();
            Ok(json!({ "cookies": cdp_cookies }))
        }
        "setCookies" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            if let Some(cookies) = params.get("cookies").and_then(|v| v.as_array()) {
                let cookie_infos: Vec<obscura_net::CookieInfo> = cookies
                    .iter()
                    .filter_map(|c| {
                        Some(obscura_net::CookieInfo {
                            name: c.get("name")?.as_str()?.to_string(),
                            value: c.get("value")?.as_str()?.to_string(),
                            domain: c
                                .get("domain")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            path: c
                                .get("path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("/")
                                .to_string(),
                            secure: c.get("secure").and_then(|v| v.as_bool()).unwrap_or(false),
                            http_only: c.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false),
                        })
                    })
                    .collect();
                page.context.cookie_jar.set_cookies_from_cdp(cookie_infos);
            }
            Ok(json!({}))
        }
        "clearBrowserCookies" => {
            if let Some(page) = ctx.get_session_page(session_id) {
                page.context.cookie_jar.clear();
            }
            Ok(json!({}))
        }
        "setCacheDisabled" => Ok(json!({})),
        "setRequestInterception" => Ok(json!({})),
        _ => Err(format!("Unknown Network method: {}", method)),
    }
}
