#[cfg(feature = "stealth")]
use std::collections::HashMap;
#[cfg(feature = "stealth")]
use std::error::Error;
#[cfg(feature = "stealth")]
use std::sync::Arc;
#[cfg(feature = "stealth")]
use std::time::Duration;

#[cfg(feature = "stealth")]
use futures_util::StreamExt;
#[cfg(feature = "stealth")]
use tokio::sync::RwLock;
#[cfg(feature = "stealth")]
use url::Url;

#[cfg(feature = "stealth")]
use crate::cookies::CookieJar;
#[cfg(feature = "stealth")]
use crate::client::{
    CallbackRegistry, InFlightGuard, ObscuraNetError, RequestInfo, RequestMode,
    ResourceRequest, Response, SsrfGuardResolver, cors_required, env_allows_private_network,
    fetch_file_url, is_forbidden_ip, redirect_taints_origin, request_fetch_site,
    request_referrer, response_too_large, serialized_request_origin, validate_cors_response,
    validate_request_mode, validate_url,
};

/// The wreq half of [`SsrfGuardResolver`]. `validate_url` only inspects the
/// host *string*, so on its own it lets a public name that resolves inward
/// straight through — the reqwest client closes that with a `dns_resolver`, and
/// until this impl existed the stealth client did not, which made `--stealth` a
/// downgrade in protection rather than only a change of fingerprint. The
/// deny-set is shared, so the two transports cannot drift apart.
#[cfg(feature = "stealth")]
impl wreq::dns::Resolve for SsrfGuardResolver {
    fn resolve(&self, name: wreq::dns::Name) -> wreq::dns::Resolving {
        let allow = self.allow_private || env_allows_private_network();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| -> Box<dyn Error + Send + Sync> { Box::new(e) })?
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
            let iter: wreq::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

#[cfg(feature = "stealth")]
pub const STEALTH_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

// The wreq emulation (Profile::Chrome145, Platform::Windows) sends this exact
// UA and sec-ch-ua-platform "Windows" on the wire. navigator has to report the
// same identity, otherwise the TLS/HTTP layer and the JS layer disagree and a
// site cross-checks the mismatch as a bot signal.
#[cfg(feature = "stealth")]
pub const STEALTH_NAVIGATOR_PLATFORM: &str = "Win32";
#[cfg(feature = "stealth")]
pub const STEALTH_UA_PLATFORM: &str = "Windows";
#[cfg(feature = "stealth")]
pub const STEALTH_UA_PLATFORM_VERSION: &str = "15.0.0";

#[cfg(feature = "stealth")]
fn wreq_response_header_value<'a>(
    headers: &'a wreq::header::HeaderMap,
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

#[cfg(feature = "stealth")]
fn validate_wreq_cors_response(
    request: &ResourceRequest,
    target: &Url,
    serialized_origin: &str,
    headers: &wreq::header::HeaderMap,
) -> Result<(), ObscuraNetError> {
    if !cors_required(request, target) {
        return Ok(());
    }
    let allow_origin =
        wreq_response_header_value(headers, "access-control-allow-origin", target)?;
    let allow_credentials =
        wreq_response_header_value(headers, "access-control-allow-credentials", target)?;
    validate_cors_response(
        request,
        target,
        serialized_origin,
        allow_origin,
        allow_credentials,
    )
}

#[cfg(feature = "stealth")]
async fn read_wreq_body_limited(
    response: wreq::Response,
    url: &Url,
    limit: usize,
) -> Result<Vec<u8>, ObscuraNetError> {
    if response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .is_some_and(|length| length > limit as u64)
    {
        return Err(response_too_large(url, limit));
    }

    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(limit);
    let stream = response.bytes_stream();
    futures_util::pin_mut!(stream);
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            ObscuraNetError::Network(format!("Failed to read body: {}", error))
        })?;
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(response_too_large(url, limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(feature = "stealth")]
async fn send_get_with_connection_reset_retry(
    request: wreq::RequestBuilder,
    url: &Url,
) -> Result<wreq::Response, wreq::Error> {
    let retry = request.try_clone();
    match request.send().await {
        Err(error) if error.is_connection_reset() => {
            let Some(retry) = retry else {
                return Err(error);
            };
            tracing::debug!(%url, "retrying GET after connection reset");
            retry.send().await
        }
        result => result,
    }
}

#[cfg(feature = "stealth")]
pub struct StealthHttpClient {
    client: wreq::Client,
    pub cookie_jar: Arc<CookieJar>,
    pub extra_headers: RwLock<HashMap<String, String>>,
    pub in_flight: Arc<std::sync::atomic::AtomicU32>,
}

#[cfg(feature = "stealth")]
impl StealthHttpClient {
    pub fn new(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_proxy(cookie_jar, None)
    }

    pub fn with_proxy(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        let emulation_opts = wreq_util::Emulation::builder()
            .profile(wreq_util::Profile::Chrome145)
            .platform(wreq_util::Platform::Windows)
            .build();

        let mut builder = wreq::Client::builder()
            .emulation(emulation_opts)
            .timeout(Duration::from_secs(30))
            // SSRF guard: reject hostnames that resolve to a private/loopback IP.
            // `false` mirrors the `validate_url(url, false)` calls below; the
            // resolver still honours OBSCURA_ALLOW_PRIVATE_NETWORK on its own.
            .dns_resolver(Arc::new(SsrfGuardResolver::new(false)))
            .redirect(wreq::redirect::Policy::none());

        // Honor SSL_CERT_FILE / SSL_CERT_DIR in the stealth client too.
        //
        // `client.rs` (the reqwest path) already reads these via `configured_root_paths()` and
        // feeds `add_root_certificate`, so a private CA works there. This client did not, which
        // made the *better-fingerprinted* transport the only one unable to reach hosts behind a
        // private/national CA (measured against a Brazilian government portal whose leaf is
        // issued by an ICP-Brasil intermediate: `--stealth` failed with CERTIFICATE_VERIFY_FAILED
        // while the reqwest path, with SSL_CERT_FILE set, completed the handshake).
        //
        // Two deliberate constraints:
        //
        // 1. `tls_cert_store` is used, NOT `tls_options`. `emulation()` overwrites `tls_options`
        //    wholesale ("This will overwrite the existing configuration"), so setting TLS options
        //    here would silently discard the Chrome fingerprint — the whole point of this client.
        //    `tls_cert_store` is a separate field on the config and composes with emulation.
        //
        // 2. Opt-in only. Supplying a store REPLACES the webpki roots (see `set_cert_store` in
        //    `tls/conn/ext.rs`), it does not add to them. Applying it unconditionally would break
        //    every ordinary site whenever the bundle is incomplete. With neither variable set,
        //    behaviour is byte-for-byte what it was before.
        if crate::client::custom_cert_store_requested(
            std::env::var_os("SSL_CERT_FILE").as_deref(),
            std::env::var_os("SSL_CERT_DIR").as_deref(),
        ) {
            match wreq::tls::trust::CertStore::builder().set_default_paths().build() {
                Ok(store) => builder = builder.tls_cert_store(store),
                Err(error) => tracing::warn!(
                    %error,
                    "SSL_CERT_FILE/SSL_CERT_DIR set but the certificate store failed to build; \
                     continuing with the default roots"
                ),
            }
        }

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
        }
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        self.fetch_with_callbacks(url, None).await
    }

    pub async fn fetch_with_callbacks(
        &self,
        url: &Url,
        callbacks: Option<&CallbackRegistry>,
    ) -> Result<Response, ObscuraNetError> {
        self.fetch_with_profile(url, ResourceRequest::navigation(), callbacks)
            .await
    }

    pub async fn fetch_resource_with_callbacks(
        &self,
        url: &Url,
        request: ResourceRequest,
        callbacks: Option<&CallbackRegistry>,
    ) -> Result<Response, ObscuraNetError> {
        self.fetch_with_profile(url, request, callbacks).await
    }

    async fn fetch_with_profile(
        &self,
        url: &Url,
        request: ResourceRequest,
        callbacks: Option<&CallbackRegistry>,
    ) -> Result<Response, ObscuraNetError> {
        validate_url(url, false)?;
        validate_request_mode(&request, url)?;
        if url.scheme() == "file" {
            return fetch_file_url(url, request.max_response_bytes).await;
        }

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
        let mut redirect_tainted = false;
        let mut request_callback_fired = false;

        for _ in 0..20 {
            validate_request_mode(&request, &current_url)?;
            let mut req = self.client.get(current_url.as_str());

            req = req
                .header("accept", request.accept())
                .header("sec-fetch-site", request_fetch_site(&request, &current_url))
                .header("sec-fetch-mode", request.mode.header_value())
                .header("sec-fetch-dest", request.destination());
            if request.mode == RequestMode::Navigate {
                req = req
                    .header("upgrade-insecure-requests", "1")
                    .header("sec-fetch-user", "?1");
            }
            if let Some(referer) = request_referrer(&request, &current_url) {
                req = req.header("referer", referer);
            }
            let request_origin = serialized_request_origin(&request, redirect_tainted);

            let cookie_header = if request.sends_credentials_to(&current_url) {
                self.cookie_jar.get_cookie_header(&current_url)
            } else {
                String::new()
            };
            if !cookie_header.is_empty() {
                req = req.header("Cookie", &cookie_header);
            }

            for (k, v) in self.extra_headers.read().await.iter() {
                if k.eq_ignore_ascii_case("origin") {
                    continue;
                }
                req = req.header(k.as_str(), v.as_str());
            }
            if cors_required(&request, &current_url) {
                req = req.header("origin", &request_origin);
            }

            let request_info = RequestInfo {
                url: current_url.clone(),
                method: "GET".to_string(),
                headers: self.extra_headers.read().await.clone(),
                resource_type: request.resource_type,
            };
            if !request_callback_fired {
                if let Some(callbacks) = callbacks {
                    callbacks.fire_request(&request_info).await;
                }
                request_callback_fired = true;
            }

            let in_flight = InFlightGuard::new(&self.in_flight);
            let resp = send_get_with_connection_reset_retry(req, &current_url)
                .await
                .map_err(|e| {
                    ObscuraNetError::Network(format!(
                        "{}: {} (source: {:?})",
                        current_url,
                        e,
                        e.source()
                    ))
                })?;

            let status = resp.status();
            validate_wreq_cors_response(
                &request,
                &current_url,
                &request_origin,
                resp.headers(),
            )?;

            if request.sends_credentials_to(&current_url) {
                for val in resp.headers().get_all("set-cookie") {
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
                if let Some(location) = resp.headers().get("location") {
                    let location_str = location.to_str().map_err(|_| {
                        ObscuraNetError::Network("Invalid redirect Location".into())
                    })?;
                    let next_url = current_url.join(location_str).map_err(|e| {
                        ObscuraNetError::Network(format!("Invalid redirect URL: {}", e))
                    })?;
                    validate_url(&next_url, false)?;
                    validate_request_mode(&request, &next_url)?;
                    redirect_tainted |=
                        redirect_taints_origin(&request, &current_url, &next_url);
                    redirects.push(current_url.clone());
                    current_url = next_url;
                    continue;
                }
            }

            let body = read_wreq_body_limited(resp, &current_url, request.max_response_bytes)
                .await?;
            drop(in_flight);

            let response = Response {
                url: current_url,
                status: status.as_u16(),
                headers: response_headers,
                body,
                redirected_from: redirects,
            };
            if let Some(callbacks) = callbacks {
                callbacks.fire_response(&request_info, &response).await;
            }
            return Ok(response);
        }

        Err(ObscuraNetError::TooManyRedirects(url.to_string()))
    }

    /// One request with no redirect following, for scripted fetch()/XHR. The
    /// caller supplies the Fetch credentials decision for this redirect hop,
    /// while this method preserves the Chrome transport fingerprint.
    pub async fn send_single(
        &self,
        method: &str,
        url: &Url,
        headers: &HashMap<String, String>,
        body: &str,
        send_cookies: bool,
        store_cookies: bool,
    ) -> Result<Response, ObscuraNetError> {
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

        let req_method = method
            .parse::<wreq::Method>()
            .map_err(|e| ObscuraNetError::Network(format!("invalid method '{}': {}", method, e)))?;
        let mut req = self.client.request(req_method, url.as_str());

        if send_cookies {
            let cookie_header = self.cookie_jar.get_cookie_header(url);
            if !cookie_header.is_empty() {
                req = req.header("cookie", &cookie_header);
            }
        }
        for (k, v) in self.extra_headers.read().await.iter() {
            req = req.header(k.as_str(), v.as_str());
        }
        for (k, v) in headers.iter() {
            req = req.header(k.as_str(), v.as_str());
        }
        if !body.is_empty() {
            req = req.body(body.to_string());
        }

        let in_flight = InFlightGuard::new(&self.in_flight);
        let resp = req.send().await.map_err(|e| {
            ObscuraNetError::Network(format!("{}: {}", url, e))
        })?;

        let status = resp.status();
        if store_cookies {
            for val in resp.headers().get_all("set-cookie") {
                if let Ok(s) = val.to_str() {
                    self.cookie_jar.set_cookie(s, url);
                }
            }
        }
        let response_headers: HashMap<String, String> = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let resp_body = read_wreq_body_limited(resp, url, 64 * 1024 * 1024).await?;
        drop(in_flight);

        Ok(Response {
            url: url.clone(),
            status: status.as_u16(),
            headers: response_headers,
            body: resp_body,
            redirected_from: Vec::new(),
        })
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
    use std::io::{Read, Write};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use url::Url;

    use super::{StealthHttpClient, send_get_with_connection_reset_retry};
    use crate::client::{ObscuraNetError, SsrfGuardResolver};
    use crate::cookies::CookieJar;
    use wreq::dns::{Name, Resolve};

    // Mirrors client::ssrf_tests::resolver_blocks_hostname_that_resolves_to_loopback.
    // Both transports must agree: a host-string check alone cannot see that a
    // public name points inward, so the stealth client needs the same resolver.
    #[tokio::test]
    async fn stealth_resolver_blocks_hostname_that_resolves_to_loopback() {
        let resolver = SsrfGuardResolver::new(false);
        let res = resolver.resolve(Name::from("localtest.me")).await;
        assert!(res.is_err(), "localtest.me -> 127.0.0.1 must be blocked");
    }

    #[tokio::test]
    async fn stealth_resolver_does_not_block_public_host() {
        // Tolerate a no-network sandbox: only an actual SSRF rejection fails.
        let resolver = SsrfGuardResolver::new(false);
        match resolver.resolve(Name::from("example.com")).await {
            Ok(_) => {}
            Err(e) => assert!(
                !e.to_string().contains("SSRF blocked"),
                "example.com wrongly SSRF-blocked: {e}"
            ),
        }
    }

    const PLAIN_BODY: &str = "<!DOCTYPE html><html><body><p id=\"mark\">gzip ok</p></body></html>";

    // gzip (level 9) of PLAIN_BODY, hardcoded so the fixture needs no
    // compression dependency. A wrong byte fails the assert below.
    const GZIP_BODY: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xb3, 0x51,
        0x74, 0xf1, 0x77, 0x0e, 0x89, 0x0c, 0x70, 0x55, 0xc8, 0x28, 0xc9, 0xcd,
        0xb1, 0xb3, 0x81, 0x90, 0x49, 0xf9, 0x29, 0x95, 0x76, 0x36, 0x05, 0x0a,
        0x99, 0x29, 0xb6, 0x4a, 0xb9, 0x89, 0x45, 0xd9, 0x4a, 0x76, 0xe9, 0x55,
        0x99, 0x05, 0x0a, 0xf9, 0xd9, 0x36, 0xfa, 0x05, 0x76, 0x36, 0xfa, 0x10,
        0x69, 0x7d, 0xb0, 0x5a, 0x00, 0x80, 0x3d, 0x1c, 0x5f, 0x41, 0x00, 0x00,
        0x00,
    ];

    fn reset_fixture(respond_after_reset: bool) -> (u16, std::thread::JoinHandle<usize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let attempts = if respond_after_reset { 2 } else { 1 };
            for attempt in 0..attempts {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let read = stream.read(&mut buf).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..read]);
                }

                if attempt == 0 {
                    let socket = socket2::Socket::from(stream);
                    socket.set_linger(Some(Duration::ZERO)).unwrap();
                    drop(socket);
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .unwrap();
                }
            }
            attempts
        });
        (port, server)
    }

    #[tokio::test]
    async fn stealth_get_recovers_from_connection_reset() {
        let (port, server) = reset_fixture(true);
        let client = wreq::Client::builder().no_proxy().build().unwrap();
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        let response = send_get_with_connection_reset_retry(client.get(url.as_str()), &url)
            .await
            .expect("an idempotent GET should recover from one connection reset");

        assert_eq!(response.status(), wreq::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "ok");
        assert_eq!(server.join().unwrap(), 2);
    }

    #[tokio::test]
    async fn stealth_post_does_not_retry_connection_reset() {
        let (port, server) = reset_fixture(false);
        let client = StealthHttpClient {
            client: wreq::Client::builder().no_proxy().build().unwrap(),
            cookie_jar: Arc::new(CookieJar::new()),
            extra_headers: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        let error = client
            .send_single(
                "POST",
                &url,
                &std::collections::HashMap::new(),
                "payload",
                false,
                false,
            )
            .await
            .expect_err("POST must not be retried after a connection reset");

        assert!(matches!(error, ObscuraNetError::Network(_)));
        assert_eq!(server.join().unwrap(), 1);
    }

    /// Serve one `Content-Encoding: gzip` response on an ephemeral port.
    async fn gzip_fixture() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        GZIP_BODY.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(GZIP_BODY).await;
                    let _ = stream.shutdown().await;
                });
            }
        });

        port
    }

    // The emulation profile advertises gzip, so origins compress. Without the
    // decoder the raw gzip bytes reach the HTML parser as document text.
    #[tokio::test]
    async fn stealth_client_decodes_gzip_response() {
        let port = gzip_fixture().await;
        let client = StealthHttpClient::new(Arc::new(CookieJar::new()));
        let url = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();

        let resp = client.fetch(&url).await.expect("fixture must be reachable");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.text(), PLAIN_BODY, "gzip body must be decompressed");
    }
}
