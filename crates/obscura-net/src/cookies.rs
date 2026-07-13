use std::collections::HashMap;
use std::sync::RwLock;
use url::Url;

const DEFAULT_SAME_SITE: &str = "Lax";

/// SameSite is case-insensitive per RFC 6265bis; normalize a present value to
/// title-case so stored cookies compare equal regardless of how they were sent.
/// Unrecognized values fall back to Lax per spec.
fn normalize_same_site(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "strict" => "Strict",
        "none" => "None",
        _ => "Lax",
    }
    .to_string()
}

pub struct CookieJar {
    cookies: RwLock<HashMap<String, HashMap<String, CookieEntry>>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CookieEntry {
    name: String,
    value: String,
    path: String,
    domain: String,
    /// Cookies set without a Domain attribute are host-only: sent to the exact
    /// origin host and never to subdomains. `serde(default)` keeps persisted
    /// cookie files from before this field existed loadable.
    #[serde(default)]
    host_only: bool,
    secure: bool,
    http_only: bool,
    expires: Option<u64>,
    same_site: String,
}

impl CookieJar {
    pub fn new() -> Self {
        CookieJar {
            cookies: RwLock::new(HashMap::new()),
        }
    }

    pub fn set_cookie(&self, set_cookie_str: &str, url: &Url) {
        let parts: Vec<&str> = set_cookie_str.splitn(2, ';').collect();
        let name_value = parts[0].trim();
        let (name, value) = match name_value.split_once('=') {
            Some((n, v)) => (n.trim().to_string(), v.trim().to_string()),
            None => return,
        };

        let request_host = url.host_str().unwrap_or("").to_lowercase();
        let mut domain_attr: Option<String> = None;
        let mut path = default_cookie_path(url.path());
        let mut secure = false;
        let mut http_only = false;
        let mut expires: Option<u64> = None;
        let mut same_site = "Lax".to_string();

        if parts.len() > 1 {
            for attr in parts[1].split(';') {
                let attr = attr.trim();
                if let Some((key, val)) = attr.split_once('=') {
                    match key.trim().to_lowercase().as_str() {
                        "domain" => {
                            domain_attr = Some(val.trim().trim_start_matches('.').to_lowercase());
                        }
                        "path" => {
                            path = val.trim().to_string();
                        }
                        "expires" => {
                            if let Ok(ts) = parse_http_date(val.trim()) {
                                expires = Some(ts);
                            }
                        }
                        "max-age" => {
                            if let Ok(secs) = val.trim().parse::<i64>() {
                                if secs <= 0 {
                                    expires = Some(0);
                                } else {
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    expires = Some(now + secs as u64);
                                }
                            }
                        }
                        "samesite" => {
                            same_site = normalize_same_site(val);
                        }
                        _ => {}
                    }
                } else {
                    match attr.to_lowercase().as_str() {
                        "secure" => secure = true,
                        "httponly" => http_only = true,
                        _ => {}
                    }
                }
            }
        }

        // Validate Domain against the response origin (RFC 6265): an unrelated
        // or public-suffix Domain is ignored so a response from attacker.test
        // cannot scope a cookie to victim.test (GHSA-f22c-8v6q-v6h6).
        let (domain, host_only) = match resolve_cookie_domain(&request_host, domain_attr.as_deref()) {
            Some(d) => d,
            None => return,
        };

        if let Some(exp) = expires {
            if exp == 0 {
                let mut cookies = self.cookies.write().unwrap();
                if let Some(domain_cookies) = cookies.get_mut(&domain) {
                    domain_cookies.remove(&name);
                }
                return;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if exp < now {
                return;
            }
        }

        let entry = CookieEntry {
            name: name.clone(),
            value,
            path,
            domain: domain.clone(),
            host_only,
            secure,
            http_only,
            expires,
            same_site,
        };

        let mut cookies = self.cookies.write().unwrap();
        cookies.entry(domain).or_default().insert(name, entry);
    }

    pub fn get_cookie_header(&self, url: &Url) -> String {
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme() == "https";
        let cookies = self.cookies.read().unwrap();

        let mut matching: Vec<String> = Vec::new();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for (domain, domain_cookies) in cookies.iter() {
            if !domain_matches(host, domain) {
                continue;
            }
            for entry in domain_cookies.values() {
                if entry.host_only && !host.eq_ignore_ascii_case(domain) {
                    continue;
                }
                if let Some(exp) = entry.expires {
                    if exp < now {
                        continue;
                    }
                }
                if entry.secure && !is_secure {
                    continue;
                }
                if !path_matches(path, &entry.path) {
                    continue;
                }
                matching.push(format!("{}={}", entry.name, entry.value));
            }
        }

        matching.join("; ")
    }

    pub fn get_all_cookies(&self) -> Vec<CookieInfo> {
        let cookies = self.cookies.read().unwrap();
        let mut result = Vec::new();
        for domain_cookies in cookies.values() {
            for entry in domain_cookies.values() {
                result.push(CookieInfo {
                    name: entry.name.clone(),
                    value: entry.value.clone(),
                    domain: entry.domain.clone(),
                    path: entry.path.clone(),
                    secure: entry.secure,
                    http_only: entry.http_only,
                    same_site: entry.same_site.clone(),
                    expires: entry.expires.map(|e| e as i64),
                });
            }
        }
        result
    }

    pub fn set_cookies_from_cdp(&self, cookies: Vec<CookieInfo>) {
        let mut jar = self.cookies.write().unwrap();
        for cookie in cookies {
            let same_site = if cookie.same_site.is_empty() {
                DEFAULT_SAME_SITE.to_string()
            } else {
                normalize_same_site(&cookie.same_site)
            };
            let expires = cookie.expires.and_then(|e| if e > 0 { Some(e as u64) } else { None });
            let entry = CookieEntry {
                name: cookie.name.clone(),
                value: cookie.value,
                path: cookie.path,
                domain: cookie.domain.clone(),
                // CDP/persisted import is trusted; honor the explicit domain as
                // domain-scoped (matches the prior behavior).
                host_only: false,
                secure: cookie.secure,
                http_only: cookie.http_only,
                expires,
                same_site,
            };
            jar.entry(cookie.domain).or_default().insert(cookie.name, entry);
        }
    }

    pub fn get_js_visible_cookies(&self, url: &Url) -> String {
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme() == "https";
        let cookies = self.cookies.read().unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut matching: Vec<String> = Vec::new();

        for (domain, domain_cookies) in cookies.iter() {
            if !domain_matches(host, domain) {
                continue;
            }
            for entry in domain_cookies.values() {
                if entry.host_only && !host.eq_ignore_ascii_case(domain) {
                    continue;
                }
                if entry.http_only {
                    continue;
                }
                if let Some(exp) = entry.expires {
                    if exp < now {
                        continue;
                    }
                }
                if entry.secure && !is_secure {
                    continue;
                }
                if !path_matches(path, &entry.path) {
                    continue;
                }
                matching.push(format!("{}={}", entry.name, entry.value));
            }
        }

        matching.join("; ")
    }

    pub fn set_cookie_from_js(&self, cookie_str: &str, url: &Url) {
        let parts: Vec<&str> = cookie_str.splitn(2, ';').collect();
        let name_value = parts[0].trim();
        let (name, value) = match name_value.split_once('=') {
            Some((n, v)) => (n.trim().to_string(), v.trim().to_string()),
            None => return,
        };

        let request_host = url.host_str().unwrap_or("").to_lowercase();
        let mut domain_attr: Option<String> = None;
        let mut path = default_cookie_path(url.path());
        let mut secure = false;
        let mut expires: Option<u64> = None;
        let mut same_site = "Lax".to_string();

        if parts.len() > 1 {
            for attr in parts[1].split(';') {
                let attr = attr.trim();
                if let Some((key, val)) = attr.split_once('=') {
                    match key.trim().to_lowercase().as_str() {
                        "domain" => {
                            domain_attr = Some(val.trim().trim_start_matches('.').to_lowercase());
                        }
                        "path" => {
                            path = val.trim().to_string();
                        }
                        "expires" => {
                            if let Ok(ts) = parse_http_date(val.trim()) {
                                expires = Some(ts);
                            }
                        }
                        "max-age" => {
                            if let Ok(secs) = val.trim().parse::<i64>() {
                                if secs <= 0 {
                                    expires = Some(0);
                                } else {
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    expires = Some(now + secs as u64);
                                }
                            }
                        }
                        "samesite" => {
                            same_site = normalize_same_site(val);
                        }
                        _ => {}
                    }
                } else {
                    match attr.to_lowercase().as_str() {
                        "secure" => secure = true,
                        _ => {}
                    }
                }
            }
        }

        let (domain, host_only) = match resolve_cookie_domain(&request_host, domain_attr.as_deref()) {
            Some(d) => d,
            None => return,
        };

        if let Some(exp) = expires {
            if exp == 0 {
                let mut cookies = self.cookies.write().unwrap();
                if let Some(domain_cookies) = cookies.get_mut(&domain) {
                    domain_cookies.remove(&name);
                }
                return;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if exp < now {
                return;
            }
        }

        let entry = CookieEntry {
            name: name.clone(),
            value,
            path,
            domain: domain.clone(),
            host_only,
            secure,
            http_only: false,
            expires,
            same_site,
        };

        let mut cookies = self.cookies.write().unwrap();
        cookies.entry(domain).or_default().insert(name, entry);
    }

    pub fn delete_cookie(&self, name: &str, domain: &str) {
        let mut cookies = self.cookies.write().unwrap();
        if domain.is_empty() {
            for domain_cookies in cookies.values_mut() {
                domain_cookies.remove(name);
            }
        } else {
            let domains_to_try = [
                domain.to_string(),
                format!(".{}", domain.trim_start_matches('.')),
                domain.trim_start_matches('.').to_string(),
            ];
            for d in &domains_to_try {
                if let Some(domain_cookies) = cookies.get_mut(d.as_str()) {
                    domain_cookies.remove(name);
                }
            }
        }
    }

    pub fn delete_cookies_filtered(&self, name: &str, domain: &str, path: Option<&str>) {
        let mut cookies = self.cookies.write().unwrap();
        let matches_path = |entry_path: &str| match path {
            Some(p) => entry_path == p,
            None => true,
        };
        if domain.is_empty() {
            for domain_cookies in cookies.values_mut() {
                domain_cookies.retain(|n, e| !(n == name && matches_path(&e.path)));
            }
        } else {
            let domains_to_try = [
                domain.to_string(),
                format!(".{}", domain.trim_start_matches('.')),
                domain.trim_start_matches('.').to_string(),
            ];
            for d in &domains_to_try {
                if let Some(domain_cookies) = cookies.get_mut(d.as_str()) {
                    domain_cookies.retain(|n, e| !(n == name && matches_path(&e.path)));
                }
            }
        }
    }

    pub fn clear(&self) {
        self.cookies.write().unwrap().clear();
    }

    /// Serialize all non-expired cookies to a JSON file.
    /// Writes atomically via tempfile then rename.
    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        use std::io::Write;

        let cookies = self.cookies.read().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut all: Vec<CookieInfo> = Vec::new();
        for domain_cookies in cookies.values() {
            for entry in domain_cookies.values() {
                if let Some(exp) = entry.expires {
                    if exp < now {
                        continue;
                    }
                }
                all.push(CookieInfo {
                    name: entry.name.clone(),
                    value: entry.value.clone(),
                    domain: entry.domain.clone(),
                    path: entry.path.clone(),
                    secure: entry.secure,
                    http_only: entry.http_only,
                    same_site: entry.same_site.clone(),
                    expires: entry.expires.map(|e| e as i64),
                });
            }
        }

        let json = serde_json::to_string_pretty(&all).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e)
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(
            path.parent().unwrap_or(std::path::Path::new(".")),
        )?;
        tmp.write_all(json.as_bytes())?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }

    /// Load cookies from a JSON file into the jar.
    /// Merges with existing cookies (does not clear).
    /// Returns the number of cookies loaded.
    pub fn load_from_file(&self, path: &std::path::Path) -> Result<usize, std::io::Error> {
        if !path.exists() {
            return Ok(0);
        }
        let data = std::fs::read_to_string(path)?;
        let cookies: Vec<CookieInfo> =
            serde_json::from_str(&data).map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
            })?;
        let count = cookies.len();
        self.set_cookies_from_cdp(cookies);
        Ok(count)
    }
}

impl Default for CookieJar {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CookieInfo {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    #[serde(default, rename = "sameSite")]
    pub same_site: String,
    #[serde(default)]
    pub expires: Option<i64>,
}

fn parse_http_date(s: &str) -> Result<u64, ()> {
    let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

    let s = s.replace('-', " ");
    let parts: Vec<&str> = s.split_whitespace().collect();

    if parts.len() < 5 { return Err(()); }

    let day: u64 = parts[1].parse().map_err(|_| ())?;
    let month = months.iter().position(|m| parts[2].to_lowercase().starts_with(m))
        .ok_or(())? as u64 + 1;
    let year: u64 = parts[3].parse().map_err(|_| ())?;

    let time_parts: Vec<&str> = parts[4].split(':').collect();
    let hour: u64 = time_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minute: u64 = time_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let second: u64 = time_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let mut days_total: u64 = 0;
    for y in 1970..year {
        days_total += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
    }
    let days_in_month = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    for m in 1..month {
        days_total += days_in_month[m as usize] + if m == 2 && is_leap { 1 } else { 0 };
    }
    days_total += day - 1;

    Ok(days_total * 86400 + hour * 3600 + minute * 60 + second)
}

/// Resolve the storage domain and host-only flag for a cookie being set from
/// `origin_host` (RFC 6265 §5.2/§5.3). With no Domain attribute the cookie is
/// host-only: scoped to the exact origin host. A Domain attribute is honored
/// only when it domain-matches the origin (equal to it or a parent domain) and
/// is not an obvious public suffix; otherwise the attribute is ignored and the
/// cookie is stored host-only on the origin. This is what stops a response from
/// attacker.test planting a cookie scoped to victim.test.
///
/// Returns None only when the origin host itself is absent (the cookie cannot
/// be scoped and is dropped).
///
/// Note: a full public suffix list is not bundled, so multi-label public
/// suffixes (co.uk, github.io) are not rejected; the domain-match check still
/// blocks the reported cross-domain attack, and single-label suffixes (com,
/// local) are rejected.
fn resolve_cookie_domain(origin_host: &str, domain_attr: Option<&str>) -> Option<(String, bool)> {
    let origin = origin_host.trim().trim_start_matches('.').to_lowercase();
    if origin.is_empty() {
        return None;
    }
    let dom = match domain_attr {
        None => return Some((origin, true)),
        Some(raw) => raw.trim().trim_start_matches('.').to_lowercase(),
    };
    if dom.is_empty() || dom == origin {
        return Some((origin, true));
    }
    if dom.contains('.') && origin.ends_with(&format!(".{dom}")) {
        Some((dom, false))
    } else {
        Some((origin, true))
    }
}

// RFC 6265 5.1.4 default-path: the path a cookie is scoped to when its
// Set-Cookie carries no Path attribute. It is the request URI's directory — the
// path up to but not including the right-most '/' — NOT the full request path.
// Using the full path scopes a session cookie to the exact URL that set it: a
// cookie set on `/app/login` would then not match `/app/dashboard`, silently
// logging the user out on the next navigation. Browsers store `/app` here.
pub fn default_cookie_path(request_path: &str) -> String {
    // "If the uri-path is empty or if the first character is not '/', output /."
    if !request_path.starts_with('/') {
        return "/".to_string();
    }
    match request_path.rfind('/') {
        // "If the uri-path contains no more than one '/', output /."
        Some(0) | None => "/".to_string(),
        // "Output the characters up to, but not including, the right-most '/'."
        Some(idx) => request_path[..idx].to_string(),
    }
}

// RFC 6265 5.1.4 path-match. A bare `starts_with` over-matches sibling paths
// that share a string prefix (a Path=/admin cookie leaking to /administrator),
// so a prefix match also requires the boundary to fall on a '/'.
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    // Prefix match: exact when the cookie-path already ends in '/', otherwise
    // the next char of the request-path must be the '/' boundary.
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

fn domain_matches(host: &str, domain: &str) -> bool {
    // Avoid allocations on the hot path. Cookie lookup runs per fetch
    // (every subresource on a page) and walks every domain in the jar.
    // Previously this allocated 2 lowercase Strings + a "." prefix
    // per (host, domain) pair.
    let domain = domain.trim_start_matches('.');
    if host.len() < domain.len() {
        return false;
    }
    // Exact match (case-insensitive)
    if host.eq_ignore_ascii_case(domain) {
        return true;
    }
    // Suffix match with a '.' boundary: host = "sub.example.com",
    // domain = "example.com". The byte before the suffix in host
    // must be '.'.
    let prefix_len = host.len() - domain.len();
    if prefix_len < 1 { return false; }
    if !host.is_char_boundary(prefix_len) { return false; }
    if host.as_bytes()[prefix_len - 1] != b'.' { return false; }
    host[prefix_len..].eq_ignore_ascii_case(domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/path").unwrap();
        jar.set_cookie("session=abc123; Path=/; Secure; HttpOnly", &url);

        let header = jar.get_cookie_header(&url);
        assert!(header.contains("session=abc123"));
    }

    #[test]
    fn test_cookie_domain_matching() {
        let jar = CookieJar::new();
        let url = Url::parse("https://www.example.com/").unwrap();
        jar.set_cookie("token=xyz; Domain=example.com", &url);

        let header = jar.get_cookie_header(&url);
        assert!(header.contains("token=xyz"));

        let sub_url = Url::parse("https://api.example.com/").unwrap();
        let header2 = jar.get_cookie_header(&sub_url);
        assert!(header2.contains("token=xyz"));

        let other_url = Url::parse("https://other.com/").unwrap();
        let header3 = jar.get_cookie_header(&other_url);
        assert!(header3.is_empty());
    }

    #[test]
    fn test_cdp_cookie_with_leading_dot_domain_matches_requests() {
        let jar = CookieJar::new();
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "token".to_string(),
            value: "xyz".to_string(),
            domain: ".example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            same_site: String::new(),
            expires: None,
        }]);

        let apex_url = Url::parse("https://example.com/").unwrap();
        let apex_header = jar.get_cookie_header(&apex_url);
        assert!(apex_header.contains("token=xyz"));

        let subdomain_url = Url::parse("https://api.example.com/").unwrap();
        let subdomain_header = jar.get_cookie_header(&subdomain_url);
        assert!(subdomain_header.contains("token=xyz"));

        let other_url = Url::parse("https://other.com/").unwrap();
        let other_header = jar.get_cookie_header(&other_url);
        assert!(other_header.is_empty());
    }

    #[test]
    fn test_secure_cookie_not_sent_over_http() {
        let jar = CookieJar::new();
        let https_url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("secure_token=secret; Secure", &https_url);

        let http_url = Url::parse("http://example.com/").unwrap();
        let header = jar.get_cookie_header(&http_url);
        assert!(header.is_empty());
    }

    #[test]
    fn test_max_age_zero_deletes_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("session=abc", &url);
        assert!(jar.get_cookie_header(&url).contains("session=abc"));

        jar.set_cookie("session=abc; Max-Age=0", &url);
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn test_max_age_sets_expiry() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("token=xyz; Max-Age=3600", &url);
        assert!(jar.get_cookie_header(&url).contains("token=xyz"));
    }

    #[test]
    fn test_expired_cookie_not_sent() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("old=gone; Expires=Thu, 01 Jan 2020 00:00:00 GMT", &url);
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn test_samesite_parsed() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("strict_cookie=val; SameSite=Strict", &url);
        assert!(jar.get_cookie_header(&url).contains("strict_cookie=val"));
    }

    #[test]
    fn test_clear_cookies() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("a=1", &url);
        assert!(!jar.get_cookie_header(&url).is_empty());

        jar.clear();
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn test_set_cookies_from_cdp_preserves_same_site_and_expires() {
        let jar = CookieJar::new();
        let future_expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "sid".to_string(),
            value: "abc".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: true,
            same_site: "Strict".to_string(),
            expires: Some(future_expiry),
        }]);

        let cookies = jar.get_all_cookies();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].same_site, "Strict");
        assert_eq!(cookies[0].expires, Some(future_expiry));
    }

    #[test]
    fn test_set_cookies_from_cdp_session_when_expires_none() {
        let jar = CookieJar::new();
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "n".to_string(),
            value: "v".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            same_site: String::new(),
            expires: None,
        }]);
        let cookies = jar.get_all_cookies();
        assert_eq!(cookies[0].expires, None);
        assert_eq!(cookies[0].same_site, DEFAULT_SAME_SITE);
    }

    #[test]
    fn test_delete_cookies_filtered_path_mismatch_preserves_cookie() {
        let jar = CookieJar::new();
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "sid".to_string(),
            value: "v".to_string(),
            domain: "example.com".to_string(),
            path: "/admin".to_string(),
            secure: false,
            http_only: false,
            same_site: String::new(),
            expires: None,
        }]);
        jar.delete_cookies_filtered("sid", "example.com", Some("/other"));
        assert_eq!(jar.get_all_cookies().len(), 1);

        jar.delete_cookies_filtered("sid", "example.com", Some("/admin"));
        assert!(jar.get_all_cookies().is_empty());
    }

    #[test]
    fn test_delete_cookies_filtered_no_path_deletes_regardless() {
        let jar = CookieJar::new();
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "sid".to_string(),
            value: "v".to_string(),
            domain: "example.com".to_string(),
            path: "/admin".to_string(),
            secure: false,
            http_only: false,
            same_site: String::new(),
            expires: None,
        }]);
        jar.delete_cookies_filtered("sid", "example.com", None);
        assert!(jar.get_all_cookies().is_empty());
    }

    #[test]
    fn test_set_cookies_from_cdp_expired_does_not_persist() {
        let jar = CookieJar::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "old".to_string(),
            value: "v".to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            same_site: String::new(),
            expires: Some(now - 1),
        }]);
        let url = Url::parse("https://example.com/").unwrap();
        assert!(jar.get_cookie_header(&url).is_empty());
    }
    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cookies.json");

        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("session=abc123; Domain=example.com; Path=/", &url);
        jar.set_cookie("token=xyz; Secure; HttpOnly", &url);

        jar.save_to_file(&path).unwrap();
        assert!(path.exists());

        let jar2 = CookieJar::new();
        let count = jar2.load_from_file(&path).unwrap();
        assert_eq!(count, 2);

        let header = jar2.get_cookie_header(&url);
        assert!(header.contains("session=abc123"));
        assert!(header.contains("token=xyz"));
    }

    #[test]
    fn test_load_nonexistent_file_returns_zero() {
        let jar = CookieJar::new();
        let count = jar
            .load_from_file(std::path::Path::new("/nonexistent/cookies.json"))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_domain_matches_subdomain_without_leading_dot() {
        let jar = CookieJar::new();
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "session".to_string(),
            value: "abc".to_string(),
            domain: "xiaohongshu.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: true,
            same_site: String::new(),
            expires: None,
        }]);
        let url = Url::parse("https://www.xiaohongshu.com/explore").unwrap();
        let header = jar.get_cookie_header(&url);
        assert!(header.contains("session=abc"), "Cookie header was: '{}'", header);
    }

    #[test]
    fn test_cookie_from_file_load_then_send_in_request() {
        // Simulate what happens: load cookies from file → navigate → cookie should be in request
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cookies.json");
        
        // Write cookies like we exported from Chrome
        let cookies = serde_json::json!([
            {"name": "a1", "value": "testval", "domain": "xiaohongshu.com", "path": "/", "secure": false, "httpOnly": false},
            {"name": "web_session", "value": "sess123", "domain": "xiaohongshu.com", "path": "/", "secure": false, "httpOnly": true},
        ]);
        std::fs::write(&path, serde_json::to_string(&cookies).unwrap()).unwrap();
        
        let jar = CookieJar::new();
        let count = jar.load_from_file(&path).unwrap();
        assert_eq!(count, 2, "Should load 2 cookies");
        
        let url = Url::parse("https://www.xiaohongshu.com/explore").unwrap();
        let header = jar.get_cookie_header(&url);
        assert!(header.contains("a1=testval"), "Missing a1 in: '{}'", header);
        assert!(header.contains("web_session=sess123"), "Missing web_session in: '{}'", header);
    }

    #[test]
    fn attacker_response_cannot_set_unrelated_victim_domain_cookie() {
        // GHSA-f22c-8v6q-v6h6: a response from attacker.test must not be able to
        // plant a cookie scoped to victim.test.
        let jar = CookieJar::new();
        let attacker = Url::parse("http://attacker.test/").unwrap();
        jar.set_cookie("sid=attacker; Domain=victim.test; Path=/", &attacker);

        let victim = Url::parse("http://victim.test/account").unwrap();
        assert!(
            !jar.get_cookie_header(&victim).contains("sid=attacker"),
            "cross-domain cookie leaked to victim: {}",
            jar.get_cookie_header(&victim)
        );
        // The cookie is stored host-only on the attacker origin instead.
        assert!(jar.get_cookie_header(&attacker).contains("sid=attacker"));
    }

    #[test]
    fn document_cookie_cannot_set_unrelated_victim_domain_cookie() {
        let jar = CookieJar::new();
        let attacker = Url::parse("http://attacker.test/").unwrap();
        jar.set_cookie_from_js("js_sid=attacker; Domain=victim.test; Path=/", &attacker);

        let victim = Url::parse("http://victim.test/account").unwrap();
        assert!(
            !jar.get_cookie_header(&victim).contains("js_sid=attacker"),
            "cross-domain JS cookie leaked to victim: {}",
            jar.get_cookie_header(&victim)
        );
    }

    #[test]
    fn public_suffix_domain_attribute_is_ignored() {
        let jar = CookieJar::new();
        let url = Url::parse("http://www.example.com/").unwrap();
        jar.set_cookie("bad=1; Domain=com; Path=/", &url);
        // "com" is a public suffix; the cookie must not be scoped to it.
        let other = Url::parse("http://other.com/").unwrap();
        assert!(!jar.get_cookie_header(&other).contains("bad=1"));
    }

    #[test]
    fn host_only_cookie_not_sent_to_subdomain() {
        let jar = CookieJar::new();
        let www = Url::parse("http://www.example.com/").unwrap();
        jar.set_cookie("hostonly=1; Path=/", &www); // no Domain attribute -> host-only

        assert!(jar.get_cookie_header(&www).contains("hostonly=1"));
        let sub = Url::parse("http://sub.www.example.com/").unwrap();
        assert!(
            !jar.get_cookie_header(&sub).contains("hostonly=1"),
            "host-only cookie leaked to subdomain: {}",
            jar.get_cookie_header(&sub)
        );
    }

    #[test]
    fn valid_subdomain_can_set_parent_domain_cookie() {
        // A subdomain setting Domain=<parent> (a legitimate parent) still works.
        let jar = CookieJar::new();
        let www = Url::parse("http://www.example.com/").unwrap();
        jar.set_cookie("token=1; Domain=example.com; Path=/", &www);

        let apex = Url::parse("http://example.com/").unwrap();
        assert!(jar.get_cookie_header(&apex).contains("token=1"));
        let api = Url::parse("http://api.example.com/").unwrap();
        assert!(jar.get_cookie_header(&api).contains("token=1"));
    }

    #[test]
    fn cookie_path_requires_slash_boundary() {
        // RFC 6265 5.1.4: a Path=/admin cookie must NOT be sent to a sibling
        // path like /administrator that merely shares the string prefix. It is
        // sent to /admin, /admin/, and /admin/x.
        let jar = CookieJar::new();
        let admin = Url::parse("https://example.com/admin").unwrap();
        jar.set_cookie("sess=1; Path=/admin", &admin);

        let sibling = Url::parse("https://example.com/administrator").unwrap();
        assert!(
            !jar.get_cookie_header(&sibling).contains("sess=1"),
            "cookie leaked to sibling path /administrator: {}",
            jar.get_cookie_header(&sibling)
        );

        assert!(jar.get_cookie_header(&admin).contains("sess=1"));
        let exact_slash = Url::parse("https://example.com/admin/").unwrap();
        assert!(jar.get_cookie_header(&exact_slash).contains("sess=1"));
        let sub = Url::parse("https://example.com/admin/panel").unwrap();
        assert!(jar.get_cookie_header(&sub).contains("sess=1"));
    }

    #[test]
    fn default_cookie_path_is_request_directory() {
        // RFC 6265 5.1.4 default-path: up to (not including) the right-most '/'.
        assert_eq!(default_cookie_path("/app/login"), "/app");
        assert_eq!(default_cookie_path("/app/"), "/app");
        assert_eq!(default_cookie_path("/a/b/c"), "/a/b");
        // No more than one '/', empty, or non-absolute -> "/".
        assert_eq!(default_cookie_path("/foo"), "/");
        assert_eq!(default_cookie_path("/"), "/");
        assert_eq!(default_cookie_path(""), "/");
        assert_eq!(default_cookie_path("relative"), "/");
    }

    #[test]
    fn cookie_without_path_defaults_to_directory_not_full_path() {
        // A Set-Cookie with no Path attribute on /app/login must scope to /app
        // (RFC 6265 5.1.4), so the session survives navigation to /app/dashboard.
        // Before this fix it was scoped to the full path /app/login and vanished
        // on the next page, appearing as a silent logout.
        let jar = CookieJar::new();
        let login = Url::parse("https://example.com/app/login").unwrap();
        jar.set_cookie("sid=abc", &login);

        let dashboard = Url::parse("https://example.com/app/dashboard").unwrap();
        assert!(
            jar.get_cookie_header(&dashboard).contains("sid=abc"),
            "session cookie was not sent to a sibling path under the same directory: {}",
            jar.get_cookie_header(&dashboard)
        );
        // Still sent at the directory root and the original path.
        let app_root = Url::parse("https://example.com/app/").unwrap();
        assert!(jar.get_cookie_header(&app_root).contains("sid=abc"));
        assert!(jar.get_cookie_header(&login).contains("sid=abc"));

        // But not to an unrelated top-level path outside the directory.
        let other = Url::parse("https://example.com/other").unwrap();
        assert!(
            !jar.get_cookie_header(&other).contains("sid=abc"),
            "cookie leaked outside its default-path directory"
        );
    }

    #[test]
    fn js_cookie_without_path_also_defaults_to_directory() {
        // document.cookie set on /shop/cart with no path must reach /shop/checkout.
        let jar = CookieJar::new();
        let cart = Url::parse("https://example.com/shop/cart").unwrap();
        jar.set_cookie_from_js("cart=xyz", &cart);
        let checkout = Url::parse("https://example.com/shop/checkout").unwrap();
        assert!(
            jar.get_js_visible_cookies(&checkout).contains("cart=xyz"),
            "JS cookie not visible at sibling path: {}",
            jar.get_js_visible_cookies(&checkout)
        );
    }
}
