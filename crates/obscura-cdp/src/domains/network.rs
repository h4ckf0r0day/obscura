use serde_json::{json, Value};

use crate::cookie_params::{parse_cdp_cookie, parse_delete_cookies_params};
use crate::dispatch::CdpContext;

const SESSION_COOKIE_EXPIRES: i64 = -1;
const DEFAULT_SECURE_PORT: u16 = 443;
const DEFAULT_INSECURE_PORT: u16 = 80;
const SOURCE_SCHEME_SECURE: &str = "Secure";
const SOURCE_SCHEME_NONSECURE: &str = "NonSecure";
const DEFAULT_SAME_SITE: &str = "Lax";

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
        "getCookies" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let cookies = page.context.cookie_jar.get_all_cookies();
            let cdp_cookies: Vec<Value> = cookies.iter().map(cookie_info_to_cdp_json).collect();
            Ok(json!({ "cookies": cdp_cookies }))
        }
        "setCookie" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            let cookie = parse_cdp_cookie(params)
                .ok_or("setCookie: missing required name/domain (or url)")?;
            page.context.cookie_jar.set_cookies_from_cdp(vec![cookie]);
            Ok(json!({ "success": true }))
        }
        "setCookies" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            if let Some(cookies) = params.get("cookies").and_then(|v| v.as_array()) {
                let parsed: Vec<_> = cookies.iter().filter_map(parse_cdp_cookie).collect();
                page.context.cookie_jar.set_cookies_from_cdp(parsed);
            }
            Ok(json!({}))
        }
        "deleteCookies" => {
            let page = ctx.get_session_page(session_id).ok_or("No page")?;
            if let Some(filter) = parse_delete_cookies_params(params) {
                page.context.cookie_jar.delete_cookies_filtered(
                    &filter.name,
                    &filter.domain,
                    filter.path.as_deref(),
                );
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
