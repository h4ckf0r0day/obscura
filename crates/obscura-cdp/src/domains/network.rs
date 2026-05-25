use std::sync::Arc;

use serde_json::{json, Value};

use obscura_net::CookieJar;

use crate::cookie_params::{parse_cdp_cookie, parse_delete_cookies_params};
use crate::dispatch::CdpContext;

const SESSION_COOKIE_EXPIRES: i64 = -1;
const DEFAULT_SECURE_PORT: u16 = 443;
const DEFAULT_INSECURE_PORT: u16 = 80;
const SOURCE_SCHEME_SECURE: &str = "Secure";
const SOURCE_SCHEME_NONSECURE: &str = "NonSecure";
const DEFAULT_SAME_SITE: &str = "Lax";

// Resolve the cookie jar for a Network request: prefer the session's page jar,
// fall back to the default browser context. Puppeteer and Playwright both call
// Network.setCookie/getCookies/deleteCookies BEFORE attaching to a target —
// requiring a session would break those flows (Storage.* already mirrors this).
fn cookie_jar_for<'a>(ctx: &'a CdpContext, session_id: &Option<String>) -> &'a Arc<CookieJar> {
    ctx.get_session_page(session_id)
        .map(|p| &p.context.cookie_jar)
        .unwrap_or(&ctx.default_context.cookie_jar)
}

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
            let ua = params.get("userAgent").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(page) = ctx.get_session_page(session_id) {
                page.http_client.set_user_agent(ua).await;
            }
            Ok(json!({}))
        }
        "getCookies" | "getAllCookies" => {
            let cookies = cookie_jar_for(ctx, session_id).get_all_cookies();
            let cdp_cookies: Vec<Value> = cookies.iter().map(cookie_info_to_cdp_json).collect();
            Ok(json!({ "cookies": cdp_cookies }))
        }
        "setCookie" => {
            let cookie = parse_cdp_cookie(params)
                .ok_or("setCookie: missing required name/domain (or url)")?;
            cookie_jar_for(ctx, session_id).set_cookies_from_cdp(vec![cookie]);
            Ok(json!({ "success": true }))
        }
        "setCookies" => {
            if let Some(cookies) = params.get("cookies").and_then(|v| v.as_array()) {
                let parsed: Vec<_> = cookies.iter().filter_map(parse_cdp_cookie).collect();
                cookie_jar_for(ctx, session_id).set_cookies_from_cdp(parsed);
            }
            Ok(json!({}))
        }
        "deleteCookies" => {
            if let Some(filter) = parse_delete_cookies_params(params) {
                cookie_jar_for(ctx, session_id).delete_cookies_filtered(
                    &filter.name,
                    &filter.domain,
                    filter.path.as_deref(),
                );
            }
            Ok(json!({}))
        }
        "clearBrowserCookies" => {
            cookie_jar_for(ctx, session_id).clear();
            Ok(json!({}))
        }
        "setCacheDisabled" => Ok(json!({})),
        "setRequestInterception" => Ok(json!({})),
        _ => Err(format!("Unknown Network method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_net::CookieInfo;

    fn sample_cookie(name: &str) -> CookieInfo {
        CookieInfo {
            name: name.to_string(),
            value: "v".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            same_site: String::new(),
            expires: None,
        }
    }

    #[tokio::test]
    async fn set_cookie_without_session_targets_default_context() {
        let mut ctx = CdpContext::new();
        let params = json!({
            "name": "sid",
            "value": "abc",
            "domain": "example.com",
            "path": "/"
        });
        let resp = handle("setCookie", &params, &mut ctx, &None)
            .await
            .expect("setCookie must succeed without a session");
        assert_eq!(resp["success"], json!(true));
        let cookies = ctx.default_context.cookie_jar.get_all_cookies();
        assert_eq!(cookies.len(), 1, "default cookie jar must receive the cookie");
        assert_eq!(cookies[0].name, "sid");
    }

    #[tokio::test]
    async fn set_cookies_without_session_targets_default_context() {
        let mut ctx = CdpContext::new();
        let params = json!({
            "cookies": [
                { "name": "a", "value": "1", "domain": "example.com", "path": "/" },
                { "name": "b", "value": "2", "domain": "example.com", "path": "/" }
            ]
        });
        handle("setCookies", &params, &mut ctx, &None)
            .await
            .expect("setCookies must succeed without a session");
        assert_eq!(ctx.default_context.cookie_jar.get_all_cookies().len(), 2);
    }

    #[tokio::test]
    async fn delete_cookies_without_session_targets_default_context() {
        let mut ctx = CdpContext::new();
        ctx.default_context
            .cookie_jar
            .set_cookies_from_cdp(vec![sample_cookie("sid")]);
        let params = json!({ "name": "sid", "domain": "example.com" });
        handle("deleteCookies", &params, &mut ctx, &None)
            .await
            .expect("deleteCookies must succeed without a session");
        assert!(ctx.default_context.cookie_jar.get_all_cookies().is_empty());
    }

    #[tokio::test]
    async fn get_all_cookies_returns_every_cookie_in_jar() {
        let mut ctx = CdpContext::new();
        ctx.default_context.cookie_jar.set_cookies_from_cdp(vec![
            sample_cookie("a"),
            sample_cookie("b"),
        ]);
        let resp = handle("getAllCookies", &json!({}), &mut ctx, &None)
            .await
            .expect("getAllCookies must succeed");
        let arr = resp["cookies"].as_array().expect("cookies array");
        assert_eq!(arr.len(), 2);
    }

    #[tokio::test]
    async fn get_cookies_falls_back_to_default_context_when_no_session() {
        let mut ctx = CdpContext::new();
        ctx.default_context
            .cookie_jar
            .set_cookies_from_cdp(vec![sample_cookie("sid")]);
        let resp = handle("getCookies", &json!({}), &mut ctx, &None)
            .await
            .expect("getCookies must succeed without a session");
        let arr = resp["cookies"].as_array().expect("cookies array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "sid");
    }

    #[tokio::test]
    async fn clear_browser_cookies_without_session_clears_default_context() {
        let mut ctx = CdpContext::new();
        ctx.default_context
            .cookie_jar
            .set_cookies_from_cdp(vec![sample_cookie("sid")]);
        handle("clearBrowserCookies", &json!({}), &mut ctx, &None)
            .await
            .expect("clearBrowserCookies must succeed");
        assert!(ctx.default_context.cookie_jar.get_all_cookies().is_empty());
    }
}

pub(crate) fn cookie_info_to_cdp_json(c: &obscura_net::CookieInfo) -> Value {
    let expires = c.expires.unwrap_or(SESSION_COOKIE_EXPIRES);
    let session = c.expires.is_none();
    let same_site = if c.same_site.is_empty() {
        DEFAULT_SAME_SITE
    } else {
        c.same_site.as_str()
    };
    json!({
        "name": c.name,
        "value": c.value,
        "domain": c.domain,
        "path": c.path,
        "expires": expires,
        "size": c.name.len() + c.value.len(),
        "httpOnly": c.http_only,
        "secure": c.secure,
        "session": session,
        "sameSite": same_site,
        "sameParty": false,
        "sourceScheme": if c.secure { SOURCE_SCHEME_SECURE } else { SOURCE_SCHEME_NONSECURE },
        "sourcePort": if c.secure { DEFAULT_SECURE_PORT } else { DEFAULT_INSECURE_PORT },
        "priority": "Medium",
    })
}
