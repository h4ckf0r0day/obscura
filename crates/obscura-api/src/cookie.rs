use std::sync::Arc;
use obscura_net::CookieJar;
use serde::{Deserialize, Serialize};

/// A cookie as exposed to the Rust API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
}

impl Cookie {
    /// Create a cookie from name=value pair with defaults.
    pub fn new(name: impl Into<String>, value: impl Into<String>, domain: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            path: "/".into(),
            secure: false,
            http_only: false,
        }
    }
}

/// Cookie management for a browser session.
pub struct CookieStore {
    jar: Arc<CookieJar>,
}

impl CookieStore {
    pub(crate) fn new(jar: Arc<CookieJar>) -> Self {
        Self { jar }
    }

    /// Set a cookie via Set-Cookie header string.
    ///
    /// Example: `store.set("session=abc123; Domain=example.com; Path=/; HttpOnly")?;`
    pub fn set(&self, set_cookie_str: &str, url: &str) -> Result<(), crate::error::Error> {
        let parsed = url::Url::parse(url)
            .map_err(|e| crate::error::Error::Internal(e.into()))?;
        self.jar.set_cookie(set_cookie_str, &parsed);
        Ok(())
    }

    /// Get all cookies as a serializable list.
    pub fn get_all(&self) -> Vec<Cookie> {
        self.jar.get_all_cookies()
            .into_iter()
            .map(|c| Cookie {
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                secure: c.secure,
                http_only: c.http_only,
            })
            .collect()
    }

    /// Get cookies for a specific URL.
    pub fn get_for_url(&self, url: &str) -> Result<Vec<Cookie>, crate::error::Error> {
        let parsed = url::Url::parse(url)
            .map_err(|e| crate::error::Error::Internal(e.into()))?;
        let header = self.jar.get_cookie_header(&parsed);
        Ok(header
            .split("; ")
            .filter(|s| !s.is_empty())
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                Some(Cookie {
                    name: parts.next()?.to_string(),
                    value: parts.next().unwrap_or("").to_string(),
                    domain: parsed.host_str()?.to_string(),
                    path: "/".into(),
                    secure: false,
                    http_only: false,
                })
            })
            .collect())
    }

    /// Save cookies to a file (JSON format).
    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), crate::error::Error> {
        self.jar.save_to_file(path).map_err(|e| crate::error::Error::Internal(e.into()))
    }

    /// Load cookies from a file.
    pub fn load_from_file(&self, path: &std::path::Path) -> Result<usize, crate::error::Error> {
        self.jar.load_from_file(path).map_err(|e| crate::error::Error::Internal(e.into()))
    }
}
