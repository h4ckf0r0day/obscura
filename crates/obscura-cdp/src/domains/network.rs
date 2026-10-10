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
        "disable" => {
            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.clear_response_bodies();
            } else {
                for page in &mut ctx.pages {
                    page.clear_response_bodies();
                }
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
                    #[cfg(feature = "stealth")]
                    if let Some(client) = &page.stealth_client {
                        client.set_extra_headers(header_map.clone()).await;
                    }
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
            cookie_jar_for(ctx, session_id)
                .set_cookies_from_cdp_with_scope([(cookie.cookie, cookie.host_only)]);
            Ok(json!({ "success": true }))
        }
        "setCookies" => {
            if let Some(cookies) = params.get("cookies").and_then(|v| v.as_array()) {
                let parsed: Vec<_> = cookies.iter().filter_map(parse_cdp_cookie).collect();
                cookie_jar_for(ctx, session_id).set_cookies_from_cdp_with_scope(
                    parsed
                        .into_iter()
                        .map(|cookie| (cookie.cookie, cookie.host_only)),
                );
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
        "setBlockedURLs" => {
            let patterns = params
                .get("urls")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(ToString::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            if let Some(page) = ctx.get_session_page_mut(session_id) {
                page.set_blocked_urls(patterns);
            } else {
                for page in &mut ctx.pages {
                    page.set_blocked_urls(patterns.clone());
                }
            }
            Ok(json!({}))
        }
        "getResponseBody" => {
            let request_id = params
                .get("requestId")
                .and_then(|v| v.as_str())
                .ok_or("Network.getResponseBody requires requestId")?;

            let body = if let Some(page) = ctx.get_session_page(session_id) {
                page.get_response_body(request_id)
            } else {
                ctx.pages.iter().find_map(|page| page.get_response_body(request_id))
            };

            match body {
                Some(body) => Ok(json!({
                    "body": body.body,
                    "base64Encoded": body.base64_encoded,
                })),
                None => Err(format!("No response body found for requestId {}", request_id)),
            }
        }
        _ => Err(format!("Unknown Network method: {}", method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_net::CookieInfo;

    async fn check_extra_headers_on_navigation_fetch_and_xhr(stealth: bool) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..4 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut buffer = [0u8; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "request ended before its headers");
                    request.extend_from_slice(&buffer[..count]);
                    assert!(request.len() < 8192);
                }
                requests.push(String::from_utf8(request).unwrap());
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
            }
            requests
        });
        let mut ctx = CdpContext::new();
        ctx.default_context = Arc::new(obscura_browser::BrowserContext::with_storage_and_network(
            "headers".into(), None, stealth, None, None, true,
        ));
        let page_id = ctx.create_page();
        let session = Some("headers-session".to_string());
        ctx.sessions.insert(session.clone().unwrap(), page_id);
        handle("setExtraHTTPHeaders", &json!({"headers": {"X-Obscura-Test": "retained"}}), &mut ctx, &session).await.unwrap();
        ctx.get_session_page_mut(&session).unwrap().navigate(&url).await.unwrap();
        for expression in [
            "fetch('/fetch', {headers: {'x-obscura-test': 'local'}}).then(response => response.text())",
            "new Promise((resolve, reject) => { const xhr = new XMLHttpRequest(); xhr.open('GET', '/xhr'); xhr.setRequestHeader('x-obscura-test', 'local'); xhr.onload = () => resolve(xhr.responseText); xhr.onerror = reject; xhr.send(); })",
        ] {
            let result = ctx.get_session_page_mut(&session).unwrap()
                .evaluate_for_cdp_with_timeout(expression, true, true, 5_000).await.unwrap();
            assert_eq!(result.value, Some(json!("ok")));
        }
        handle("setExtraHTTPHeaders", &json!({"headers": {}}), &mut ctx, &session).await.unwrap();
        let result = ctx.get_session_page_mut(&session).unwrap()
            .evaluate_for_cdp_with_timeout("fetch('/cleared').then(response => response.text())", true, true, 5_000).await.unwrap();
        assert_eq!(result.value, Some(json!("ok")));
        let requests = tokio::time::timeout(std::time::Duration::from_secs(5), server).await.unwrap().unwrap();
        let header = |request: &String| request.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)|
                name.eq_ignore_ascii_case("X-Obscura-Test") && value.trim() == "retained")
        });
        assert!(requests[..3].iter().all(header), "navigation, fetch and XHR must carry the header: {requests:?}");
        assert!(!header(&requests[3]), "clearing headers must affect subsequent requests");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn extra_headers_reach_navigation_fetch_and_xhr() {
        check_extra_headers_on_navigation_fetch_and_xhr(false).await;
    }

    #[cfg(feature = "stealth")]
    #[tokio::test(flavor = "current_thread")]
    async fn stealth_extra_headers_reach_navigation_fetch_and_xhr() {
        check_extra_headers_on_navigation_fetch_and_xhr(true).await;
    }

    async fn check_extra_headers_are_page_scoped(stealth: bool) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..7 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut buffer = [0u8; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    assert!(request.len() < 8192);
                }
                requests.push(String::from_utf8(request).unwrap());
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
            }
            requests
        });
        let mut ctx = CdpContext::new();
        ctx.default_context = Arc::new(obscura_browser::BrowserContext::with_storage_and_network(
            "header-isolation".into(), None, stealth, None, None, true,
        ));
        for session in ["page-a", "page-b"] {
            let page_id = ctx.create_page();
            ctx.sessions.insert(session.to_string(), page_id);
            ctx.get_session_page_mut(&Some(session.to_string())).unwrap().navigate(&url).await.unwrap();
        }
        let a = Some("page-a".to_string());
        let b = Some("page-b".to_string());
        handle("setExtraHTTPHeaders", &json!({"headers": {"X-Obscura-Test": "a-only"}}), &mut ctx, &a).await.unwrap();
        for session in [&a, &b] {
            let result = ctx.get_session_page_mut(session).unwrap()
                .evaluate_for_cdp_with_timeout("fetch('/fetch').then(response => response.text())", true, true, 5_000).await.unwrap();
            assert_eq!(result.value, Some(json!("ok")));
            ctx.get_session_page_mut(session).unwrap().navigate(&url).await.unwrap();
        }
        // A page created after the override must not inherit it either.
        let page_id = ctx.create_page();
        ctx.sessions.insert("page-c".to_string(), page_id);
        ctx.get_session_page_mut(&Some("page-c".to_string())).unwrap().navigate(&url).await.unwrap();
        let requests = tokio::time::timeout(std::time::Duration::from_secs(5), server).await.unwrap().unwrap();
        let headers: Vec<_> = requests.iter().map(|request| request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("X-Obscura-Test").then(|| value.trim().to_string())
        })).collect();
        assert_eq!(headers, vec![None, None, Some("a-only".into()), Some("a-only".into()), None, None, None],
            "only page A's fetch and navigation may carry its override");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn extra_headers_are_page_scoped() {
        check_extra_headers_are_page_scoped(false).await;
    }

    #[cfg(feature = "stealth")]
    #[tokio::test(flavor = "current_thread")]
    async fn stealth_extra_headers_are_page_scoped() {
        check_extra_headers_are_page_scoped(true).await;
    }

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

    #[tokio::test]
    async fn set_blocked_urls_targets_session_page_without_enabling_interception() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id.clone());

        handle(
            "setBlockedURLs",
            &json!({
                "urls": [
                    "*://*.example.com/*.png",
                    "*://cdn.example.com/*"
                ]
            }),
            &mut ctx,
            &session_id,
        )
        .await
        .expect("setBlockedURLs must succeed for a session page");

        let page = ctx.get_page(&page_id).unwrap();
        assert_eq!(
            page.blocked_url_patterns,
            vec![
                "*://*.example.com/*.png".to_string(),
                "*://cdn.example.com/*".to_string(),
            ]
        );
        assert!(!page.intercept_enabled);
        assert!(page.intercept_block_patterns.is_empty());
    }

    #[tokio::test]
    async fn set_blocked_urls_without_session_updates_existing_pages() {
        let mut ctx = CdpContext::new();
        let left = ctx.create_page();
        let right = ctx.create_page();

        handle(
            "setBlockedURLs",
            &json!({ "urls": ["*://tiles.example.test/*"] }),
            &mut ctx,
            &None,
        )
        .await
        .expect("setBlockedURLs must succeed without a session");

        assert_eq!(
            ctx.get_page(&left).unwrap().blocked_url_patterns,
            vec!["*://tiles.example.test/*".to_string()]
        );
        assert_eq!(
            ctx.get_page(&right).unwrap().blocked_url_patterns,
            vec!["*://tiles.example.test/*".to_string()]
        );
    }

    #[tokio::test]
    async fn set_blocked_urls_replaces_existing_patterns() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id.clone());

        handle(
            "setBlockedURLs",
            &json!({ "urls": ["*://old.example.test/*"] }),
            &mut ctx,
            &session_id,
        )
        .await
        .unwrap();
        handle(
            "setBlockedURLs",
            &json!({ "urls": ["*://new.example.test/*"] }),
            &mut ctx,
            &session_id,
        )
        .await
        .unwrap();

        assert_eq!(
            ctx.get_page(&page_id).unwrap().blocked_url_patterns,
            vec!["*://new.example.test/*".to_string()]
        );
    }

    #[tokio::test]
    async fn get_response_body_returns_stored_document_body() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id.clone());

        let page = ctx.get_page_mut(&page_id).unwrap();
        page.navigate("data:text/html,<html><body>hello body</body></html>")
            .await
            .unwrap();
        let request_id = page.network_events[0].request_id.clone();

        let result = handle(
            "getResponseBody",
            &json!({ "requestId": request_id }),
            &mut ctx,
            &session_id,
        )
        .await
        .unwrap();

        assert_eq!(result["body"], "<html><body>hello body</body></html>");
        assert_eq!(result["base64Encoded"], false);
    }

    #[tokio::test]
    async fn get_response_body_errors_for_unknown_request_id() {
        let mut ctx = CdpContext::new();
        let err = handle(
            "getResponseBody",
            &json!({ "requestId": "missing" }),
            &mut ctx,
            &None,
        )
        .await
        .unwrap_err();

        assert!(err.contains("missing"));
    }

    #[tokio::test]
    async fn network_disable_clears_stored_response_bodies() {
        let mut ctx = CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = Some("session-1".to_string());
        ctx.sessions.insert(session_id.clone().unwrap(), page_id.clone());

        let page = ctx.get_page_mut(&page_id).unwrap();
        page.navigate("data:text/html,<html><body>temporary body</body></html>")
            .await
            .unwrap();
        let request_id = page.network_events[0].request_id.clone();

        handle("disable", &json!({}), &mut ctx, &session_id)
            .await
            .unwrap();

        let err = handle(
            "getResponseBody",
            &json!({ "requestId": request_id }),
            &mut ctx,
            &session_id,
        )
        .await
        .unwrap_err();
        assert!(err.contains("No response body found"));
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
