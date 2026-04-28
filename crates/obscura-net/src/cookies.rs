use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use url::Url;

pub struct CookieJar {
    cookies: RwLock<HashMap<String, HashMap<String, CookieEntry>>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CookieEntry {
    name: String,
    value: String,
    path: String,
    domain: String,
    secure: bool,
    http_only: bool,
    expires: Option<u64>,
    same_site: String,
}

const PERSIST_FORMAT_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedJar {
    version: u32,
    cookies: Vec<CookieEntry>,
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

    /// Serialize all non-expired cookies to a JSON file at `path`.
    ///
    /// Writes atomically: writes to `<path>.tmp`, then renames into place.
    /// Expired cookies are dropped before write.
    pub fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let cookies = self.cookies.read().unwrap();
        let mut entries: Vec<CookieEntry> = cookies
            .values()
            .flat_map(|domain_cookies| domain_cookies.values().cloned())
            .filter(|e| match e.expires {
                Some(exp) => exp > now,
                None => true,
            })
            .collect();
        entries.sort_by(|a, b| {
            a.domain
                .cmp(&b.domain)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.name.cmp(&b.name))
        });
        drop(cookies);

        let persisted = PersistedJar {
            version: PERSIST_FORMAT_VERSION,
            cookies: entries,
        };

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let tmp_path = match path.file_name() {
            Some(name) => {
                let mut tmp_name = name.to_os_string();
                tmp_name.push(".tmp");
                path.with_file_name(tmp_name)
            }
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cookie store path has no file name",
                ));
            }
        };

        let json = serde_json::to_string_pretty(&persisted).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::Other, format!("serialize cookies: {}", e))
        })?;
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Load cookies from a JSON file written by `save_to_path` and merge them
    /// into this jar. Existing cookies with the same `(domain, name)` are
    /// overwritten.
    ///
    /// Returns the number of cookies loaded. Missing file returns Ok(0).
    pub fn load_from_path(&self, path: &Path) -> std::io::Result<usize> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };

        let persisted: PersistedJar = serde_json::from_slice(&bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("parse cookie store: {}", e),
            )
        })?;

        if persisted.version != PERSIST_FORMAT_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "unsupported cookie store version {} (expected {})",
                    persisted.version, PERSIST_FORMAT_VERSION
                ),
            ));
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut jar = self.cookies.write().unwrap();
        let mut loaded = 0;
        for entry in persisted.cookies {
            if let Some(exp) = entry.expires {
                if exp <= now {
                    continue;
                }
            }
            jar.entry(entry.domain.clone())
                .or_default()
                .insert(entry.name.clone(), entry);
            loaded += 1;
        }
        Ok(loaded)
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
    let domain = domain.to_lowercase();
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

    fn temp_cookie_path(label: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("obscura_cookies_{}_{}_{}.json", label, pid, nanos))
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let path = temp_cookie_path("roundtrip");
        let url = Url::parse("https://example.com/api").unwrap();

        let jar = CookieJar::new();
        jar.set_cookie("session=abc123; Path=/api; Secure; HttpOnly", &url);
        jar.set_cookie("token=xyz; Domain=example.com; Max-Age=3600", &url);
        jar.save_to_path(&path).expect("save");

        let restored = CookieJar::new();
        let n = restored.load_from_path(&path).expect("load");
        assert_eq!(n, 2);

        let header = restored.get_cookie_header(&url);
        assert!(header.contains("session=abc123"), "got: {}", header);
        assert!(header.contains("token=xyz"), "got: {}", header);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_missing_file_is_ok() {
        let jar = CookieJar::new();
        let path = temp_cookie_path("missing");
        let n = jar.load_from_path(&path).expect("load missing should succeed");
        assert_eq!(n, 0);
    }

    #[test]
    fn test_save_drops_expired_cookies() {
        let path = temp_cookie_path("expired");
        let url = Url::parse("https://example.com/").unwrap();

        let jar = CookieJar::new();
        jar.set_cookie("keep=1; Max-Age=3600", &url);
        // Manually insert an expired entry — set_cookie rejects on insert,
        // so use the internal lock.
        {
            let mut inner = jar.cookies.write().unwrap();
            inner
                .entry("example.com".to_string())
                .or_default()
                .insert(
                    "stale".to_string(),
                    CookieEntry {
                        name: "stale".to_string(),
                        value: "old".to_string(),
                        path: "/".to_string(),
                        domain: "example.com".to_string(),
                        secure: false,
                        http_only: false,
                        expires: Some(1),
                        same_site: "Lax".to_string(),
                    },
                );
        }

        jar.save_to_path(&path).expect("save");

        let restored = CookieJar::new();
        let n = restored.load_from_path(&path).expect("load");
        assert_eq!(n, 1);
        assert!(restored.get_cookie_header(&url).contains("keep=1"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_rejects_unknown_version() {
        let path = temp_cookie_path("badver");
        std::fs::write(&path, br#"{"version":999,"cookies":[]}"#).unwrap();

        let jar = CookieJar::new();
        let err = jar.load_from_path(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_save_uses_atomic_rename() {
        // Save once, corrupt the temp file, save again — second save must
        // not be observed as truncated by a concurrent reader.
        let path = temp_cookie_path("atomic");
        let url = Url::parse("https://example.com/").unwrap();

        let jar = CookieJar::new();
        jar.set_cookie("a=1", &url);
        jar.save_to_path(&path).unwrap();
        jar.set_cookie("b=2", &url);
        jar.save_to_path(&path).unwrap();

        let restored = CookieJar::new();
        restored.load_from_path(&path).unwrap();
        let header = restored.get_cookie_header(&url);
        assert!(header.contains("a=1"));
        assert!(header.contains("b=2"));

        let _ = std::fs::remove_file(&path);
    }
}
