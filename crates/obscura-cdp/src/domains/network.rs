use serde_json::{json, Value};

use crate::dispatch::CdpContext;

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "enable" => {
            let tx_clone = ctx.intercept_tx.clone();
            if let (Some(tx), Some(page)) = (tx_clone, ctx.get_session_page_mut(session_id)) {
                page.set_network_event_tx(tx);
            }
            Ok(json!({}))
        }
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
                .unwrap_or(obscura_net::DEFAULT_USER_AGENT);
            let ua = if ua.trim().is_empty() {
                obscura_net::DEFAULT_USER_AGENT
            } else {
                ua
            };
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.http_client.set_user_agent(ua).await;
                if let Some(js) = &mut page.js {
                    js.set_user_agent(ua);
                }
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
                    .filter_map(cdp_cookie_to_cookie_info)
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
        "getResponseBody" => {
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .ok_or("requestId required")?;
            let bodies = ctx.network_response_bodies.lock().await;
            let body = bodies.get(request_id).ok_or_else(|| {
                format!("No resource with given identifier found: {}", request_id)
            })?;
            Ok(json!({
                "body": body.body.clone(),
                "base64Encoded": body.base64_encoded,
            }))
        }
        _ => Err(format!("Unknown Network method: {}", method)),
    }
}

fn cdp_cookie_to_cookie_info(c: &Value) -> Option<obscura_net::CookieInfo> {
    let name = c.get("name")?.as_str()?.to_string();
    let value = c.get("value")?.as_str()?.to_string();
    let mut domain = c
        .get("domain")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let mut path = c
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("/")
        .to_string();
    let mut secure = c.get("secure").and_then(|v| v.as_bool()).unwrap_or(false);

    if domain.is_empty() {
        let url = c.get("url").and_then(|v| v.as_str())?;
        let parsed = url::Url::parse(url).ok()?;
        domain = parsed.host_str()?.to_ascii_lowercase();
        if c.get("path").is_none() {
            path = default_cookie_path(parsed.path());
        }
        secure = secure || parsed.scheme() == "https";
    }

    if domain.is_empty() {
        return None;
    }

    Some(obscura_net::CookieInfo {
        name,
        value,
        domain,
        path,
        secure,
        http_only: c.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

fn default_cookie_path(url_path: &str) -> String {
    if !url_path.starts_with('/') || url_path == "/" {
        return "/".to_string();
    }
    match url_path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => url_path[..index].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_cookies_accepts_cdp_cookie_with_url_only() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert("session-1".to_string(), page_id);

        handle(
            "setCookies",
            &json!({
                "cookies": [{
                    "name": "sessionid",
                    "value": "abc",
                    "url": "https://www.instagram.com/annaSihombing96067/",
                    "path": "/",
                    "secure": true,
                    "httpOnly": true
                }]
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("Network.setCookies should accept url-scoped cookies");

        let page = ctx.get_session_page(&session_id).unwrap();
        let url = url::Url::parse("https://www.instagram.com/graphql/query/").unwrap();
        let header = page.context.cookie_jar.get_cookie_header(&url);
        assert!(header.contains("sessionid=abc"));
    }

    #[tokio::test]
    async fn enable_attaches_network_events_without_fetch_pause() {
        let mut ctx = CdpContext::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        ctx.intercept_tx = Some(tx);
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert("session-1".to_string(), page_id);

        handle("enable", &json!({}), &mut ctx, &session_id)
            .await
            .expect("Network.enable should succeed");

        let page = ctx.get_session_page(&session_id).unwrap();
        assert!(
            !page.intercept_enabled,
            "Network.enable should surface Network events without turning on Fetch.requestPaused"
        );
    }

    #[tokio::test]
    async fn get_response_body_returns_cached_body_by_request_id() {
        let mut ctx = CdpContext::new();
        ctx.network_response_bodies.lock().await.insert(
            "request-1".to_string(),
            crate::dispatch::NetworkResponseBody {
                body: "{\"ok\":true}".to_string(),
                base64_encoded: false,
            },
        );

        let result = handle(
            "getResponseBody",
            &json!({"requestId": "request-1"}),
            &mut ctx,
            &None,
        )
        .await
        .expect("Network.getResponseBody should return cached bodies");

        assert_eq!(
            result,
            json!({
                "body": "{\"ok\":true}",
                "base64Encoded": false,
            })
        );
    }

    #[tokio::test]
    async fn set_user_agent_override_uses_default_for_blank_value() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert("session-1".to_string(), page_id);

        handle(
            "setUserAgentOverride",
            &json!({"userAgent": ""}),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("Network.setUserAgentOverride should accept blank UA");

        let page = ctx.get_session_page(&session_id).unwrap();
        assert_eq!(
            page.http_client.user_agent.read().await.as_str(),
            obscura_net::DEFAULT_USER_AGENT
        );
    }
}
