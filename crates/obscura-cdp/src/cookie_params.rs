use obscura_net::CookieInfo;
use serde_json::Value;

const DEFAULT_COOKIE_PATH: &str = "/";

pub fn parse_cdp_cookie(value: &Value) -> Option<CookieInfo> {
    let name = value.get("name").and_then(|v| v.as_str())?.to_string();
    let cookie_value = value
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let url_parsed = value
        .get("url")
        .and_then(|v| v.as_str())
        .and_then(|u| url::Url::parse(u).ok());

    let domain = value
        .get("domain")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| url_parsed.as_ref().and_then(|u| u.host_str().map(|h| h.to_string())))
        .unwrap_or_default();

    if domain.is_empty() {
        return None;
    }

    let path = value
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            url_parsed
                .as_ref()
                .map(|u| obscura_net::default_cookie_path(u.path()))
        })
        .unwrap_or_else(|| DEFAULT_COOKIE_PATH.to_string());

    let secure = value.get("secure").and_then(|v| v.as_bool()).unwrap_or(false);
    let http_only = value.get("httpOnly").and_then(|v| v.as_bool()).unwrap_or(false);
    let same_site = value
        .get("sameSite")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let expires = value.get("expires").and_then(|v| v.as_f64()).map(|f| f as i64);

    Some(CookieInfo {
        name,
        value: cookie_value,
        domain,
        path,
        secure,
        http_only,
        same_site,
        expires,
    })
}

pub struct DeleteCookiesFilter {
    pub name: String,
    pub domain: String,
    pub path: Option<String>,
}

pub fn parse_delete_cookies_params(params: &Value) -> Option<DeleteCookiesFilter> {
    let name = params.get("name").and_then(|v| v.as_str())?.to_string();
    if name.is_empty() {
        return None;
    }

    let url_parsed = params
        .get("url")
        .and_then(|v| v.as_str())
        .and_then(|u| url::Url::parse(u).ok());

    let domain = params
        .get("domain")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| url_parsed.as_ref().and_then(|u| u.host_str().map(|h| h.to_string())))
        .unwrap_or_default();

    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| url_parsed.as_ref().map(|u| u.path().to_string()));

    Some(DeleteCookiesFilter { name, domain, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_with_explicit_domain_path() {
        let v = json!({
            "name": "session",
            "value": "abc",
            "domain": ".example.com",
            "path": "/app",
            "secure": true,
            "httpOnly": true,
            "sameSite": "Strict",
            "expires": 1_900_000_000.0,
        });
        let c = parse_cdp_cookie(&v).unwrap();
        assert_eq!(c.name, "session");
        assert_eq!(c.value, "abc");
        assert_eq!(c.domain, ".example.com");
        assert_eq!(c.path, "/app");
        assert!(c.secure);
        assert!(c.http_only);
        assert_eq!(c.same_site, "Strict");
        assert_eq!(c.expires, Some(1_900_000_000));
    }

    #[test]
    fn parse_with_url_fallback_for_domain_and_path() {
        // RFC 6265 5.1.4 default-path: a cookie set under /v1/things with no
        // explicit Path is scoped to the directory /v1, not the full request
        // path. Puppeteer's page.setCookie / Playwright's addCookies reach this
        // path when the caller omits `path`.
        let v = json!({
            "name": "tok",
            "value": "xyz",
            "url": "https://api.example.com/v1/things",
        });
        let c = parse_cdp_cookie(&v).unwrap();
        assert_eq!(c.domain, "api.example.com");
        assert_eq!(c.path, "/v1");
    }

    #[test]
    fn parse_explicit_path_overrides_default() {
        // An explicit Path attribute wins over the RFC default-path.
        let v = json!({
            "name": "tok",
            "value": "xyz",
            "url": "https://api.example.com/v1/things",
            "path": "/v1/things",
        });
        let c = parse_cdp_cookie(&v).unwrap();
        assert_eq!(c.path, "/v1/things");
    }

    #[test]
    fn parse_rejects_when_no_domain_or_url() {
        let v = json!({ "name": "x", "value": "y" });
        assert!(parse_cdp_cookie(&v).is_none());
    }

    #[test]
    fn parse_default_path_when_url_has_no_path() {
        let v = json!({
            "name": "tok",
            "value": "v",
            "domain": "example.com",
        });
        let c = parse_cdp_cookie(&v).unwrap();
        assert_eq!(c.path, "/");
    }

    #[test]
    fn delete_filter_requires_name() {
        let v = json!({ "domain": "example.com" });
        assert!(parse_delete_cookies_params(&v).is_none());
    }

    #[test]
    fn delete_filter_uses_url_for_domain_and_path() {
        let v = json!({ "name": "session", "url": "https://example.com/admin" });
        let f = parse_delete_cookies_params(&v).unwrap();
        assert_eq!(f.name, "session");
        assert_eq!(f.domain, "example.com");
        assert_eq!(f.path.as_deref(), Some("/admin"));
    }
}
