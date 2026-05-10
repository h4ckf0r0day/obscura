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
use crate::client::{env_allows_private_network, validate_url, ObscuraNetError, Response};

#[cfg(feature = "stealth")]
pub const STEALTH_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

#[cfg(feature = "stealth")]
pub struct StealthHttpClient {
    client: wreq::Client,
    pub cookie_jar: Arc<CookieJar>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
    pub allow_private_network: bool,
}

#[cfg(feature = "stealth")]
impl StealthHttpClient {
    pub fn new(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_proxy(cookie_jar, None)
    }

    pub fn with_proxy(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        Self::with_options(cookie_jar, proxy_url, env_allows_private_network())
    }

    pub fn with_options(
        cookie_jar: Arc<CookieJar>,
        proxy_url: Option<&str>,
        allow_private_network: bool,
    ) -> Self {
        let cert_store = wreq::tls::CertStore::builder()
            .set_default_paths()
            .build()
            .expect("Failed to load system CA certificates");

        let emulation_opts = wreq_util::EmulationOption::builder()
            .emulation(wreq_util::Emulation::Chrome145)
            .emulation_os(wreq_util::EmulationOS::Linux)
            .build();

        let mut builder = wreq::Client::builder()
            .emulation(emulation_opts)
            .cert_store(cert_store)
            .timeout(Duration::from_secs(30))
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
        validate_url(url, self.allow_private_network)?;

        let mut current_url = url.clone();
        let mut redirects = Vec::new();

        for _ in 0..20 {
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
                    validate_url(&next_url, self.allow_private_network)?;
                    redirects.push(current_url.clone());
                    current_url = next_url;
                    continue;
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
