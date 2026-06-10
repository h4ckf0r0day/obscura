#[cfg(feature = "stealth")]
use std::collections::HashMap;
#[cfg(feature = "stealth")]
use std::error::Error;
#[cfg(feature = "stealth")]
use std::sync::Arc;
#[cfg(feature = "stealth")]
use std::time::Duration;

#[cfg(feature = "stealth")]
use tokio::sync::RwLock;
#[cfg(feature = "stealth")]
use url::Url;

#[cfg(feature = "stealth")]
use crate::cookies::CookieJar;
#[cfg(feature = "stealth")]
use crate::client::{Response, ObscuraNetError};

#[cfg(feature = "stealth")]
pub const STEALTH_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

#[cfg(feature = "stealth")]
pub struct StealthHttpClient {
    client: wreq::Client,
    pub cookie_jar: Arc<CookieJar>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
    /// Mirrors `ObscuraHttpClient::allow_private_network`. When false (default),
    /// `fetch` applies the same SSRF guard as the reqwest client to the initial
    /// URL and every redirect hop. Without this the `--stealth` path was a hole
    /// straight through the PR#279 protections.
    allow_private_network: bool,
}

/// `wreq` DNS resolver enforcing the same SSRF policy as the reqwest client's
/// `SsrfDnsResolver`: it resolves the host and rejects the request if any
/// resolved address is forbidden, so `wreq` connects only to vetted addresses.
/// This closes DNS rebinding on the stealth path; IP-literal hosts never reach a
/// custom resolver and are caught by `validate_url` in `fetch`.
#[cfg(feature = "stealth")]
struct StealthSsrfResolver {
    allow_private_network: bool,
}

#[cfg(feature = "stealth")]
impl wreq::dns::Resolve for StealthSsrfResolver {
    fn resolve(&self, name: wreq::dns::Name) -> wreq::dns::Resolving {
        let allow = self.allow_private_network || crate::client::env_allows_private_network();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0))
                    .await
                    .map_err(|e| -> Box<dyn Error + Send + Sync> { Box::new(e) })?
                    .collect();
            if !allow {
                if let Some(bad) = addrs.iter().find(|a| crate::client::is_forbidden_ip(&a.ip())) {
                    let bad_ip = bad.ip();
                    return Err(Box::<dyn Error + Send + Sync>::from(format!(
                        "SSRF blocked: '{host}' resolves to forbidden address {bad_ip}"
                    )));
                }
            }
            Ok(Box::new(addrs.into_iter()) as wreq::dns::Addrs)
        })
    }
}

#[cfg(feature = "stealth")]
impl StealthHttpClient {
    pub fn new(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_proxy_and_network(cookie_jar, None, false)
    }

    pub fn with_proxy(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        Self::with_proxy_and_network(cookie_jar, proxy_url, false)
    }

    pub fn with_proxy_and_network(
        cookie_jar: Arc<CookieJar>,
        proxy_url: Option<&str>,
        allow_private_network: bool,
    ) -> Self {
        // Issue #184: `set_default_paths()` reads OpenSSL's compile-time CA
        // paths, which only resolve on Linux. On Windows the store ends up
        // empty and every TLS handshake fails with CERTIFICATE_VERIFY_FAILED.
        // `CertStore::default()` uses wreq's bundled Mozilla roots
        // (`webpki-root-certs`), which works the same on every platform.
        let cert_store = wreq::tls::CertStore::default();

        let emulation_opts = wreq_util::EmulationOption::builder()
            .emulation(wreq_util::Emulation::Chrome145)
            .emulation_os(wreq_util::EmulationOS::Linux)
            .build();

        let mut builder = wreq::Client::builder()
            .emulation(emulation_opts)
            .cert_store(cert_store)
            .timeout(Duration::from_secs(30))
            // Resolve-time SSRF guard — parity with the reqwest client. Rejects a
            // host that resolves to a forbidden address, closing DNS rebinding on
            // the stealth path. The literal-IP / localhost layer is validate_url.
            .dns_resolver(StealthSsrfResolver { allow_private_network })
            .redirect(wreq::redirect::Policy::none());

        if let Some(proxy) = proxy_url {
            if let Ok(p) = wreq::Proxy::all(proxy) {
                builder = builder.proxy(p);
            }
        }

        let client = builder.build().expect("failed to build wreq stealth client");

        StealthHttpClient {
            client,
            cookie_jar,
            extra_headers: RwLock::new(HashMap::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            allow_private_network,
        }
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        let mut current_url = url.clone();

        if let Some(host) = current_url.host_str() {
            if crate::blocklist::is_blocked(host) {
                tracing::debug!("Blocked tracker: {}", current_url);
                return Ok(Response {
                    status: 0,
                    url: current_url,
                    headers: HashMap::new(),
                    body: Vec::new(),
                    redirected_from: Vec::new(),
                });
            }
        }

        let mut redirects = Vec::new();

        for _ in 0..20 {
            // SSRF guard — same policy as the reqwest client. Runs on the
            // initial URL (first iteration) and on every redirect target
            // (`current_url` is reassigned to the hop below before `continue`),
            // so a 302 to 169.254.169.254 / 127.0.0.1 / a non-http scheme is
            // rejected before any connection is made.
            crate::client::validate_url(&current_url, self.allow_private_network)?;

            let mut req = self.client.get(current_url.as_str());

            let cookie_header = self.cookie_jar.get_cookie_header(&current_url);
            if !cookie_header.is_empty() {
                req = req.header("Cookie", &cookie_header);
            }

            for (k, v) in self.extra_headers.read().await.iter() {
                req = req.header(k.as_str(), v.as_str());
            }

            self.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let resp = req.send().await.map_err(|e| {
                self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                ObscuraNetError::Network(format!("{}: {} (source: {:?})", current_url, e, e.source()))
            })?;
            self.in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

            let status = resp.status();

            for val in resp.headers().get_all("set-cookie") {
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
                if let Some(location) = resp.headers().get("location") {
                    let location_str = location.to_str().map_err(|_| {
                        ObscuraNetError::Network("Invalid redirect Location".into())
                    })?;
                    let next_url = current_url.join(location_str).map_err(|e| {
                        ObscuraNetError::Network(format!("Invalid redirect URL: {}", e))
                    })?;
                    redirects.push(current_url.clone());
                    current_url = next_url;
                    continue;
                }
            }

            // NAVDOS-02: bound host memory by the declared Content-Length.
            // (Chunked bodies without a Content-Length still read in full here; a
            // streaming cap for the wreq path is a follow-up.)
            if let Some(len) = resp.content_length() {
                if len as usize > crate::client::max_response_body() {
                    return Err(ObscuraNetError::Network(format!(
                        "Response body too large: {} bytes",
                        len
                    )));
                }
            }
            let body = resp.bytes().await.map_err(|e| {
                ObscuraNetError::Network(format!("Failed to read body: {}", e))
            })?.to_vec();

            return Ok(Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body,
                redirected_from: redirects,
            });
        }

        Err(ObscuraNetError::TooManyRedirects(url.to_string()))
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

#[cfg(all(test, feature = "stealth"))]
mod tests {
    use super::*;
    use wreq::dns::Resolve;

    // The resolve-time guard must reject a hostname that resolves to loopback,
    // closing DNS rebinding on the stealth path. `localhost` resolves locally
    // (no network) to 127.0.0.1 / ::1, both forbidden.
    #[tokio::test]
    async fn ssrf_resolver_rejects_host_resolving_to_loopback() {
        let guarded = StealthSsrfResolver { allow_private_network: false };
        assert!(
            guarded.resolve(wreq::dns::Name::from("localhost")).await.is_err(),
            "a host resolving to loopback must be rejected at resolve time"
        );

        let permissive = StealthSsrfResolver { allow_private_network: true };
        assert!(
            permissive.resolve(wreq::dns::Name::from("localhost")).await.is_ok(),
            "allow_private_network must bypass the resolve-time guard"
        );
    }
}
