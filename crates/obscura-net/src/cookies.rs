use std::collections::HashMap;
use std::sync::RwLock;
use url::Url;

pub struct CookieJar {
    cookies: RwLock<HashMap<String, HashMap<String, CookieEntry>>>,
}

#[derive(Debug, Clone)]
struct CookieEntry {
    name: String,
    value: String,
    path: String,
    domain: String,
    secure: bool,
    http_only: bool,
    expires: Option<u64>,
    #[allow(dead_code)]
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

        let mut domain = url.host_str().unwrap_or("").to_lowercase();
        let mut path = url.path().to_string();
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
                            domain = val.trim().trim_start_matches('.').to_lowercase();
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
                            same_site = val.trim().to_string();
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
                if let Some(exp) = entry.expires {
                    if exp < now {
                        continue;
                    }
                }
                if entry.secure && !is_secure {
                    continue;
                }
                if !path.starts_with(&entry.path) {
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
                });
            }
        }
        result
    }

    pub fn set_cookies_from_cdp(&self, cookies: Vec<CookieInfo>) {
        let mut jar = self.cookies.write().unwrap();
        for cookie in cookies {
            let entry = CookieEntry {
                name: cookie.name.clone(),
                value: cookie.value,
                path: cookie.path,
                domain: cookie.domain.clone(),
                secure: cookie.secure,
                http_only: cookie.http_only,
                expires: None,
                same_site: "Lax".to_string(),
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
                if !path.starts_with(&entry.path) {
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

        let mut domain = url.host_str().unwrap_or("").to_lowercase();
        let mut path = url.path().to_string();
        let mut secure = false;
        let mut expires: Option<u64> = None;
        let mut same_site = "Lax".to_string();

        if parts.len() > 1 {
            for attr in parts[1].split(';') {
                let attr = attr.trim();
                if let Some((key, val)) = attr.split_once('=') {
                    match key.trim().to_lowercase().as_str() {
                        "domain" => {
                            domain = val.trim().trim_start_matches('.').to_lowercase();
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
                            same_site = val.trim().to_string();
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

    pub fn clear(&self) {
        self.cookies.write().unwrap().clear();
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

fn domain_matches(host: &str, domain: &str) -> bool {
    let host = host.to_lowercase();
    let domain = domain.trim_start_matches('.').to_lowercase();
    host == domain || host.ends_with(&format!(".{}", domain))
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
}
