use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use std::net::{IpAddr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use tokio::sync::{RwLock, watch};
use url::Url;

use crate::cookies::CookieJar;
use crate::interceptor::{InterceptAction, RequestInterceptor};

fn configured_root_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("SSL_CERT_FILE").filter(|path| !path.is_empty()) {
        paths.push(path.into());
    }
    if let Some(directory) = std::env::var_os("SSL_CERT_DIR").filter(|path| !path.is_empty()) {
        match std::fs::read_dir(directory) {
            Ok(entries) => {
                paths.extend(entries.filter_map(|entry| entry.ok().map(|entry| entry.path())));
            }
            Err(error) => {
                tracing::warn!(%error, "failed to read SSL_CERT_DIR");
            }
        }
    }
    paths.sort();
    paths
}

fn configured_root_certificates() -> &'static [reqwest::Certificate] {
    static ROOTS: OnceLock<Vec<reqwest::Certificate>> = OnceLock::new();

    ROOTS.get_or_init(|| {
        let mut certificates = Vec::new();
        for path in configured_root_paths() {
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "failed to read CA certificate file");
                    continue;
                }
            };
            match reqwest::Certificate::from_pem_bundle(&bytes) {
                Ok(mut bundle) if !bundle.is_empty() => certificates.append(&mut bundle),
                _ => match reqwest::Certificate::from_der(&bytes) {
                    Ok(certificate) => certificates.push(certificate),
                    Err(error) => {
                        tracing::warn!(%error, path = %path.display(), "failed to parse CA certificate file");
                    }
                },
            }
        }
        certificates
    })
}

/// Whether SSL_CERT_FILE / SSL_CERT_DIR request a custom TLS trust store. A
/// variable that is set but empty (e.g. `SSL_CERT_FILE=""`, a common shell
/// accident) is treated as unset, matching the `!is_empty()` filter in
/// `configured_root_paths` above. The stealth client (wreq) relies on this:
/// supplying a store to `tls_cert_store` REPLACES the bundled webpki roots, so
/// an empty value would otherwise build a near-empty store and break all HTTPS.
///
/// The only non-test caller is the stealth (wreq) client, so a plain build
/// without the `stealth` feature sees it as unused.
#[cfg_attr(not(feature = "stealth"), allow(dead_code))]
pub(crate) fn custom_cert_store_requested(
    cert_file: Option<&std::ffi::OsStr>,
    cert_dir: Option<&std::ffi::OsStr>,
) -> bool {
    cert_file.is_some_and(|v| !v.is_empty()) || cert_dir.is_some_and(|v| !v.is_empty())
}

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

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
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

/// Fetch metadata for a browser-owned request. Navigation keeps its existing
/// profile; render resources use this type so they do not masquerade as HTML
/// documents when they move onto the page's asynchronous transport.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum RequestMode {
    Navigate,
    NoCors,
    Cors,
    SameOrigin,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum RequestCredentials {
    Omit,
    SameOrigin,
    Include,
}

impl RequestMode {
    pub(crate) fn header_value(self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::NoCors => "no-cors",
            Self::Cors => "cors",
            Self::SameOrigin => "same-origin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRequest {
    pub resource_type: ResourceType,
    /// Origin-bearing environment that owns the request. This controls CORS,
    /// credentials, and Sec-Fetch-Site and must remain the document/realm for
    /// every descendant in a module graph.
    pub initiator: Option<Url>,
    /// URL used to derive the Referer header. Usually the same as `initiator`,
    /// but a module dependency is referred by its importing module while its
    /// credentials mode is still relative to the owning document.
    pub referrer: Option<Url>,
    pub mode: RequestMode,
    pub credentials: RequestCredentials,
    /// Hard limit for the decoded response body retained by this request.
    /// Callers can lower it for especially constrained resource consumers.
    pub max_response_bytes: usize,
}

impl ResourceRequest {
    pub fn navigation() -> Self {
        Self {
            resource_type: ResourceType::Document,
            initiator: None,
            referrer: None,
            mode: RequestMode::Navigate,
            credentials: RequestCredentials::Include,
            max_response_bytes: 64 * 1024 * 1024,
        }
    }

    pub fn subresource(resource_type: ResourceType, initiator: &Url) -> Self {
        let mode = match resource_type {
            ResourceType::Font | ResourceType::Xhr | ResourceType::Fetch => RequestMode::Cors,
            ResourceType::Document => RequestMode::Navigate,
            ResourceType::Script
            | ResourceType::Stylesheet
            | ResourceType::Image
            | ResourceType::Other => RequestMode::NoCors,
        };
        let credentials = match resource_type {
            ResourceType::Document
            | ResourceType::Script
            | ResourceType::Stylesheet
            | ResourceType::Image
            | ResourceType::Other => RequestCredentials::Include,
            ResourceType::Font | ResourceType::Xhr | ResourceType::Fetch => {
                RequestCredentials::SameOrigin
            }
        };
        Self {
            resource_type,
            initiator: Some(initiator.clone()),
            referrer: Some(initiator.clone()),
            mode,
            credentials,
            max_response_bytes: match resource_type {
                ResourceType::Stylesheet | ResourceType::Font => 16 * 1024 * 1024,
                ResourceType::Script | ResourceType::Other => 32 * 1024 * 1024,
                ResourceType::Document
                | ResourceType::Image
                | ResourceType::Xhr
                | ResourceType::Fetch => 64 * 1024 * 1024,
            },
        }
    }

    /// Fetch profile for JavaScript modules. Unlike classic scripts, module
    /// scripts are CORS-enabled and use `same-origin` credentials by default.
    /// Keep this separate from `subresource(Script, ..)`, whose no-CORS,
    /// include-credentials profile is still correct for classic scripts.
    pub fn module_script(initiator: &Url, referrer: &Url) -> Self {
        Self {
            resource_type: ResourceType::Script,
            initiator: Some(initiator.clone()),
            referrer: Some(referrer.clone()),
            mode: RequestMode::Cors,
            credentials: RequestCredentials::SameOrigin,
            max_response_bytes: 32 * 1024 * 1024,
        }
    }

    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    pub(crate) fn destination(&self) -> &'static str {
        match self.resource_type {
            ResourceType::Document => "document",
            ResourceType::Script => "script",
            ResourceType::Stylesheet => "style",
            ResourceType::Image => "image",
            ResourceType::Font => "font",
            ResourceType::Xhr | ResourceType::Fetch | ResourceType::Other => "empty",
        }
    }

    pub(crate) fn accept(&self) -> &'static str {
        match self.resource_type {
            ResourceType::Document => "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
            ResourceType::Stylesheet => "text/css,*/*;q=0.1",
            // AVIF is intentionally omitted until obscura's decoder can paint
            // it. Advertising a format and then discarding the selected body
            // is less faithful than negotiating the best format we can use.
            ResourceType::Image => "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            ResourceType::Script
            | ResourceType::Font
            | ResourceType::Xhr
            | ResourceType::Fetch
            | ResourceType::Other => "*/*",
        }
    }

    pub(crate) fn sends_credentials_to(&self, target: &Url) -> bool {
        match self.credentials {
            RequestCredentials::Omit => false,
            RequestCredentials::Include => true,
            RequestCredentials::SameOrigin => self
                .initiator
                .as_ref()
                .is_some_and(|initiator| initiator.origin() == target.origin()),
        }
    }
}

pub(crate) struct InFlightGuard {
    counter: Arc<AtomicU32>,
}

impl InFlightGuard {
    pub(crate) fn new(counter: &Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self {
            counter: counter.clone(),
        }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn same_origin(request: &ResourceRequest, target: &Url) -> bool {
    request
        .initiator
        .as_ref()
        .is_some_and(|initiator| initiator.origin() == target.origin())
}

pub(crate) fn cors_required(request: &ResourceRequest, target: &Url) -> bool {
    request.mode == RequestMode::Cors && !same_origin(request, target)
}

/// Serialize the request origin used by both the Origin request header and the
/// response CORS check. A redirect chain that changes origin after it has
/// already left the initiator origin is tainted and serializes to `null`.
pub(crate) fn serialized_request_origin(
    request: &ResourceRequest,
    redirect_tainted: bool,
) -> String {
    if redirect_tainted {
        return "null".to_string();
    }
    request
        .initiator
        .as_ref()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|| "null".to_string())
}

pub(crate) fn redirect_taints_origin(
    request: &ResourceRequest,
    current: &Url,
    next: &Url,
) -> bool {
    current.origin() != next.origin()
        && request
            .initiator
            .as_ref()
            .is_none_or(|initiator| initiator.origin() != current.origin())
}

pub(crate) fn validate_request_mode(
    request: &ResourceRequest,
    target: &Url,
) -> Result<(), ObscuraNetError> {
    if request.mode == RequestMode::SameOrigin && !same_origin(request, target) {
        return Err(ObscuraNetError::Cors(format!(
            "same-origin request blocked for {}",
            target
        )));
    }
    Ok(())
}

pub(crate) fn validate_cors_response(
    request: &ResourceRequest,
    target: &Url,
    serialized_origin: &str,
    allow_origin: Option<&str>,
    allow_credentials: Option<&str>,
) -> Result<(), ObscuraNetError> {
    if !cors_required(request, target) {
        return Ok(());
    }

    let allow_origin = allow_origin.ok_or_else(|| {
        ObscuraNetError::Cors(format!(
            "{} did not include Access-Control-Allow-Origin for origin {}",
            target, serialized_origin
        ))
    })?;
    if request.credentials != RequestCredentials::Include && allow_origin == "*" {
        return Ok(());
    }
    if allow_origin != serialized_origin {
        return Err(ObscuraNetError::Cors(format!(
            "{} returned Access-Control-Allow-Origin {:?}, expected {:?}",
            target, allow_origin, serialized_origin
        )));
    }
    if request.credentials == RequestCredentials::Include
        && allow_credentials != Some("true")
    {
        return Err(ObscuraNetError::Cors(format!(
            "credentialed response from {} requires Access-Control-Allow-Credentials: true",
            target
        )));
    }
    Ok(())
}

pub(crate) fn response_too_large(url: &Url, limit: usize) -> ObscuraNetError {
    ObscuraNetError::ResponseTooLarge {
        url: url.to_string(),
        limit,
    }
}

pub(crate) fn request_fetch_site(request: &ResourceRequest, target: &Url) -> &'static str {
    let Some(initiator) = request.initiator.as_ref() else {
        return "none";
    };
    if initiator.origin() == target.origin() {
        "same-origin"
    } else {
        // A public-suffix-aware `same-site` classification will be added with
        // the page resource scheduler. Until then, cross-site is the safe
        // conservative value; it never overstates ambient trust.
        "cross-site"
    }
}

pub(crate) fn request_referrer(request: &ResourceRequest, target: &Url) -> Option<String> {
    let source = request
        .referrer
        .as_ref()
        .or(request.initiator.as_ref())?;
    if !matches!(source.scheme(), "http" | "https")
        || !matches!(target.scheme(), "http" | "https")
        || (source.scheme() == "https" && target.scheme() == "http")
    {
        return None;
    }
    if source.origin() == target.origin() {
        let mut value = source.clone();
        let _ = value.set_username("");
        let _ = value.set_password(None);
        value.set_fragment(None);
        Some(value.to_string())
    } else {
        Some(format!("{}/", source.origin().ascii_serialization()))
    }
}

pub type RequestCallback = Arc<dyn Fn(&RequestInfo) + Send + Sync>;
pub type ResponseCallback = Arc<dyn Fn(&RequestInfo, &Response) + Send + Sync>;

/// Page-scoped store for the passive on_request/on_response callbacks (issue
/// #408). Each `Page` owns one, so a callback never fires for another page's
/// requests and dies with its page. The HTTP client itself stays
/// callback-free; page-driven fetches pass the page's registry in. Ids keep
/// the `u64` shape #416 established on `Page::on_request`/`on_response`.
pub struct CallbackRegistry {
    on_request: RwLock<Vec<(u64, RequestCallback)>>,
    on_response: RwLock<Vec<(u64, ResponseCallback)>>,
    id_counter: std::sync::atomic::AtomicU64,
}

impl CallbackRegistry {
    pub fn new() -> Self {
        CallbackRegistry {
            on_request: RwLock::new(Vec::new()),
            on_response: RwLock::new(Vec::new()),
            id_counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        self.id_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Register a request callback; the returned id detaches it via
    /// `remove_request`. Sync like the pre-registry push path: registration
    /// happens from `Page` setup where no reader holds the lock, so
    /// `try_write` cannot fail there.
    pub fn add_request(&self, cb: RequestCallback) -> u64 {
        let id = self.next_id();
        if let Ok(mut v) = self.on_request.try_write() {
            v.push((id, cb));
        }
        id
    }

    /// Register a response callback; see `add_request`.
    pub fn add_response(&self, cb: ResponseCallback) -> u64 {
        let id = self.next_id();
        if let Ok(mut v) = self.on_response.try_write() {
            v.push((id, cb));
        }
        id
    }

    /// Detach a request callback. Returns true when the id was found and
    /// removed, so a double detach is a visible no-op.
    pub fn remove_request(&self, id: u64) -> bool {
        match self.on_request.try_write() {
            Ok(mut v) => {
                let before = v.len();
                v.retain(|(cid, _)| *cid != id);
                v.len() != before
            }
            Err(_) => false,
        }
    }

    /// Detach a response callback; see `remove_request`.
    pub fn remove_response(&self, id: u64) -> bool {
        match self.on_response.try_write() {
            Ok(mut v) => {
                let before = v.len();
                v.retain(|(cid, _)| *cid != id);
                v.len() != before
            }
            Err(_) => false,
        }
    }

    /// True when at least one request callback is registered. Lets fire sites
    /// skip building a `RequestInfo` when nobody listens.
    pub async fn has_request_callbacks(&self) -> bool {
        !self.on_request.read().await.is_empty()
    }

    /// True when at least one response callback is registered.
    pub async fn has_response_callbacks(&self) -> bool {
        !self.on_response.read().await.is_empty()
    }

    pub async fn fire_request(&self, info: &RequestInfo) {
        for (_, cb) in self.on_request.read().await.iter() {
            cb(info);
        }
    }

    pub async fn fire_response(&self, info: &RequestInfo, resp: &Response) {
        for (_, cb) in self.on_response.read().await.iter() {
            cb(info, resp);
        }
    }
}

impl Default for CallbackRegistry {
    fn default() -> Self {
        Self::new()
    }
}

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

/// True when `ip` must never be the target of an outbound request from the
/// engine: loopback, RFC1918 private, link-local (incl. the 169.254.169.254
/// cloud-metadata endpoint), broadcast, documentation, the unspecified address
/// (0.0.0.0 / ::, which the OS routes to localhost), IPv6 unique-local
/// (fc00::/7), and any IPv4-mapped/compatible IPv6 form of the above.
/// Centralizes the SSRF deny-set so the literal-host check and the
/// DNS-resolution check (`SsrfGuardResolver`) can never disagree.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
            {
                return true;
            }
            // Unwrap IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d)
            // forms and re-check the embedded v4 so e.g. [::ffff:127.0.0.1] or
            // [::ffff:169.254.169.254] cannot slip past the v6 arm.
            if let Some(v4) = v6.to_ipv4_mapped().or_else(|| v6.to_ipv4()) {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            false
        }
    }
}

/// DNS resolver that performs the lookup and then rejects the whole request if
/// ANY resolved address is in the SSRF deny-set. This closes the DNS-rebinding
/// bypass a host-string check alone cannot: a public name that resolves to
/// 127.0.0.1 / 169.254.169.254 / an RFC1918 address is blocked at connect time,
/// using the very addresses the client will dial. When private access is
/// permitted (`--allow-private-network` or `OBSCURA_ALLOW_PRIVATE_NETWORK`) the
/// lookup passes through unfiltered.
///
/// Implemented for both transports: `reqwest::dns::Resolve` just below, and
/// `wreq::dns::Resolve` in `wreq_client.rs`, so `--stealth` never trades the
/// guard away for a better TLS fingerprint.
pub struct SsrfGuardResolver {
    pub(crate) allow_private: bool,
}

impl SsrfGuardResolver {
    pub fn new(allow_private: bool) -> Self {
        Self { allow_private }
    }
}

impl Resolve for SsrfGuardResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow = self.allow_private || env_allows_private_network();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();
            if !allow {
                if let Some(bad) = addrs.iter().find(|sa| is_forbidden_ip(sa.ip())) {
                    return Err(format!(
                        "SSRF blocked: '{}' resolves to forbidden address {}",
                        host,
                        bad.ip()
                    )
                    .into());
                }
            }
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

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
                if is_forbidden_ip(IpAddr::V4(ip)) {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to private/internal IP address {} is not allowed",
                        ip
                    )));
                }
            }
            url::Host::Ipv6(ip) => {
                if is_forbidden_ip(IpAddr::V6(ip)) {
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
            }
        }
    }

    Ok(())
}

pub(crate) async fn fetch_file_url(
    url: &Url,
    max_response_bytes: usize,
) -> Result<Response, ObscuraNetError> {
    let path = url
        .to_file_path()
        .map_err(|_| ObscuraNetError::Network("Invalid file URL".to_string()))?;
    if let Ok(metadata) = tokio::fs::metadata(&path).await {
        if metadata.len() > max_response_bytes as u64 {
            return Err(response_too_large(url, max_response_bytes));
        }
    }
    let body = tokio::fs::read(&path)
        .await
        .map_err(|e| ObscuraNetError::Network(format!("Failed to read file: {}", e)))?;
    if body.len() > max_response_bytes {
        return Err(response_too_large(url, max_response_bytes));
    }

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

fn response_header_value<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    url: &Url,
) -> Result<Option<&'a str>, ObscuraNetError> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(ObscuraNetError::Cors(format!(
            "{} returned multiple {} headers",
            url, name
        )));
    }
    first.to_str().map(Some).map_err(|_| {
        ObscuraNetError::Cors(format!("{} returned an invalid {} header", url, name))
    })
}

fn validate_reqwest_cors_response(
    request: &ResourceRequest,
    target: &Url,
    serialized_origin: &str,
    headers: &HeaderMap,
) -> Result<(), ObscuraNetError> {
    if !cors_required(request, target) {
        return Ok(());
    }
    let allow_origin = response_header_value(
        headers,
        "access-control-allow-origin",
        target,
    )?;
    let allow_credentials = response_header_value(
        headers,
        "access-control-allow-credentials",
        target,
    )?;
    validate_cors_response(
        request,
        target,
        serialized_origin,
        allow_origin,
        allow_credentials,
    )
}

fn reject_oversized_content_length(
    headers: &HeaderMap,
    url: &Url,
    limit: usize,
) -> Result<(), ObscuraNetError> {
    let Some(value) = headers.get(reqwest::header::CONTENT_LENGTH) else {
        return Ok(());
    };
    let Ok(value) = value.to_str() else {
        return Ok(());
    };
    if value
        .trim()
        .parse::<u64>()
        .is_ok_and(|length| length > limit as u64)
    {
        return Err(response_too_large(url, limit));
    }
    Ok(())
}

async fn read_reqwest_body_limited(
    mut response: reqwest::Response,
    url: &Url,
    limit: usize,
) -> Result<Vec<u8>, ObscuraNetError> {
    reject_oversized_content_length(response.headers(), url, limit)?;
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit);
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        ObscuraNetError::Network(format!("Failed to read body: {}", error))
    })? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(response_too_large(url, limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub struct ObscuraHttpClient {
    client: tokio::sync::OnceCell<Client>,
    proxy_url: Option<String>,
    pub cookie_jar: Arc<CookieJar>,
    pub user_agent: RwLock<String>,
    pub accept_language: RwLock<String>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub interceptor: RwLock<Option<Box<dyn RequestInterceptor + Send + Sync>>>,
    pub timeout: Duration,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
    pub block_trackers: bool,
    resource_loader: std::sync::Mutex<ResourceLoaderState>,
    /// When true, `validate_url` lets localhost / RFC1918 / link-local addresses
    /// through in addition to the `OBSCURA_ALLOW_PRIVATE_NETWORK` env var.
    /// Set via `--allow-private-network` on the CLI (issue #33).
    pub allow_private_network: bool,
}

const RESOURCE_CACHE_MAX_ENTRIES: usize = 256;
const RESOURCE_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ResourceCacheKey {
    url: String,
    resource_type: ResourceType,
    mode: RequestMode,
    credentials: RequestCredentials,
    initiator: Option<String>,
    referrer: Option<String>,
    user_agent: String,
    extra_headers: Vec<(String, String)>,
    max_response_bytes: usize,
}

#[derive(Clone)]
struct ResourceCacheEntry {
    response: Response,
    expires_at: Instant,
}

#[derive(Default)]
struct ResourceCache {
    entries: HashMap<ResourceCacheKey, ResourceCacheEntry>,
    insertion_order: VecDeque<ResourceCacheKey>,
    body_bytes: usize,
}

#[derive(Default)]
struct ResourceLoaderState {
    cache: ResourceCache,
    shared_fetches: HashMap<ResourceCacheKey, SharedFetchSender>,
}

#[derive(Clone)]
enum SharedFetchOutcome {
    Cacheable(Response),
    RetryUncoalesced,
}

type SharedFetchSender = watch::Sender<Option<SharedFetchOutcome>>;

struct SharedFetchLeader<'a> {
    loader: &'a std::sync::Mutex<ResourceLoaderState>,
    key: ResourceCacheKey,
    sender: SharedFetchSender,
    finished: bool,
}

impl SharedFetchLeader<'_> {
    fn finish(mut self, outcome: SharedFetchOutcome) {
        self.loader.lock().unwrap().shared_fetches.remove(&self.key);
        let _ = self.sender.send(Some(outcome));
        self.finished = true;
    }
}

impl Drop for SharedFetchLeader<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.loader.lock().unwrap().shared_fetches.remove(&self.key);
        let _ = self.sender.send(Some(SharedFetchOutcome::RetryUncoalesced));
    }
}

impl ResourceCache {
    fn get(&mut self, key: &ResourceCacheKey) -> Option<Response> {
        let entry = self.entries.get(key)?;
        if entry.expires_at <= Instant::now() {
            let expired = self.entries.remove(key)?;
            self.body_bytes = self.body_bytes.saturating_sub(expired.response.body.len());
            return None;
        }
        Some(entry.response.clone())
    }

    fn insert(&mut self, key: ResourceCacheKey, response: Response, lifetime: Duration) {
        let response_bytes = response.body.len();
        if response_bytes > RESOURCE_CACHE_MAX_BYTES {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.body_bytes = self.body_bytes.saturating_sub(previous.response.body.len());
            self.insertion_order.retain(|queued| queued != &key);
        }
        while self.entries.len() >= RESOURCE_CACHE_MAX_ENTRIES
            || self.body_bytes.saturating_add(response_bytes) > RESOURCE_CACHE_MAX_BYTES
        {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.body_bytes = self.body_bytes.saturating_sub(entry.response.body.len());
            }
        }
        self.body_bytes = self.body_bytes.saturating_add(response_bytes);
        self.insertion_order.push_back(key.clone());
        self.entries.insert(
            key,
            ResourceCacheEntry {
                response,
                expires_at: Instant::now() + lifetime,
            },
        );
    }
}

fn response_cache_lifetime(response: &Response) -> Option<Duration> {
    if !(200..300).contains(&response.status)
        || !response.redirected_from.is_empty()
        || response.header("set-cookie").is_some()
        || response
            .header("vary")
            .is_some_and(|vary| vary.split(',').any(|name| name.trim() == "*"))
    {
        return None;
    }
    let cache_control = response.header("cache-control")?;
    let mut max_age = None;
    for directive in cache_control.split(',').map(str::trim) {
        let lower = directive.to_ascii_lowercase();
        if lower == "no-store" || lower == "no-cache" {
            return None;
        }
        if let Some(value) = lower.strip_prefix("max-age=") {
            max_age = value.trim_matches('"').parse::<u64>().ok();
        }
    }
    max_age
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}

/// Derive the sec-ch-ua and sec-ch-ua-platform client-hint header values from a
/// User-Agent string, using Chromium's per-major-version GREASE algorithm so
/// the non-stealth HTTP path agrees with navigator.userAgentData instead of
/// shipping a fixed Linux/Chrome-145 hint that contradicts a Windows profile.
fn chrome_client_hints(ua: &str) -> (String, String) {
    let major: usize = ua
        .split("Chrome/")
        .nth(1)
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(145);
    const GREASE_CHARS: [char; 11] = [' ', '(', ':', '-', '.', '/', ')', ';', '=', '?', '_'];
    const GREASE_VER: [&str; 3] = ["8", "99", "24"];
    const PERMS: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    let grease_brand = format!(
        "Not{}A{}Brand",
        GREASE_CHARS[major % 11],
        GREASE_CHARS[(major + 1) % 11]
    );
    let brands = [
        (grease_brand, GREASE_VER[major % 3].to_string()),
        ("Chromium".to_string(), major.to_string()),
        ("Google Chrome".to_string(), major.to_string()),
    ];
    let p = PERMS[major % 6];
    let sec_ch_ua = p
        .iter()
        .map(|&i| format!("\"{}\";v=\"{}\"", brands[i].0, brands[i].1))
        .collect::<Vec<_>>()
        .join(", ");
    let platform = if ua.contains("Windows NT") {
        "\"Windows\""
    } else if ua.contains("Macintosh") {
        "\"macOS\""
    } else {
        "\"Linux\""
    };
    (sec_ch_ua, platform.to_string())
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
            accept_language: RwLock::new("en-US,en;q=0.9".to_string()),
            extra_headers: RwLock::new(HashMap::new()),
            interceptor: RwLock::new(None),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            timeout: Duration::from_secs(30),
            block_trackers: false,
            resource_loader: std::sync::Mutex::new(ResourceLoaderState::default()),
            allow_private_network,
        }
    }

    async fn get_client(&self) -> &Client {
        self.client.get_or_init(|| async {
            let mut builder = Client::builder()
                .redirect(Policy::none())
                .timeout(self.timeout)
                .danger_accept_invalid_certs(false)
                // SSRF guard: reject hostnames that resolve to a private/loopback IP.
                .dns_resolver(Arc::new(SsrfGuardResolver::new(self.allow_private_network)))
;

            if std::env::var_os("SSL_CERT_FILE").is_some()
                || std::env::var_os("SSL_CERT_DIR").is_some()
            {
                for certificate in configured_root_certificates() {
                    builder = builder.add_root_certificate(certificate.clone());
                }
            }

            if let Some(ref proxy) = self.proxy_url {
                if let Ok(p) = reqwest::Proxy::all(proxy.as_str()) {
                    builder = builder.proxy(p);
                }
            }

            builder.build().expect("failed to build HTTP client")
        }).await
    }

    /// Clone the request client owned by this browser context.
    ///
    /// Scripted fetch/XHR uses the same pool as navigation instead of a
    /// process-global client. This keeps its async network state inside the
    /// same ownership boundary as the V8 runtime (issue #453).
    pub async fn request_client(&self) -> Client {
        self.get_client().await.clone()
    }

    /// Read-only accessor for the proxy URL the client was configured with
    /// (if any). Exposed so callers outside the `obscura-net` crate — notably
    /// `op_fetch_url` in `obscura-js` (#139) — can route their own reqwest
    /// requests through the same upstream proxy.
    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::GET, url, None, None).await
    }

    /// `fetch` that also fires the page's passive on_request/on_response
    /// callbacks (issue #408: callbacks are page-scoped, so the page-driven
    /// fetch paths pass their registry in).
    pub async fn fetch_with_callbacks(
        &self,
        url: &Url,
        callbacks: Option<&CallbackRegistry>,
    ) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::GET, url, None, callbacks).await
    }

    pub async fn post_form(&self, url: &Url, body: &str) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::POST, url, Some(body.as_bytes().to_vec()), None).await
    }

    /// `post_form` variant of `fetch_with_callbacks`.
    pub async fn post_form_with_callbacks(
        &self,
        url: &Url,
        body: &str,
        callbacks: Option<&CallbackRegistry>,
    ) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::POST, url, Some(body.as_bytes().to_vec()), callbacks)
            .await
    }

    pub async fn fetch_with_method(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
        callbacks: Option<&CallbackRegistry>,
    ) -> Result<Response, ObscuraNetError> {
        self.fetch_with_profile(
            initial_method,
            url,
            initial_body,
            callbacks,
            ResourceRequest::navigation(),
        )
        .await
    }

    /// Fetch a non-navigation resource through the same validated client,
    /// cookie jar, proxy, connection pool, interception and callback path as
    /// the owning page. The renderer can seed its byte cache from this result
    /// instead of opening a second synchronous HTTP stack.
    pub async fn fetch_resource_with_callbacks(
        &self,
        url: &Url,
        request: ResourceRequest,
        callbacks: Option<&CallbackRegistry>,
    ) -> Result<Response, ObscuraNetError> {
        self.fetch_with_profile(Method::GET, url, None, callbacks, request)
            .await
    }

    async fn resource_cache_key(
        &self,
        method: &Method,
        url: &Url,
        body: &Option<Vec<u8>>,
        request: &ResourceRequest,
    ) -> Option<ResourceCacheKey> {
        if *method != Method::GET
            || body.is_some()
            || request.resource_type == ResourceType::Document
            || !matches!(url.scheme(), "http" | "https")
            || self.interceptor.read().await.is_some()
        {
            return None;
        }
        let mut extra_headers = self
            .extra_headers
            .read()
            .await
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
            .collect::<Vec<_>>();
        extra_headers.sort();
        if extra_headers.iter().any(|(name, value)| {
            name == "authorization"
                || name == "cookie"
                || (name == "cache-control"
                    && (value.to_ascii_lowercase().contains("no-cache")
                        || value.to_ascii_lowercase().contains("no-store")))
        }) {
            return None;
        }
        if request.sends_credentials_to(url) && !self.cookie_jar.get_cookie_header(url).is_empty() {
            return None;
        }
        Some(ResourceCacheKey {
            url: url.to_string(),
            resource_type: request.resource_type,
            mode: request.mode,
            credentials: request.credentials,
            initiator: request.initiator.as_ref().map(ToString::to_string),
            referrer: request.referrer.as_ref().map(ToString::to_string),
            user_agent: self.user_agent.read().await.clone(),
            extra_headers,
            max_response_bytes: request.max_response_bytes,
        })
    }

    async fn fetch_with_profile(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
        callbacks: Option<&CallbackRegistry>,
        request: ResourceRequest,
    ) -> Result<Response, ObscuraNetError> {
        let Some(cache_key) = self
            .resource_cache_key(&initial_method, url, &initial_body, &request)
            .await
        else {
            return self
                .fetch_with_profile_uncached(initial_method, url, initial_body, callbacks, request)
                .await;
        };

        enum Acquisition {
            Cached(Response),
            Follower(watch::Receiver<Option<SharedFetchOutcome>>),
            Leader(SharedFetchSender),
        }

        // Cache lookup and leader election are one critical section.  Without
        // that atomicity a request can miss the cache, pause, and install a
        // second leader after the first request has already populated it.
        let acquisition = {
            let mut loader = self.resource_loader.lock().unwrap();
            if let Some(response) = loader.cache.get(&cache_key) {
                Acquisition::Cached(response)
            } else if let Some(sender) = loader.shared_fetches.get(&cache_key) {
                Acquisition::Follower(sender.subscribe())
            } else {
                let (sender, _receiver) = watch::channel(None);
                loader
                    .shared_fetches
                    .insert(cache_key.clone(), sender.clone());
                Acquisition::Leader(sender)
            }
        };

        match acquisition {
            Acquisition::Cached(response) => {
                self.fire_logical_resource_callbacks(callbacks, url, &request, &response)
                    .await;
                Ok(response)
            }
            Acquisition::Follower(mut receiver) => loop {
                let outcome = { receiver.borrow().clone() };
                if let Some(outcome) = outcome {
                    break match outcome {
                        SharedFetchOutcome::Cacheable(response) => {
                            self.fire_logical_resource_callbacks(
                                callbacks, url, &request, &response,
                            )
                            .await;
                            Ok(response)
                        }
                        SharedFetchOutcome::RetryUncoalesced => {
                            self.fetch_with_profile_uncached(
                                initial_method,
                                url,
                                initial_body,
                                callbacks,
                                request,
                            )
                            .await
                        }
                    };
                }
                if receiver.changed().await.is_err() {
                    break self
                        .fetch_with_profile_uncached(
                            initial_method,
                            url,
                            initial_body,
                            callbacks,
                            request,
                        )
                        .await;
                }
            },
            Acquisition::Leader(sender) => {
                // If this future is cancelled or panics while awaiting I/O,
                // the guard removes the stale leader and wakes followers so
                // they can retry instead of waiting forever.
                let leader = SharedFetchLeader {
                    loader: &self.resource_loader,
                    key: cache_key.clone(),
                    sender,
                    finished: false,
                };
                let result = self
                    .fetch_with_profile_uncached(
                        initial_method,
                        url,
                        initial_body,
                        callbacks,
                        request,
                    )
                    .await;
                let shared_outcome = match &result {
                    Ok(response) => match response_cache_lifetime(response) {
                        Some(lifetime) => {
                            self.resource_loader.lock().unwrap().cache.insert(
                                cache_key,
                                response.clone(),
                                lifetime,
                            );
                            SharedFetchOutcome::Cacheable(response.clone())
                        }
                        None => SharedFetchOutcome::RetryUncoalesced,
                    },
                    Err(_) => SharedFetchOutcome::RetryUncoalesced,
                };
                leader.finish(shared_outcome);
                result
            }
        }
    }

    /// A cache hit or shared transport still represents an independent page
    /// request.  Keep passive request/response observers at logical-resource
    /// granularity even when only one HTTP transaction reaches the server.
    async fn fire_logical_resource_callbacks(
        &self,
        callbacks: Option<&CallbackRegistry>,
        url: &Url,
        request: &ResourceRequest,
        response: &Response,
    ) {
        let Some(callbacks) = callbacks else {
            return;
        };
        let request_info = RequestInfo {
            url: url.clone(),
            method: Method::GET.to_string(),
            headers: self.extra_headers.read().await.clone(),
            resource_type: request.resource_type,
        };
        callbacks.fire_request(&request_info).await;
        callbacks.fire_response(&request_info, response).await;
    }

    async fn fetch_with_profile_uncached(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
        callbacks: Option<&CallbackRegistry>,
        request: ResourceRequest,
    ) -> Result<Response, ObscuraNetError> {
        validate_url(url, self.allow_private_network)?;
        validate_request_mode(&request, url)?;

        if url.scheme() == "file" {
            return fetch_file_url(url, request.max_response_bytes).await;
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
        let mut redirect_tainted = false;
        let mut request_callback_fired = false;

        for _redirect_count in 0..max_redirects {
            validate_request_mode(&request, &current_url)?;
            let request_info = RequestInfo {
                url: current_url.clone(),
                method: method.to_string(),
                headers: self.extra_headers.read().await.clone(),
                resource_type: request.resource_type.clone(),
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

            if !request_callback_fired {
                if let Some(cbs) = callbacks {
                    cbs.fire_request(&request_info).await;
                }
                request_callback_fired = true;
            }

            let ua = self.user_agent.read().await.clone();
            let (sec_ch_ua, sec_ch_ua_platform) = chrome_client_hints(&ua);
            let mut headers = HeaderMap::new();
            // Chrome's top-level navigation header order. (reqwest appends
            // accept-encoding/host after these, so accept-encoding lands after
            // accept-language rather than before it; the rest matches Chrome.)
            headers.insert(
                HeaderName::from_static("sec-ch-ua"),
                HeaderValue::from_str(&sec_ch_ua)
                    .unwrap_or_else(|_| HeaderValue::from_static("\"Not:A-Brand\";v=\"99\", \"Google Chrome\";v=\"145\", \"Chromium\";v=\"145\"")),
            );
            headers.insert(HeaderName::from_static("sec-ch-ua-mobile"), HeaderValue::from_static("?0"));
            headers.insert(
                HeaderName::from_static("sec-ch-ua-platform"),
                HeaderValue::from_str(&sec_ch_ua_platform)
                    .unwrap_or_else(|_| HeaderValue::from_static("\"Windows\"")),
            );
            if request.mode == RequestMode::Navigate {
                headers.insert(HeaderName::from_static("upgrade-insecure-requests"), HeaderValue::from_static("1"));
            }
            headers.insert(USER_AGENT, HeaderValue::from_str(&ua).unwrap_or_else(|_| {
                HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36")
            }));
            headers.insert(
                reqwest::header::ACCEPT,
                HeaderValue::from_static(request.accept()),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-site"),
                HeaderValue::from_static(request_fetch_site(&request, &current_url)),
            );
            headers.insert(
                HeaderName::from_static("sec-fetch-mode"),
                HeaderValue::from_static(request.mode.header_value()),
            );
            if request.mode == RequestMode::Navigate {
                headers.insert(HeaderName::from_static("sec-fetch-user"), HeaderValue::from_static("?1"));
            }
            headers.insert(
                HeaderName::from_static("sec-fetch-dest"),
                HeaderValue::from_static(request.destination()),
            );
            if let Some(referer) = request_referrer(&request, &current_url) {
                if let Ok(value) = HeaderValue::from_str(&referer) {
                    headers.insert(reqwest::header::REFERER, value);
                }
            }
            let request_origin = serialized_request_origin(&request, redirect_tainted);
            let accept_language = self.accept_language.read().await.clone();
            if let Ok(value) = HeaderValue::from_str(&accept_language) {
                if !accept_language.is_empty() {
                    headers.insert(reqwest::header::ACCEPT_LANGUAGE, value);
                }
            }

            let cookie_header = if request.sends_credentials_to(&current_url) {
                self.cookie_jar.get_cookie_header(&current_url)
            } else {
                String::new()
            };
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
            // Origin is a forbidden browser request header. Keep it derived
            // from the initiator even when callers supplied extra headers.
            if cors_required(&request, &current_url) {
                if let Ok(value) = HeaderValue::from_str(&request_origin) {
                    headers.insert(reqwest::header::ORIGIN, value);
                }
            } else {
                headers.remove(reqwest::header::ORIGIN);
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

            let in_flight = InFlightGuard::new(&self.in_flight);
            let resp = req_builder.send().await.map_err(|e| {
                ObscuraNetError::Network(format!("{}: {}", current_url, e))
            })?;

            let status = resp.status();
            validate_reqwest_cors_response(
                &request,
                &current_url,
                &request_origin,
                resp.headers(),
            )?;

            if request.sends_credentials_to(&current_url) {
                for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                    if let Ok(s) = val.to_str() {
                        self.cookie_jar.set_cookie(s, &current_url);
                    }
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
                    validate_request_mode(&request, &next_url)?;
                    redirect_tainted |=
                        redirect_taints_origin(&request, &current_url, &next_url);
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

            let body_bytes = read_reqwest_body_limited(
                resp,
                &current_url,
                request.max_response_bytes,
            )
            .await?;
            drop(in_flight);

            let response = Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body: body_bytes,
                redirected_from: redirects,
            };

            if let Some(cbs) = callbacks {
                cbs.fire_response(&request_info, &response).await;
            }

            return Ok(response);
        }

        Err(ObscuraNetError::TooManyRedirects(current_url.to_string()))
    }

    pub async fn set_user_agent(&self, ua: &str) {
        *self.user_agent.write().await = ua.to_string();
    }

    pub async fn set_accept_language(&self, accept_language: &str) {
        *self.accept_language.write().await = accept_language.to_string();
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

    #[error("CORS error: {0}")]
    Cors(String),

    #[error("Response body exceeded {limit} byte limit: {url}")]
    ResponseTooLarge { url: String, limit: usize },
}

#[cfg(test)]
mod ssrf_tests {
    use super::{
        is_forbidden_ip, request_fetch_site, request_referrer, validate_url,
        CallbackRegistry, ObscuraHttpClient, ObscuraNetError, RequestCredentials, RequestMode,
        ResourceRequest, ResourceType, SsrfGuardResolver,
    };
    use crate::cookies::CookieJar;
    use reqwest::dns::{Name, Resolve};
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use url::Url;

    fn ip(s: &str) -> IpAddr {
        IpAddr::from_str(s).unwrap()
    }

    #[test]
    fn ipv4_private_and_special_ranges_are_forbidden() {
        for s in [
            "127.0.0.1",
            "127.5.6.7",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254", // cloud-metadata endpoint
            "0.0.0.0",         // unspecified -> localhost (was a bypass)
            "255.255.255.255", // broadcast
            "192.0.2.1",       // documentation
        ] {
            assert!(is_forbidden_ip(ip(s)), "{s} should be forbidden");
        }
    }

    #[test]
    fn public_ipv4_is_allowed() {
        for s in ["1.1.1.1", "8.8.8.8", "93.184.216.34"] {
            assert!(!is_forbidden_ip(ip(s)), "{s} should be allowed");
        }
    }

    #[test]
    fn ipv6_loopback_ula_linklocal_and_mapped_are_forbidden() {
        for s in [
            "::1",                    // loopback
            "::",                     // unspecified
            "fc00::1",                // unique-local (was a bypass)
            "fd12:3456:789a::1",      // unique-local
            "fe80::1",                // link-local
            "::ffff:127.0.0.1",       // v4-mapped loopback (was a bypass)
            "::ffff:169.254.169.254", // v4-mapped metadata
        ] {
            assert!(is_forbidden_ip(ip(s)), "{s} should be forbidden");
        }
    }

    #[test]
    fn public_ipv6_is_allowed() {
        assert!(!is_forbidden_ip(ip("2606:4700:4700::1111"))); // cloudflare dns
    }

    #[test]
    fn validate_url_blocks_unspecified_and_allows_public() {
        // 0.0.0.0 previously slipped through validate_url's literal-host check.
        assert!(validate_url(&Url::parse("http://0.0.0.0:8080/").unwrap(), false).is_err());
        assert!(validate_url(&Url::parse("http://127.0.0.1/").unwrap(), false).is_err());
        assert!(validate_url(&Url::parse("http://example.com/").unwrap(), false).is_ok());
        // The allow flag bypasses the guard (local-dev escape hatch).
        assert!(validate_url(&Url::parse("http://127.0.0.1/").unwrap(), true).is_ok());
    }

    #[test]
    fn resource_profiles_use_type_specific_fetch_metadata() {
        let document = Url::parse("https://app.example/page?q=1#fragment").unwrap();
        let image = ResourceRequest::subresource(ResourceType::Image, &document);
        assert_eq!(image.mode, RequestMode::NoCors);
        assert_eq!(image.credentials, RequestCredentials::Include);
        assert_eq!(image.destination(), "image");
        assert!(image.accept().starts_with("image/webp"));

        let stylesheet = ResourceRequest::subresource(ResourceType::Stylesheet, &document);
        assert_eq!(stylesheet.destination(), "style");
        assert_eq!(stylesheet.accept(), "text/css,*/*;q=0.1");

        let font = ResourceRequest::subresource(ResourceType::Font, &document);
        assert_eq!(font.mode, RequestMode::Cors);
        assert_eq!(font.credentials, RequestCredentials::SameOrigin);
        assert_eq!(font.destination(), "font");
        assert_eq!(font.accept(), "*/*");

        assert!(image.sends_credentials_to(
            &Url::parse("https://cdn.example/image.png").unwrap()
        ));
        assert!(font.sends_credentials_to(
            &Url::parse("https://app.example/font.woff2").unwrap()
        ));
        assert!(!font.sends_credentials_to(
            &Url::parse("https://cdn.example/font.woff2").unwrap()
        ));

        let module = ResourceRequest::module_script(&document, &document);
        assert_eq!(module.resource_type, ResourceType::Script);
        assert_eq!(module.mode, RequestMode::Cors);
        assert_eq!(module.credentials, RequestCredentials::SameOrigin);
        assert_eq!(module.destination(), "script");
        assert_eq!(module.accept(), "*/*");
        assert!(module.sends_credentials_to(
            &Url::parse("https://app.example/chunk.js").unwrap()
        ));
        assert!(!module.sends_credentials_to(
            &Url::parse("https://cdn.example/chunk.js").unwrap()
        ));
    }

    #[test]
    fn subresource_referrer_and_fetch_site_follow_default_browser_policy() {
        let source = Url::parse("https://user:secret@app.example/path?q=1#frag").unwrap();
        let request = ResourceRequest::subresource(ResourceType::Image, &source);
        let same_origin = Url::parse("https://app.example/image.png").unwrap();
        let cross_origin = Url::parse("https://cdn.example/image.png").unwrap();
        let downgrade = Url::parse("http://cdn.example/image.png").unwrap();

        assert_eq!(request_fetch_site(&request, &same_origin), "same-origin");
        assert_eq!(request_fetch_site(&request, &cross_origin), "cross-site");
        assert_eq!(
            request_referrer(&request, &same_origin).as_deref(),
            Some("https://app.example/path?q=1")
        );
        assert_eq!(
            request_referrer(&request, &cross_origin).as_deref(),
            Some("https://app.example/")
        );
        assert_eq!(request_referrer(&request, &downgrade), None);
    }

    async fn http_fixture(
        responses: Vec<String>,
    ) -> (Url, tokio::sync::mpsc::UnboundedReceiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                loop {
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = request_tx.send(String::from_utf8_lossy(&request).into_owned());
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (
            Url::parse(&format!("http://{address}/resource")).unwrap(),
            request_rx,
        )
    }

    fn ok_response(headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn cross_origin_font_sends_origin_and_omits_cross_origin_cookies() {
        let (target, mut received) = http_fixture(vec![ok_response(
            "Access-Control-Allow-Origin: *\r\nSet-Cookie: rejected=1; Path=/\r\n",
            "font",
        )])
        .await;
        let initiator = Url::parse("http://127.0.0.1:1/page").unwrap();
        let jar = Arc::new(CookieJar::new());
        jar.set_cookie("seed=1; Path=/", &target);
        let client = ObscuraHttpClient::with_full_options(jar.clone(), None, true);

        let response = client
            .fetch_resource_with_callbacks(
                &target,
                ResourceRequest::subresource(ResourceType::Font, &initiator),
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.body, b"font");
        let request = received.recv().await.unwrap().to_ascii_lowercase();
        assert!(request.contains("origin: http://127.0.0.1:1\r\n"));
        assert!(request.contains("sec-fetch-mode: cors\r\n"));
        assert!(request.contains("sec-fetch-dest: font\r\n"));
        assert!(!request.contains("cookie:"));
        assert_eq!(jar.get_cookie_header(&target), "seed=1");
    }

    #[tokio::test]
    async fn cross_origin_module_uses_cors_script_profile_without_credentials() {
        let (target, mut received) = http_fixture(vec![ok_response(
            "Access-Control-Allow-Origin: *\r\nSet-Cookie: rejected=1; Path=/\r\n",
            "export default 1;",
        )])
        .await;
        let initiator = Url::parse("http://127.0.0.1:1/page").unwrap();
        let importing_module = target.join("/parent.js").unwrap();
        let jar = Arc::new(CookieJar::new());
        jar.set_cookie("seed=1; Path=/", &target);
        let client = ObscuraHttpClient::with_full_options(jar.clone(), None, true);

        let response = client
            .fetch_resource_with_callbacks(
                &target,
                ResourceRequest::module_script(&initiator, &importing_module),
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.body, b"export default 1;");
        let request = received.recv().await.unwrap().to_ascii_lowercase();
        assert!(request.contains("origin: http://127.0.0.1:1\r\n"));
        assert!(request.contains("sec-fetch-mode: cors\r\n"));
        assert!(request.contains("sec-fetch-dest: script\r\n"));
        assert!(request.contains(&format!("referer: {}\r\n", importing_module)));
        assert!(!request.contains("cookie:"));
        assert_eq!(jar.get_cookie_header(&target), "seed=1");
    }

    #[tokio::test]
    async fn credentialed_cors_rejects_wildcard_and_accepts_exact_origin() {
        let initiator = Url::parse("http://127.0.0.1:1/page").unwrap();
        let wildcard = ok_response(
            "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Credentials: true\r\n",
            "blocked",
        );
        let exact = ok_response(
            "Access-Control-Allow-Origin: http://127.0.0.1:1\r\nAccess-Control-Allow-Credentials: true\r\nSet-Cookie: accepted=1; Path=/\r\n",
            "allowed",
        );
        let (target, mut received) = http_fixture(vec![wildcard, exact]).await;
        let jar = Arc::new(CookieJar::new());
        jar.set_cookie("seed=1; Path=/", &target);
        let client = ObscuraHttpClient::with_full_options(jar.clone(), None, true);
        let mut request = ResourceRequest::subresource(ResourceType::Image, &initiator);
        request.mode = RequestMode::Cors;
        request.credentials = RequestCredentials::Include;

        let error = client
            .fetch_resource_with_callbacks(&target, request.clone(), None)
            .await
            .unwrap_err();
        assert!(matches!(error, ObscuraNetError::Cors(_)));
        client
            .fetch_resource_with_callbacks(&target, request, None)
            .await
            .unwrap();

        let first = received.recv().await.unwrap().to_ascii_lowercase();
        let second = received.recv().await.unwrap().to_ascii_lowercase();
        assert!(first.contains("cookie: seed=1\r\n"));
        assert!(second.contains("cookie: seed=1\r\n"));
        let cookies = jar.get_cookie_header(&target);
        assert!(cookies.contains("seed=1"));
        assert!(cookies.contains("accepted=1"));
    }

    #[tokio::test]
    async fn same_origin_font_needs_no_cors_header_and_sends_cookies() {
        let (target, mut received) = http_fixture(vec![ok_response("", "same")]).await;
        let mut initiator = target.clone();
        initiator.set_path("/page");
        let jar = Arc::new(CookieJar::new());
        jar.set_cookie("same=1; Path=/", &target);
        let client = ObscuraHttpClient::with_full_options(jar, None, true);
        client
            .fetch_resource_with_callbacks(
                &target,
                ResourceRequest::subresource(ResourceType::Font, &initiator),
                None,
            )
            .await
            .unwrap();
        let request = received.recv().await.unwrap().to_ascii_lowercase();
        assert!(!request.contains("origin:"));
        assert!(request.contains("cookie: same=1\r\n"));
    }

    #[tokio::test]
    async fn accept_language_override_reaches_the_wire() {
        let (target, mut received) = http_fixture(vec![ok_response("", "ok")]).await;
        let client = ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        );
        client.set_accept_language("de-DE,de;q=0.9").await;

        client.fetch(&target).await.unwrap();

        let request = received.recv().await.unwrap();
        assert!(
            request
                .lines()
                .any(|line| line.eq_ignore_ascii_case("accept-language: de-DE,de;q=0.9")),
            "request did not contain the configured Accept-Language header: {request}"
        );
    }

    #[tokio::test]
    async fn response_limits_reject_content_length_and_streamed_overflow() {
        let advertised = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n";
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n4\r\nefgh\r\n0\r\n\r\n";
        let (target, _) = http_fixture(vec![advertised.to_string(), chunked.to_string()]).await;
        let client = ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        );
        let initiator = target.clone();
        let request = ResourceRequest::subresource(ResourceType::Image, &initiator)
            .with_max_response_bytes(6);

        for _ in 0..2 {
            let error = client
                .fetch_resource_with_callbacks(&target, request.clone(), None)
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                ObscuraNetError::ResponseTooLarge { limit: 6, .. }
            ));
            assert_eq!(client.active_requests(), 0);
        }
    }

    async fn hanging_fixture() -> (Url, tokio::sync::oneshot::Receiver<()>) {
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer).await;
            let _ = started_tx.send(());
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        (
            Url::parse(&format!("http://{address}/hang")).unwrap(),
            started_rx,
        )
    }

    async fn cancelled_shared_fetch_fixture(
    ) -> (Url, tokio::sync::oneshot::Receiver<()>, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut first_stream = None;
            let mut started_tx = Some(started_tx);
            for index in 0..3 {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).await;
                observed.fetch_add(1, Ordering::SeqCst);
                if index == 0 {
                    if let Some(started_tx) = started_tx.take() {
                        let _ = started_tx.send(());
                    }
                    // Hold the transport open until the leader task is
                    // cancelled. The next two connections prove both the
                    // waiting follower retry and a fresh cache leader work.
                    first_stream = Some(stream);
                    continue;
                }
                let body = "shared";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nCache-Control: public, max-age=3600\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
            drop(first_stream);
        });
        (
            Url::parse(&format!("http://{address}/shared.js")).unwrap(),
            started_rx,
            requests,
        )
    }

    #[tokio::test]
    async fn cancellation_returns_active_requests_to_zero() {
        let (target, started) = hanging_fixture().await;
        let client = Arc::new(ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        ));
        let task = tokio::spawn({
            let client = client.clone();
            async move { client.fetch(&target).await }
        });
        started.await.unwrap();
        assert_eq!(client.active_requests(), 1);
        task.abort();
        let _ = task.await;
        assert_eq!(client.active_requests(), 0);
    }

    #[tokio::test]
    async fn cancelled_shared_subresource_leader_wakes_follower_and_clears_slot() {
        let (target, started, network_requests) = cancelled_shared_fetch_fixture().await;
        let initiator = target.join("/page.html").unwrap();
        let request = ResourceRequest::subresource(ResourceType::Script, &initiator);
        let client = Arc::new(ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        ));

        let leader = tokio::spawn({
            let client = client.clone();
            let target = target.clone();
            let request = request.clone();
            async move {
                client
                    .fetch_resource_with_callbacks(&target, request, None)
                    .await
            }
        });
        started.await.unwrap();

        let follower = tokio::spawn({
            let client = client.clone();
            let target = target.clone();
            let request = request.clone();
            async move {
                client
                    .fetch_resource_with_callbacks(&target, request, None)
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let follower_is_waiting = client
                    .resource_loader
                    .lock()
                    .unwrap()
                    .shared_fetches
                    .values()
                    .next()
                    .is_some_and(|sender| sender.receiver_count() > 0);
                if follower_is_waiting {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("follower did not join the shared fetch");

        leader.abort();
        let _ = leader.await;
        let response = tokio::time::timeout(Duration::from_secs(2), follower)
            .await
            .expect("follower remained blocked after leader cancellation")
            .unwrap()
            .unwrap();
        assert_eq!(response.body, b"shared");

        // The follower intentionally retried without populating the cache.
        // A subsequent request must be able to install a fresh leader, and
        // its successful response is then reusable.
        client
            .fetch_resource_with_callbacks(&target, request.clone(), None)
            .await
            .unwrap();
        client
            .fetch_resource_with_callbacks(&target, request, None)
            .await
            .unwrap();
        assert_eq!(network_requests.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn transport_timeout_returns_active_requests_to_zero() {
        let (target, started) = hanging_fixture().await;
        let mut client = ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        );
        client.timeout = std::time::Duration::from_millis(25);
        let fetch = client.fetch(&target);
        let (_, result) = tokio::join!(started, fetch);
        assert!(result.is_err());
        assert_eq!(client.active_requests(), 0);
    }

    #[tokio::test]
    async fn callbacks_fire_once_across_redirects() {
        let redirect = "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (target, _) = http_fixture(vec![redirect.to_string(), ok_response("", "done")]).await;
        let client = ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        );
        let callbacks = CallbackRegistry::new();
        let requests = Arc::new(AtomicUsize::new(0));
        let responses = Arc::new(AtomicUsize::new(0));
        let request_count = requests.clone();
        callbacks.add_request(Arc::new(move |_| {
            request_count.fetch_add(1, Ordering::SeqCst);
        }));
        let response_count = responses.clone();
        callbacks.add_response(Arc::new(move |_, _| {
            response_count.fetch_add(1, Ordering::SeqCst);
        }));

        client
            .fetch_with_callbacks(&target, Some(&callbacks))
            .await
            .unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(responses.load(Ordering::SeqCst), 1);
    }

    async fn cacheable_resource_fixture(
        status: u16,
        headers: &'static str,
    ) -> (Url, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let observed = observed.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 2048];
                    let _ = stream.read(&mut request).await;
                    observed.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    let body = "globalThis.__sharedRuns=(globalThis.__sharedRuns||0)+1;";
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n{body}",
                        body.len(),
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (
            Url::parse(&format!("http://{address}/shared.js")).unwrap(),
            requests,
        )
    }

    #[tokio::test]
    async fn cacheable_identical_subresources_share_one_in_flight_request() {
        let (url, network_requests) = cacheable_resource_fixture(
            200,
            "Cache-Control: public, max-age=3600\r\nVary: Accept-Language\r\n",
        )
        .await;
        let initiator = url.join("/page.html").unwrap();
        let client = Arc::new(ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        ));
        let callbacks = Arc::new(CallbackRegistry::new());
        let callback_requests = Arc::new(AtomicUsize::new(0));
        let callback_responses = Arc::new(AtomicUsize::new(0));
        let observed_requests = callback_requests.clone();
        callbacks.add_request(Arc::new(move |_| {
            observed_requests.fetch_add(1, Ordering::SeqCst);
        }));
        let observed_responses = callback_responses.clone();
        callbacks.add_response(Arc::new(move |_, _| {
            observed_responses.fetch_add(1, Ordering::SeqCst);
        }));

        let mut fetches = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let client = client.clone();
            let callbacks = callbacks.clone();
            let url = url.clone();
            let request = ResourceRequest::subresource(ResourceType::Script, &initiator);
            fetches.spawn(async move {
                client
                    .fetch_resource_with_callbacks(&url, request, Some(&callbacks))
                    .await
                    .unwrap()
            });
        }
        let mut responses = Vec::new();
        while let Some(response) = fetches.join_next().await {
            responses.push(response.unwrap());
        }

        assert_eq!(responses.len(), 32);
        assert!(responses.iter().all(|response| response.status == 200));
        assert_eq!(network_requests.load(Ordering::SeqCst), 1);
        assert_eq!(callback_requests.load(Ordering::SeqCst), 32);
        assert_eq!(callback_responses.load(Ordering::SeqCst), 32);
    }

    #[tokio::test]
    async fn cacheable_identical_module_scripts_share_one_in_flight_request() {
        let (url, network_requests) = cacheable_resource_fixture(
            200,
            "Cache-Control: public, max-age=3600\r\n",
        )
        .await;
        let initiator = url.join("/app.js").unwrap();
        let client = Arc::new(ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        ));

        let mut fetches = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let client = client.clone();
            let url = url.clone();
            let request = ResourceRequest::module_script(&initiator, &initiator);
            fetches.spawn(async move {
                client
                    .fetch_resource_with_callbacks(&url, request, None)
                    .await
                    .unwrap()
            });
        }
        let mut responses = Vec::new();
        while let Some(response) = fetches.join_next().await {
            responses.push(response.unwrap());
        }

        assert_eq!(responses.len(), 16);
        assert!(responses.iter().all(|response| response.status == 200));
        assert_eq!(network_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn distinct_subresource_urls_do_not_coalesce() {
        let (url, network_requests) =
            cacheable_resource_fixture(200, "Cache-Control: public, max-age=3600\r\n").await;
        let initiator = url.join("/page.html").unwrap();
        let client = Arc::new(ObscuraHttpClient::with_full_options(
            Arc::new(CookieJar::new()),
            None,
            true,
        ));

        let mut fetches = tokio::task::JoinSet::new();
        for index in 0..24 {
            let client = client.clone();
            let url = url.join(&format!("/distinct/{index}.js")).unwrap();
            let request = ResourceRequest::subresource(ResourceType::Script, &initiator);
            fetches.spawn(async move {
                client
                    .fetch_resource_with_callbacks(&url, request, None)
                    .await
                    .unwrap()
            });
        }
        let mut responses = Vec::new();
        while let Some(response) = fetches.join_next().await {
            responses.push(response.unwrap());
        }

        assert_eq!(responses.len(), 24);
        assert_eq!(network_requests.load(Ordering::SeqCst), 24);
    }

    #[tokio::test]
    async fn no_store_vary_star_and_error_responses_are_not_reused() {
        for (status, headers) in [
            (200, "Cache-Control: no-store\r\n"),
            (200, "Cache-Control: public, max-age=3600\r\nVary: *\r\n"),
            (500, "Cache-Control: public, max-age=3600\r\n"),
        ] {
            let (url, network_requests) = cacheable_resource_fixture(status, headers).await;
            let initiator = url.join("/page.html").unwrap();
            let client =
                ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
            let request = ResourceRequest::subresource(ResourceType::Script, &initiator);
            client
                .fetch_resource_with_callbacks(&url, request.clone(), None)
                .await
                .unwrap();
            client
                .fetch_resource_with_callbacks(&url, request, None)
                .await
                .unwrap();
            assert_eq!(
                network_requests.load(Ordering::SeqCst),
                2,
                "status={status} headers={headers:?}",
            );
        }
    }

    #[tokio::test]
    async fn authorization_and_cookie_bearing_requests_bypass_resource_cache() {
        for header in [
            ("Authorization", "Bearer secret"),
            ("Cookie", "session=secret"),
        ] {
            let (url, network_requests) =
                cacheable_resource_fixture(200, "Cache-Control: public, max-age=3600\r\n").await;
            let initiator = url.join("/page.html").unwrap();
            let client =
                ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
            client
                .set_extra_headers(HashMap::from([(
                    header.0.to_string(),
                    header.1.to_string(),
                )]))
                .await;
            let request = ResourceRequest::subresource(ResourceType::Script, &initiator);
            client
                .fetch_resource_with_callbacks(&url, request.clone(), None)
                .await
                .unwrap();
            client
                .fetch_resource_with_callbacks(&url, request, None)
                .await
                .unwrap();
            assert_eq!(
                network_requests.load(Ordering::SeqCst),
                2,
                "header={header:?}"
            );
        }
    }

    #[tokio::test]
    async fn resolver_blocks_hostname_that_resolves_to_loopback() {
        // localtest.me is a public DNS name that resolves to 127.0.0.1 — the
        // canonical DNS-rebinding test. The guard must reject it. If DNS is
        // unavailable the lookup itself errors (also Err), so the assertion
        // holds either way.
        let r = SsrfGuardResolver::new(false);
        let res = r.resolve(Name::from_str("localtest.me").unwrap()).await;
        assert!(res.is_err(), "localtest.me -> 127.0.0.1 must be blocked");
    }

    #[tokio::test]
    async fn resolver_does_not_ssrf_block_public_host() {
        // A public host must not be SSRF-blocked. Tolerate a no-network sandbox
        // by only failing on an actual SSRF rejection, not a lookup failure.
        let r = SsrfGuardResolver::new(false);
        match r.resolve(Name::from_str("example.com").unwrap()).await {
            Ok(_) => {}
            Err(e) => assert!(
                !e.to_string().contains("SSRF blocked"),
                "example.com wrongly SSRF-blocked: {e}"
            ),
        }
    }

    /// Mint a throwaway CA plus a 127.0.0.1 leaf it signed, and serve one
    /// canned HTTPS response with the leaf on an ephemeral port. Returns the
    /// port and the CA certificate as PEM.
    async fn https_fixture_with_private_ca() -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();

        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf_params =
            rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
        let leaf_cert = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();

        let certs = vec![tokio_rustls::rustls::pki_types::CertificateDer::from(
            leaf_cert.der().to_vec(),
        )];
        let key = tokio_rustls::rustls::pki_types::PrivateKeyDer::Pkcs8(
            tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer::from(
                leaf_key.serialize_der(),
            ),
        );
        let config = tokio_rustls::rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(stream).await else {
                        return; // Handshake rejection is the point of one test.
                    };
                    let mut buf = [0u8; 1024];
                    let _ = tls.read(&mut buf).await;
                    let body = "private ca ok";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });

        (port, ca_cert.pem())
    }

    // The two configured-roots tests set/rely on SSL_CERT_FILE, which is
    // cached once per process at client build. They are only correct under
    // `cargo nextest` (one process per test), the same constraint the whole
    // workspace already has.

    #[tokio::test]
    async fn configured_roots_trust_a_private_ca_via_ssl_cert_file() {
        let (port, ca_pem) = https_fixture_with_private_ca().await;
        let ca_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca_file.path(), ca_pem).unwrap();
        std::env::set_var("SSL_CERT_FILE", ca_file.path());

        let client =
            ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
        let url = Url::parse(&format!("https://127.0.0.1:{port}/")).unwrap();
        let resp = client.fetch(&url).await.expect("private CA in SSL_CERT_FILE must be trusted");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.text(), "private ca ok");
    }

    #[tokio::test]
    async fn configured_roots_trust_a_private_ca_via_ssl_cert_dir() {
        let (port, ca_pem) = https_fixture_with_private_ca().await;
        let ca_dir = tempfile::tempdir().unwrap();
        std::fs::write(ca_dir.path().join("private-ca.pem"), ca_pem).unwrap();
        std::env::set_var("SSL_CERT_DIR", ca_dir.path());

        let client =
            ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
        let url = Url::parse(&format!("https://127.0.0.1:{port}/")).unwrap();
        let resp = client
            .fetch(&url)
            .await
            .expect("private CA in SSL_CERT_DIR must be trusted");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.text(), "private ca ok");
    }

    #[tokio::test]
    async fn private_ca_is_still_rejected_without_ssl_cert_file() {
        // The same fixture that the SSL_CERT_FILE test trusts must fail here. The
        // listener is reachable (same setup), so an Err can only be TLS.
        let (port, _ca_pem) = https_fixture_with_private_ca().await;
        let client =
            ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()), None, true);
        let url = Url::parse(&format!("https://127.0.0.1:{port}/")).unwrap();
        assert!(client.fetch(&url).await.is_err(), "unknown CA must be rejected");
    }
}

#[cfg(test)]
mod cert_env_tests {
    use super::custom_cert_store_requested;
    use std::ffi::OsStr;

    #[test]
    fn empty_ssl_cert_env_is_treated_as_unset() {
        // Set-but-empty must NOT request a custom store: for the stealth client
        // that would replace the webpki roots with a near-empty default-paths
        // store and break all HTTPS.
        assert!(!custom_cert_store_requested(Some(OsStr::new("")), None));
        assert!(!custom_cert_store_requested(None, Some(OsStr::new(""))));
        assert!(!custom_cert_store_requested(
            Some(OsStr::new("")),
            Some(OsStr::new(""))
        ));
        // Genuinely unset: no custom store.
        assert!(!custom_cert_store_requested(None, None));
        // Set and non-empty: build the custom store (behavior unchanged).
        assert!(custom_cert_store_requested(
            Some(OsStr::new("/etc/corp/ca.pem")),
            None
        ));
        assert!(custom_cert_store_requested(
            None,
            Some(OsStr::new("/etc/ssl/certs"))
        ));
    }
}
