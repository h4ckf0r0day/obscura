use serde_json::{json, Value};

use crate::cookie_params::{parse_cdp_cookie, parse_delete_cookies_params};
use crate::dispatch::CdpContext;
use crate::domains::network::cookie_info_to_cdp_json;

fn cookie_jar_for(
    ctx: &CdpContext,
    params: &Value,
    session_id: &Option<String>,
) -> Result<std::sync::Arc<obscura_net::CookieJar>, String> {
    match params.get("browserContextId").and_then(|value| value.as_str()) {
        Some(id) => ctx
            .browser_context(id)
            .map(|context| context.cookie_jar.clone())
            .ok_or_else(|| format!("Browser context not found: {}", id)),
        None => Ok(ctx
            .get_session_page(session_id)
            .map(|page| page.context.cookie_jar.clone())
            .unwrap_or_else(|| ctx.default_context.cookie_jar.clone())),
    }
}

pub async fn handle(
    method: &str,
    params: &Value,
    ctx: &mut CdpContext,
    session_id: &Option<String>,
) -> Result<Value, String> {
    match method {
        "getCookies" => {
            let cookies = cookie_jar_for(ctx, params, session_id)?.get_all_cookies();
            let cdp_cookies: Vec<Value> = cookies.iter().map(cookie_info_to_cdp_json).collect();
            Ok(json!({ "cookies": cdp_cookies }))
        }
        "setCookies" => {
            if let Some(cookies) = params.get("cookies").and_then(|v| v.as_array()) {
                let parsed: Vec<_> = cookies.iter().filter_map(parse_cdp_cookie).collect();
                cookie_jar_for(ctx, params, session_id)?.set_cookies_from_cdp(parsed);
            }
            Ok(json!({}))
        }
        "clearCookies" => {
            cookie_jar_for(ctx, params, session_id)?.clear();
            Ok(json!({}))
        }
        "deleteCookies" => {
            if let Some(filter) = parse_delete_cookies_params(params) {
                cookie_jar_for(ctx, params, session_id)?.delete_cookies_filtered(
                    &filter.name,
                    &filter.domain,
                    filter.path.as_deref(),
                );
            }
            Ok(json!({}))
        }
        _ => Ok(json!({})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_net::CookieInfo;

    fn sample_cookie(value: &str) -> CookieInfo {
        CookieInfo {
            name: "sid".to_string(),
            value: value.to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            same_site: String::new(),
            expires: None,
        }
    }

    #[tokio::test]
    async fn clear_cookies_is_scoped_to_browser_context() {
        let mut ctx = CdpContext::new();
        let browser_context_id = ctx.create_browser_context();
        ctx.browser_context(&browser_context_id)
            .unwrap()
            .cookie_jar
            .set_cookies_from_cdp(vec![sample_cookie("isolated")]);
        ctx.default_context
            .cookie_jar
            .set_cookies_from_cdp(vec![sample_cookie("default")]);

        handle(
            "clearCookies",
            &json!({ "browserContextId": browser_context_id }),
            &mut ctx,
            &None,
        )
        .await
        .unwrap();

        assert!(ctx
            .browser_context(&browser_context_id)
            .unwrap()
            .cookie_jar
            .get_all_cookies()
            .is_empty());
        let default = ctx.default_context.cookie_jar.get_all_cookies();
        assert_eq!(default.len(), 1);
        assert_eq!(default[0].value, "default");
    }

    #[tokio::test]
    async fn clear_cookies_without_context_clears_default_context() {
        let mut ctx = CdpContext::new();
        ctx.default_context
            .cookie_jar
            .set_cookies_from_cdp(vec![sample_cookie("default")]);

        handle("clearCookies", &json!({}), &mut ctx, &None)
            .await
            .unwrap();

        assert!(ctx.default_context.cookie_jar.get_all_cookies().is_empty());
    }
}
