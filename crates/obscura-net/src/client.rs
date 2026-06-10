use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use tokio::sync::RwLock;
use url::Url;

use crate::cookies::CookieJar;
use crate::interceptor::{InterceptAction, RequestInterceptor};

#[derive(Debug, Clone)]
pub struct Response {
    pub url: Url,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub redirected_from: Vec<Url>,
}

impl Response {
    /// Decode the body as text, honoring the response charset.
    ///
    /// Uses the HTTP `Content-Type` header's `charset=` parameter, then for
    /// HTML responses falls back to sniffing `<meta charset>` in the first
    /// 1KB, then UTF-8. Mirrors browser behaviour per the HTML5 spec.
    pub fn text(&self) -> String {
        if self.is_html() {
            crate::encoding::decode_response(&self.body, self.content_type())
        } else {
            crate::encoding::decode_non_html(&self.body, self.content_type())
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(|s| s.as_str())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    pub fn is_html(&self) -> bool {
        self.content_type()
            .map(|ct| ct.contains("text/html"))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct RequestInfo {
    pub url: Url,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub resource_type: ResourceType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceType {
    Document,
    Script,
    Stylesheet,
    Image,
    Font,
    Xhr,
    Fetch,
    Other,
}

pub type RequestCallback = Arc<dyn Fn(&RequestInfo) + Send + Sync>;
pub type ResponseCallback = Arc<dyn Fn(&RequestInfo, &Response) + Send + Sync>;

/// Process-wide opt-in via env var. Older flow that issue #4 introduced. The
/// new `--allow-private-network` CLI flag (issue #33) sets a per-client field
/// that is OR'd with this so existing scripts and Docker setups that pin the
/// env var keep working unchanged.
pub fn env_allows_private_network() -> bool {
    matches!(
        std::env::var("OBSCURA_ALLOW_PRIVATE_NETWORK")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Returns true if `ip` is a forbidden SSRF target: loopback, private, internal,
/// or otherwise non-publicly-routable. Canonicalizes IPv4-mapped and NAT64 IPv6
/// addresses to their embedded IPv4 and re-checks, so `::ffff:127.0.0.1` and
/// `64:ff9b::7f00:1` cannot smuggle an internal v4 address past the v6 arm.
///
/// Shared by the URL-string pre-check ([`validate_url`]) and the resolve-time
/// guard ([`SsrfDnsResolver`]) so the policy can never drift between the two
/// layers. The previous hand-rolled predicate missed `0.0.0.0`, IPv4-mapped
/// IPv6, IPv6 ULA, CGNAT and NAT64 (audit SSRF-04 / OPS-02).
pub fn is_forbidden_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_ipv4(&v4);
            }
            // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4 target in the
            // last 32 bits — re-check that v4 so it can't reach an internal host.
            let s = v6.segments();
            if s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0 {
                let v4 = Ipv4Addr::new(
                    (s[6] >> 8) as u8,
                    (s[6] & 0xff) as u8,
                    (s[7] >> 8) as u8,
                    (s[7] & 0xff) as u8,
                );
                return is_forbidden_ipv4(&v4);
            }
            is_forbidden_ipv6(v6)
        }
    }
}

fn is_forbidden_ipv4(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()                          // 127.0.0.0/8
        || ip.is_private()                    // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()                 // 169.254.0.0/16 (cloud metadata)
        || ip.is_broadcast()                  // 255.255.255.255
        || ip.is_documentation()              // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || ip.is_unspecified()                // 0.0.0.0
        || o[0] == 0                          // 0.0.0.0/8 "this host" (reaches loopback on Linux)
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || ip.is_multicast()                  // 224.0.0.0/4
        || o[0] >= 240                        // 240.0.0.0/4 reserved
}

fn is_forbidden_ipv6(ip: &Ipv6Addr) -> bool {
    ip.is_loopback()                               // ::1
        || ip.is_unspecified()                     // ::
        || ip.is_unicast_link_local()              // fe80::/10
        || (ip.segments()[0] & 0xfe00) == 0xfc00   // fc00::/7 unique local (ULA)
        || ip.is_multicast()                       // ff00::/8
}

/// reqwest DNS resolver that resolves a host and rejects the request if **any**
/// resolved address is a forbidden SSRF target. reqwest connects to exactly the
/// addresses this resolver returns (it does not re-resolve), so this closes the
/// DNS-rebinding / TOCTOU window that a URL-string check alone leaves wide open
/// (audit SSRF-01: a domain with a short-TTL record pointing at 169.254.169.254
/// or 127.0.0.1). IP-literal hosts never reach a custom resolver, so
/// [`validate_url`]'s literal check (same [`is_forbidden_ip`]) is the first layer.
#[derive(Debug, Clone)]
pub struct SsrfDnsResolver {
    allow_private_network: bool,
}

impl SsrfDnsResolver {
    pub fn new(allow_private_network: bool) -> Self {
        Self { allow_private_network }
    }
}

impl reqwest::dns::Resolve for SsrfDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow = self.allow_private_network || env_allows_private_network();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            // getaddrinfo via tokio's blocking pool. Port 0 is a placeholder;
            // reqwest substitutes the URL's real port when it connects.
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();
            if !allow {
                if let Some(bad) = addrs.iter().find(|a| is_forbidden_ip(&a.ip())) {
                    let bad_ip = bad.ip();
                    return Err(Box::<dyn std::error::Error + Send + Sync>::from(format!(
                        "SSRF blocked: '{host}' resolves to forbidden address {bad_ip}"
                    )));
                }
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Default cap (bytes) on a single response body buffered into memory
/// (NAVDOS-01/02). A hostile server can stream an endless or huge body; without
/// a cap the native `Vec` grows until the host is OOM-killed — V8's `--max-old-
/// space-size` bounds the JS heap, not this native buffer. Override with
/// `OBSCURA_MAX_BODY_BYTES`.
pub fn max_response_body() -> usize {
    const DEFAULT: usize = 256 * 1024 * 1024; // 256 MiB
    std::env::var("OBSCURA_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT)
}

/// Read a reqwest response body into memory, stopping at `max` bytes. A larger
/// (or endless / wrong-`Content-Length`) body is truncated rather than allowed
/// to exhaust host memory.
pub async fn read_body_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, ObscuraNetError> {
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| ObscuraNetError::Network(format!("Failed to read body: {}", e)))?
    {
        let remaining = max.saturating_sub(buf.len());
        if remaining == 0 {
            tracing::warn!("Response body exceeded {} bytes; truncated", max);
            break;
        }
        if chunk.len() > remaining {
            buf.extend_from_slice(&chunk[..remaining]);
            tracing::warn!("Response body exceeded {} bytes; truncated", max);
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// SSRF guard shared by every HTTP path in this crate. Rejects non-`http`/
/// `https`/`file` schemes unconditionally, and — unless `allow_private_network`
/// is set (flag or `OBSCURA_ALLOW_PRIVATE_NETWORK`) — IP-literal hosts that are
/// forbidden per [`is_forbidden_ip`] plus the `localhost` / `127.0.0.1` / `::1`
/// hostnames. Called on the initial URL and on every redirect hop. This is the
/// pre-flight literal layer; resolvable hostnames are enforced at connect time
/// by [`SsrfDnsResolver`] (rebinding-safe). `pub(crate)` so the stealth (`wreq`)
/// client enforces the exact same policy.
pub(crate) fn validate_url(url: &Url, allow_private_network: bool) -> Result<(), ObscuraNetError> {
    let allow_private_network = allow_private_network || env_allows_private_network();
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" && scheme != "file" {
        return Err(ObscuraNetError::Network(format!(
            "Forbidden URL scheme '{}' - only http, https, and file are allowed",
            scheme
        )));
    }

    if scheme == "file" || allow_private_network {
        return Ok(());
    }

    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if is_forbidden_ip(&IpAddr::V4(ip)) {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to private/internal IP address {} is not allowed",
                        ip
                    )));
                }
            }
            url::Host::Ipv6(ip) => {
                if is_forbidden_ip(&IpAddr::V6(ip)) {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to private/internal IPv6 address {} is not allowed",
                        ip
                    )));
                }
            }
            url::Host::Domain(domain) => {
                let lower_domain = domain.to_lowercase();
                if lower_domain == "localhost"
                    || lower_domain.ends_with(".localhost")
                    || lower_domain == "127.0.0.1"
                    || lower_domain == "::1"
                {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to localhost domain '{}' is not allowed",
                        domain
                    )));
                }
                // Resolvable hostnames are enforced at connect time by
                // SsrfDnsResolver (rebinding-safe); the string check above is
                // only a cheap fast-path for the obvious loopback literals.
            }
        }
    }

    Ok(())
}

async fn fetch_file_url(url: &Url) -> Result<Response, ObscuraNetError> {
    let path = url
        .to_file_path()
        .map_err(|_| ObscuraNetError::Network("Invalid file URL".to_string()))?;
    let body = tokio::fs::read(&path)
        .await
        .map_err(|e| ObscuraNetError::Network(format!("Failed to read file: {}", e)))?;

    let mut headers = HashMap::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ct = match ext.to_lowercase().as_str() {
            "html" | "htm" => "text/html",
            "css" => "text/css",
            "js" | "mjs" => "application/javascript",
            "json" => "application/json",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            _ => "application/octet-stream",
        };
        headers.insert("content-type".to_string(), ct.to_string());
    }

    Ok(Response {
        url: url.clone(),
        status: 200,
        headers,
        body,
        redirected_from: Vec::new(),
    })
}

pub struct ObscuraHttpClient {
    client: tokio::sync::OnceCell<Client>,
    proxy_url: Option<String>,
    pub cookie_jar: Arc<CookieJar>,
    pub user_agent: RwLock<String>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub interceptor: RwLock<Option<Box<dyn RequestInterceptor + Send + Sync>>>,
    pub on_request: RwLock<Vec<RequestCallback>>,
    pub on_response: RwLock<Vec<ResponseCallback>>,
    pub timeout: Duration,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
    pub block_trackers: bool,
    /// When true, `validate_url` lets localhost / RFC1918 / link-local addresses
    /// through in addition to the `OBSCURA_ALLOW_PRIVATE_NETWORK` env var.
    /// Set via `--allow-private-network` on the CLI (issue #33).
    pub allow_private_network: bool,
}

impl ObscuraHttpClient {
    pub fn new() -> Self {
        Self::with_cookie_jar(Arc::new(CookieJar::new()))
    }

    pub fn with_cookie_jar(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_options(cookie_jar, None)
    }

    pub fn with_options(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        Self::with_full_options(cookie_jar, proxy_url, false)
    }

    pub fn with_full_options(
        cookie_jar: Arc<CookieJar>,
        proxy_url: Option<&str>,
        allow_private_network: bool,
    ) -> Self {
        ObscuraHttpClient {
            client: tokio::sync::OnceCell::new(),
            proxy_url: proxy_url.map(|s| s.to_string()),
            cookie_jar,
            user_agent: RwLock::new(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36".to_string(),
            ),
            extra_headers: RwLock::new(HashMap::new()),
            interceptor: RwLock::new(None),
            on_request: RwLock::new(Vec::new()),
            on_response: RwLock::new(Vec::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            timeout: Duration::from_secs(30),
            block_trackers: false,
            allow_private_network,
        }
    }

    async fn get_client(&self) -> &Client {
        let allow_private = self.allow_private_network;
        self.client.get_or_init(|| async {
            let mut builder = Client::builder()
                .redirect(Policy::none())
                .timeout(Duration::from_secs(30))
                .danger_accept_invalid_certs(false)
                // Resolve-time SSRF guard: rejects any host that resolves to a
                // forbidden address, closing the DNS-rebinding window that the
                // URL-string check cannot (a name passes validate_url, then
                // re-resolves to 127.0.0.1/169.254.169.254 at connect time).
                .dns_resolver(Arc::new(SsrfDnsResolver::new(allow_private)))
;

            if let Some(ref proxy) = self.proxy_url {
                if let Ok(p) = reqwest::Proxy::all(proxy.as_str()) {
                    builder = builder.proxy(p);
                }
            }

            builder.build().expect("failed to build HTTP client")
        }).await
    }

    /// Read-only accessor for the proxy URL the client was configured with
    /// (if any). Exposed so callers outside the `obscura-net` crate — notably
    /// `op_fetch_url` in `obscura-js` (#139) — can route their own reqwest
    /// requests through the same upstream proxy.
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::GET, url, None).await
    }

    pub async fn post_form(&self, url: &Url, body: &str) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::POST, url, Some(body.as_bytes().to_vec())).await
    }

    pub async fn fetch_with_method(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
    ) -> Result<Response, ObscuraNetError> {
        validate_url(url, self.allow_private_network)?;

        if url.scheme() == "file" {
            return fetch_file_url(url).await;
        }

        let mut method = initial_method;
        let mut body = initial_body;
        if self.block_trackers {
            if let Some(host) = url.host_str() {
                if crate::blocklist::is_blocked(host) {
                    tracing::debug!("Blocked tracker: {}", url);
                    return Ok(Response {
                        status: 0,
                        url: url.clone(),
                        headers: HashMap::new(),
                        body: Vec::new(),
                        redirected_from: Vec::new(),
                    });
                }
            }
        }

        let mut current_url = url.clone();
        let mut redirects = Vec::new();
        let max_redirects = 20;

        for _redirect_count in 0..max_redirects {
            let request_info = RequestInfo {
                url: current_url.clone(),
                method: method.to_string(),
                headers: self.extra_headers.read().await.clone(),
                resource_type: ResourceType::Document,
            };

            if let Some(interceptor) = self.interceptor.read().await.as_ref() {
                match interceptor.intercept(&request_info).await {
                    InterceptAction::Continue => {}
                    InterceptAction::Block => {
                        return Err(ObscuraNetError::Blocked(current_url.to_string()));
                    }
                    InterceptAction::Fulfill(response) => {
                        return Ok(response);
                    }
                    InterceptAction::ModifyHeaders(headers) => {
                        let mut extra = self.extra_headers.write().await;
                        extra.extend(headers);
                    }
                }
            }

            for cb in self.on_request.read().await.iter() {
                cb(&request_info);
            }

            let ua = self.user_agent.read().await.clone();
            let mut headers = HeaderMap::new();
            headers.insert(USER_AGENT, HeaderValue::from_str(&ua).unwrap_or_else(|_| {
                HeaderValue::from_static("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36")
            }));
            headers.insert(
                reqwest::header::ACCEPT,
                HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"),
            );
            headers.insert(
                reqwest::header::ACCEPT_LANGUAGE,
                HeaderValue::from_static("en-US,en;q=0.9"),
            );
            headers.insert(
                HeaderName::from_static("sec-ch-ua"),
                HeaderValue::from_static("\"Chromium\";v=\"145\", \"Not;A=Brand\";v=\"24\", \"Google Chrome\";v=\"145\""),
            );
            headers.insert(
                HeaderName::from_static("sec-ch-ua-mobile"),
                HeaderValue::from_static("?0"),
            );
            headers.insert(
                HeaderName::from_static("sec-ch-ua-platform"),
                HeaderValue::from_static("\"Linux\""),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-dest"),
                HeaderValue::from_static("document"),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-mode"),
                HeaderValue::from_static("navigate"),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-site"),
                HeaderValue::from_static("none"),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-user"),
                HeaderValue::from_static("?1"),
            );
            headers.insert(
                HeaderName::from_static("upgrade-insecure-requests"),
                HeaderValue::from_static("1"),
            );

            let cookie_header = self.cookie_jar.get_cookie_header(&current_url);
            tracing::debug!(
                "Cookie header for {}: {} cookies ({} bytes)",
                current_url.host_str().unwrap_or("?"),
                cookie_header.split("; ").filter(|s| !s.is_empty()).count(),
                cookie_header.len(),
            );
            if !cookie_header.is_empty() {
                match HeaderValue::from_str(&cookie_header) {
                    Ok(val) => {
                        headers.insert(reqwest::header::COOKIE, val);
                    }
                    Err(_) => {
                        let filtered: String = cookie_header
                            .split("; ")
                            .filter(|pair| HeaderValue::from_str(pair).is_ok())
                            .collect::<Vec<_>>()
                            .join("; ");
                        if !filtered.is_empty() {
                            if let Ok(val) = HeaderValue::from_str(&filtered) {
                                headers.insert(reqwest::header::COOKIE, val);
                            }
                        }
                        tracing::debug!(
                            "Cookie header invalid chars, filtered {} -> {} bytes",
                            cookie_header.len(), filtered.len(),
                        );
                    }
                }
            }

            for (k, v) in self.extra_headers.read().await.iter() {
                if let (Ok(name), Ok(val)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(v),
                ) {
                    headers.insert(name, val);
                }
            }

            let mut req_builder = self.get_client().await.request(method.clone(), current_url.as_str())
                .headers(headers);

            if let Some(ref b) = body {
                if method == Method::POST {
                    req_builder = req_builder.header(
                        reqwest::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    );
                }
                req_builder = req_builder.body(b.clone());
            }

            self.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = req_builder.send().await.map_err(|e| {
                self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                ObscuraNetError::Network(format!("{}: {}", current_url, e))
            })?;
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            let status = resp.status();

            for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                if let Ok(s) = val.to_str() {
                    self.cookie_jar.set_cookie(s, &current_url);
                }
            }

            let response_headers: HashMap<String, String> = resp
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
                .collect();

            if status.is_redirection() {
                if let Some(location) = resp.headers().get(reqwest::header::LOCATION) {
                    let location_str = location.to_str().map_err(|_| {
                        ObscuraNetError::Network("Invalid redirect Location header".into())
                    })?;
                    let next_url = current_url.join(location_str).map_err(|e| {
                        ObscuraNetError::Network(format!("Invalid redirect URL: {}", e))
                    })?;
                    validate_url(&next_url, self.allow_private_network)?;
                    redirects.push(current_url.clone());
                    current_url = next_url;
                    if status == reqwest::StatusCode::MOVED_PERMANENTLY
                        || status == reqwest::StatusCode::FOUND
                        || status == reqwest::StatusCode::SEE_OTHER
                    {
                        method = Method::GET;
                        body = None;
                    }
                    continue;
                }
            }

            let body_bytes = read_body_capped(resp, max_response_body()).await?;

            let response = Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body: body_bytes,
                redirected_from: redirects,
            };

            for cb in self.on_response.read().await.iter() {
                cb(&request_info, &response);
            }

            return Ok(response);
        }

        Err(ObscuraNetError::TooManyRedirects(current_url.to_string()))
    }

    pub async fn set_user_agent(&self, ua: &str) {
        *self.user_agent.write().await = ua.to_string();
    }

    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) {
        *self.extra_headers.write().await = headers;
    }

    pub fn active_requests(&self) -> u32 {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_network_idle(&self) -> bool {
        self.active_requests() == 0
    }
}

impl Default for ObscuraHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ObscuraNetError {
    #[error("Network error: {0}")]
    Network(String),

    #[error("Too many redirects: {0}")]
    TooManyRedirects(String),

    #[error("Request blocked: {0}")]
    Blocked(String),
}

#[cfg(test)]
mod ssrf_guard_tests {
    use super::*;

    fn check(raw: &str, allow_private: bool) -> Result<(), ObscuraNetError> {
        validate_url(&Url::parse(raw).unwrap(), allow_private)
    }

    // This is the exact guard the reqwest client (initial URL + every redirect
    // hop) and the stealth client now share. These IPs/hosts are what a 302 to
    // an internal target resolves to before being followed.
    #[test]
    fn rejects_internal_ip_literals_when_guard_on() {
        for raw in [
            "http://127.0.0.1/",
            "http://127.0.0.1:8080/",
            "http://169.254.169.254/", // cloud metadata
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://[::1]:9/",
            "http://localhost/",
            "http://sub.localhost/",
        ] {
            assert!(check(raw, false).is_err(), "{raw} should be rejected with guard on");
        }
    }

    #[test]
    fn allows_public_hosts_when_guard_on() {
        for raw in ["http://93.184.216.34/", "https://example.com/", "http://8.8.8.8/"] {
            assert!(check(raw, false).is_ok(), "{raw} should pass with guard on");
        }
    }

    #[test]
    fn allow_private_network_opens_internal_targets() {
        // With the opt-in, the same internal targets pass — this is the flag
        // the stealth client must honour identically to the reqwest client.
        for raw in ["http://127.0.0.1:8080/", "http://169.254.169.254/", "http://10.0.0.1/"] {
            assert!(check(raw, true).is_ok(), "{raw} should pass with allow_private_network");
        }
    }

    #[test]
    fn rejects_non_http_schemes_unconditionally() {
        // Scheme check runs before the allow_private_network short-circuit, so
        // a redirect to e.g. ftp:// / gopher:// is rejected even with the opt-in.
        for raw in ["ftp://example.com/", "gopher://169.254.169.254/", "data:text/html,x"] {
            assert!(check(raw, true).is_err(), "{raw} scheme should be rejected even with opt-in");
        }
    }

    // Audit SSRF-04 / OPS-02: the previous predicate let all of these through.
    #[test]
    fn rejects_extended_internal_literals() {
        for raw in [
            "http://0.0.0.0/",                  // unspecified / "this host"
            "http://0.1.2.3/",                  // 0.0.0.0/8
            "http://100.64.0.1/",               // CGNAT 100.64/10
            "http://224.0.0.1/",                // IPv4 multicast
            "http://240.0.0.1/",                // reserved 240/4
            "http://[::]/",                     // IPv6 unspecified
            "http://[::ffff:127.0.0.1]/",       // IPv4-mapped loopback
            "http://[::ffff:169.254.169.254]/", // IPv4-mapped cloud metadata
            "http://[::ffff:10.0.0.1]/",        // IPv4-mapped RFC1918
            "http://[fc00::1]/",                // IPv6 ULA
            "http://[fd00::1]/",                // IPv6 ULA
            "http://[64:ff9b::7f00:1]/",        // NAT64 -> 127.0.0.1
            "http://[ff02::1]/",                // IPv6 multicast
        ] {
            assert!(check(raw, false).is_err(), "{raw} must be rejected (guard on)");
        }
    }

    #[test]
    fn is_forbidden_ip_classifies_canonical_forms() {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
        let forbidden: &[IpAddr] = &[
            "127.0.0.1".parse().unwrap(),
            "10.0.0.1".parse().unwrap(),
            "169.254.169.254".parse().unwrap(),
            "0.0.0.0".parse().unwrap(),
            "100.64.0.1".parse().unwrap(),
            IpAddr::V6("::ffff:127.0.0.1".parse::<Ipv6Addr>().unwrap()),
            IpAddr::V6("fc00::1".parse::<Ipv6Addr>().unwrap()),
            IpAddr::V6("64:ff9b::7f00:1".parse::<Ipv6Addr>().unwrap()),
        ];
        for ip in forbidden {
            assert!(is_forbidden_ip(ip), "{ip} should be forbidden");
        }
        let allowed: &[IpAddr] = &[
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            "2606:4700:4700::1111".parse().unwrap(), // public IPv6 (Cloudflare)
        ];
        for ip in allowed {
            assert!(!is_forbidden_ip(ip), "{ip} should be allowed");
        }
    }

    // Resolve-time guard: a hostname that resolves to a forbidden address must
    // be rejected at connect time, closing the rebinding window. `localhost`
    // resolves locally (no network) to 127.0.0.1 / ::1, both forbidden.
    #[tokio::test]
    async fn resolver_rejects_host_resolving_to_loopback() {
        let client = reqwest::Client::builder()
            .dns_resolver(std::sync::Arc::new(SsrfDnsResolver::new(false)))
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let err = client
            .get("http://localhost:9/")
            .send()
            .await
            .expect_err("localhost must be rejected by the resolve-time SSRF guard");
        let mut chain = err.to_string();
        let mut src = std::error::Error::source(&err);
        while let Some(e) = src {
            chain.push_str(" | ");
            chain.push_str(&e.to_string());
            src = e.source();
        }
        assert!(
            chain.contains("SSRF blocked") || chain.contains("forbidden address"),
            "expected an SSRF-guard rejection, got: {chain}"
        );
    }

    // With the opt-in, the resolver must NOT pre-emptively reject loopback
    // (the failure, if any, is a plain connection error to the closed port).
    #[tokio::test]
    async fn resolver_allows_loopback_with_opt_in() {
        let client = reqwest::Client::builder()
            .dns_resolver(std::sync::Arc::new(SsrfDnsResolver::new(true)))
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        if let Err(err) = client.get("http://localhost:9/").send().await {
            let mut chain = err.to_string();
            let mut src = std::error::Error::source(&err);
            while let Some(e) = src {
                chain.push_str(&e.to_string());
                src = e.source();
            }
            assert!(
                !chain.contains("SSRF blocked"),
                "opt-in must bypass the SSRF guard, got: {chain}"
            );
        }
    }
}
