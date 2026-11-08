//! Pins the SSRF-checked address through an HTTP(S) proxy.
//!
//! With an `http://` or `https://` proxy, reqwest and wreq hand the proxy the
//! target *name*: `CONNECT name:443` for HTTPS, `GET http://name/...` for
//! plain HTTP. The proxy resolves that name itself, so a check of a local
//! lookup (`validate_proxied_host`) can be beaten by a name whose answer
//! changes between the two lookups (DNS rebinding).
//!
//! Both clients therefore talk to a small loopback proxy instead. It resolves
//! each target once, refuses it unless every address passes `is_forbidden_ip`,
//! and forwards to the real proxy with the checked address in place of the
//! name: `CONNECT 93.184.216.34:443`, `GET http://93.184.216.34/...`. The
//! client keeps the name everywhere else, so the `Host` header, TLS SNI and
//! certificate checks inside the tunnel are unchanged. Redirects and scripted
//! fetches go through the same client, so every request is pinned.
//!
//! The loopback proxy accepts only requests that carry its random token as
//! proxy credentials, so another local process cannot use it to reach the
//! real proxy. `socks5://` and `socks4://` already receive a checked address;
//! `socks5h://`, `socks4a://` and `.onion` targets are resolved by the proxy
//! by design and are passed through unchanged.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, HOST, PROXY_AUTHORIZATION};
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use crate::client::{env_allows_private_network, is_forbidden_ip};

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TUNNEL_RESPONSE_HEAD: usize = 16 * 1024;
const TOKEN_USER: &str = "obscura";

type Lookup = Arc<
    dyn Fn(String, u16) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>
        + Send
        + Sync,
>;
type ProxyBody = BoxBody<Bytes, hyper::Error>;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The proxy URL a client should use for `proxy_url`.
///
/// For an `http://` or `https://` proxy with the SSRF guard on, this is a
/// loopback proxy that pins every target to its checked address, started
/// once per distinct proxy and kept for the life of the process. Any other
/// proxy, or the guard switched off, returns `proxy_url` unchanged.
pub fn pinned_proxy_url(proxy_url: &str, allow_private_network: bool) -> String {
    if allow_private_network || env_allows_private_network() {
        return proxy_url.to_string();
    }
    let Some(upstream) = Upstream::parse(proxy_url) else {
        return proxy_url.to_string();
    };
    static PINNED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let mut pinned = PINNED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(local) = pinned.get(proxy_url) {
        return local.clone();
    }
    match start(upstream, lookup()) {
        Ok(local) => {
            pinned.insert(proxy_url.to_string(), local.clone());
            local
        }
        Err(error) => {
            // The local pre-check (`validate_proxied_host`) still runs, so the
            // client keeps the protection it had before pinning existed.
            tracing::error!(%error, "could not start the address-pinning proxy; using the proxy directly");
            proxy_url.to_string()
        }
    }
}

#[cfg(not(test))]
pub(crate) fn lookup() -> Lookup {
    Arc::new(|host, port| {
        Box::pin(async move { Ok(tokio::net::lookup_host((host.as_str(), port)).await?.collect()) })
    })
}

/// Tests answer names from `tests::HOSTS` so they can stand in for DNS.
#[cfg(test)]
pub(crate) fn lookup() -> Lookup {
    Arc::new(|host, port| Box::pin(async move { tests::resolve(&host, port).await }))
}

/// The real proxy, as parsed from the configured URL.
struct Upstream {
    host: String,
    port: u16,
    tls: bool,
    authorization: Option<HeaderValue>,
}

impl Upstream {
    fn parse(proxy_url: &str) -> Option<Self> {
        let url = Url::parse(proxy_url).ok()?;
        let tls = match url.scheme() {
            "http" => false,
            "https" => true,
            _ => return None,
        };
        let host = match url.host()? {
            url::Host::Domain(domain) => domain.to_string(),
            url::Host::Ipv4(ip) => ip.to_string(),
            url::Host::Ipv6(ip) => ip.to_string(),
        };
        let port = url.port_or_known_default()?;
        let authorization = (!url.username().is_empty()).then(|| {
            let decode = |part: &str| percent_encoding::percent_decode_str(part).decode_utf8_lossy().into_owned();
            let credentials = format!("{}:{}", decode(url.username()), decode(url.password().unwrap_or("")));
            basic(&credentials)
        });
        Some(Upstream { host, port, tls, authorization })
    }

    async fn connect(&self) -> io::Result<Box<dyn Io>> {
        let tcp = tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, TcpStream::connect((self.host.as_str(), self.port)))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connecting to the proxy timed out"))??;
        tcp.set_nodelay(true)?;
        if !self.tls {
            return Ok(Box::new(tcp));
        }
        let name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let tls = tls_connector().connect(name, tcp).await?;
        Ok(Box::new(tls))
    }

    /// Opens `CONNECT authority` through the real proxy.
    async fn tunnel(&self, authority: &str) -> io::Result<Box<dyn Io>> {
        let mut stream = self.connect().await?;
        let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
        if let Some(authorization) = &self.authorization {
            head.push_str("Proxy-Authorization: ");
            head.push_str(authorization.to_str().unwrap_or_default());
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).await?;
        // Read only the response head; the tunnel's first bytes belong to the client.
        let mut response = Vec::new();
        while !response.ends_with(b"\r\n\r\n") {
            if response.len() >= MAX_TUNNEL_RESPONSE_HEAD {
                return Err(io::Error::other("the proxy sent an oversized CONNECT response"));
            }
            let mut byte = [0u8; 1];
            if stream.read(&mut byte).await? == 0 {
                return Err(io::Error::other("the proxy closed the connection during CONNECT"));
            }
            response.push(byte[0]);
        }
        let status_line = response.split(|byte| *byte == b'\n').next().unwrap_or_default();
        let status = std::str::from_utf8(status_line).unwrap_or_default().split_whitespace().nth(1);
        if status != Some("200") {
            return Err(io::Error::other(format!(
                "the proxy refused CONNECT {authority}: {}",
                String::from_utf8_lossy(status_line).trim()
            )));
        }
        Ok(stream)
    }
}

fn basic(credentials: &str) -> HeaderValue {
    let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
    HeaderValue::from_str(&format!("Basic {encoded}")).expect("base64 is a valid header value")
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    static CONNECTOR: OnceLock<tokio_rustls::TlsConnector> = OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            for path in crate::client::configured_root_paths() {
                use rustls::pki_types::{pem::PemObject, CertificateDer};
                match CertificateDer::pem_file_iter(&path) {
                    Ok(certificates) => {
                        for certificate in certificates.flatten() {
                            let _ = roots.add(certificate);
                        }
                    }
                    Err(error) => tracing::warn!(?error, path = %path.display(), "failed to read CA certificate file"),
                }
            }
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring supports the default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            tokio_rustls::TlsConnector::from(Arc::new(config))
        })
        .clone()
}

struct Pinning {
    upstream: Upstream,
    lookup: Lookup,
    /// `Proxy-Authorization` the clients must send.
    expected: HeaderValue,
}

/// Starts a loopback pinning proxy for `upstream` on its own thread and
/// returns the proxy URL the clients use, token included.
fn start(upstream: Upstream, lookup: Lookup) -> io::Result<String> {
    let mut token = [0u8; 24];
    getrandom::fill(&mut token).map_err(|error| io::Error::other(error.to_string()))?;
    let token: String = token.iter().map(|byte| format!("{byte:02x}")).collect();
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let state = Arc::new(Pinning { upstream, lookup, expected: basic(&format!("{TOKEN_USER}:{token}")) });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    std::thread::Builder::new().name("obscura-proxy-pin".into()).spawn(move || {
        runtime.block_on(async move {
            let listener = match TcpListener::from_std(listener) {
                Ok(listener) => listener,
                Err(error) => return tracing::error!(%error, "address-pinning proxy stopped"),
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = stream.set_nodelay(true);
                    let service = hyper::service::service_fn(move |request| handle(state.clone(), request));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .preserve_header_case(true)
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades()
                        .await;
                });
            }
        });
    })?;
    Ok(format!("http://{TOKEN_USER}:{token}@{address}"))
}

/// Why a request was not forwarded. Returning it from the service closes the
/// client connection without a response, so a refused plain-HTTP request
/// fails in the client instead of arriving as a page.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Compares without stopping at the first difference, so the time taken does
/// not reveal how much of the token another local process guessed.
fn same_bytes(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

fn empty(status: StatusCode) -> Response<ProxyBody> {
    let mut response = Response::new(Empty::new().map_err(|never| match never {}).boxed());
    *response.status_mut() = status;
    response
}

async fn handle(state: Arc<Pinning>, mut request: Request<Incoming>) -> Result<Response<ProxyBody>, Refused> {
    let presented = request.headers().get(PROXY_AUTHORIZATION).map_or(&b""[..], |value| value.as_bytes());
    if !same_bytes(presented, state.expected.as_bytes()) {
        return Ok(empty(StatusCode::PROXY_AUTHENTICATION_REQUIRED));
    }
    request.headers_mut().remove(PROXY_AUTHORIZATION);
    if request.method() == Method::CONNECT {
        return Ok(connect(state, request).await);
    }
    forward(state, request).await
}

async fn connect(state: Arc<Pinning>, request: Request<Incoming>) -> Response<ProxyBody> {
    let Some((host, port)) = request.uri().authority().and_then(|a| Some((a.host().to_string(), a.port_u16()?))) else {
        return empty(StatusCode::BAD_REQUEST);
    };
    let authority = match pin(&state.lookup, &host, port).await {
        Ok(authority) => authority,
        Err(reason) => {
            tracing::warn!("{reason}");
            return empty(StatusCode::FORBIDDEN);
        }
    };
    let upstream = match state.upstream.tunnel(&authority).await {
        Ok(upstream) => upstream,
        Err(error) => {
            tracing::warn!(%error, "proxy tunnel failed");
            return empty(StatusCode::BAD_GATEWAY);
        }
    };
    tokio::spawn(async move {
        let Ok(client) = hyper::upgrade::on(request).await else { return };
        let mut client = TokioIo::new(client);
        let mut upstream = upstream;
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });
    empty(StatusCode::OK)
}

async fn forward(state: Arc<Pinning>, mut request: Request<Incoming>) -> Result<Response<ProxyBody>, Refused> {
    let uri = request.uri().clone();
    let (Some("http"), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return Err(Refused(format!("not an absolute http:// proxy request: {uri}")));
    };
    let port = authority.port_u16().unwrap_or(80);
    let pinned = pin(&state.lookup, authority.host(), port).await.map_err(|reason| {
        tracing::warn!("{reason}");
        Refused(reason)
    })?;
    if !request.headers().contains_key(HOST) {
        let host = HeaderValue::from_str(authority.as_str()).map_err(|e| Refused(e.to_string()))?;
        request.headers_mut().insert(HOST, host);
    }
    let path = uri.path_and_query().map_or("/", |path| path.as_str());
    *request.uri_mut() = Uri::try_from(format!("http://{pinned}{path}")).map_err(|e| Refused(e.to_string()))?;
    if let Some(authorization) = &state.upstream.authorization {
        request.headers_mut().insert(PROXY_AUTHORIZATION, authorization.clone());
    }
    request.headers_mut().remove("proxy-connection");
    let upstream = state.upstream.connect().await.map_err(|e| Refused(format!("connecting to the proxy: {e}")))?;
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .preserve_header_case(true)
        .handshake(TokioIo::new(upstream))
        .await
        .map_err(|e| Refused(format!("proxy handshake: {e}")))?;
    tokio::spawn(connection);
    let response = sender.send_request(request).await.map_err(|e| Refused(format!("proxy request: {e}")))?;
    Ok(response.map(BodyExt::boxed))
}

/// The authority to hand the real proxy for `host:port`: the checked address,
/// or the name itself for `.onion`, which only the proxy's network can reach.
async fn pin(lookup: &Lookup, host: &str, port: u16) -> Result<String, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(format!("SSRF blocked: proxied request to forbidden address {ip}"));
        }
        return Ok(SocketAddr::new(ip, port).to_string());
    }
    let lower = bare.to_ascii_lowercase();
    if lower == "onion" || lower.ends_with(".onion") {
        return Ok(format!("{bare}:{port}"));
    }
    let addresses = lookup(bare.to_string(), port)
        .await
        .map_err(|error| format!("SSRF blocked: '{bare}' could not be resolved to check it ({error})"))?;
    if let Some(bad) = addresses.iter().find(|address| is_forbidden_ip(address.ip())) {
        return Err(format!("SSRF blocked: '{bare}' resolves to forbidden address {}", bad.ip()));
    }
    let first = addresses.first().ok_or_else(|| format!("SSRF blocked: '{bare}' did not resolve"))?;
    Ok(SocketAddr::new(first.ip(), port).to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    //! Each test fakes the attack: a proxy that, handed the target's *name*,
    //! reaches an internal server (as if its own lookup answered differently),
    //! and handed the checked address reaches the public one. A pinned request
    //! must never reach the internal server.

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Names the tests resolve; any other name goes to the system resolver.
    static HOSTS: Mutex<Vec<(String, IpAddr)>> = Mutex::new(Vec::new());

    pub(crate) async fn resolve(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let found: Vec<_> = HOSTS.lock().unwrap_or_else(|e| e.into_inner()).iter()
            .filter(|(name, _)| name == host).map(|(_, ip)| SocketAddr::new(*ip, port)).collect();
        if !found.is_empty() {
            return Ok(found);
        }
        Ok(tokio::net::lookup_host((host, port)).await?.collect())
    }

    fn name(host: &str, ip: &str) {
        HOSTS.lock().unwrap().push((host.to_string(), ip.parse().unwrap()));
    }

    /// Answers every request with `body` and counts the connections.
    async fn origin(body: &'static str) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (address, hits)
    }

    /// An HTTP proxy that routes by the authority it is handed and records the
    /// request head of every request it receives.
    struct FakeProxy {
        url: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    async fn fake_proxy(routes: Vec<(String, SocketAddr)>, credentials: Option<&str>) -> FakeProxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let routes = Arc::new(routes);
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let (log, routes) = (log.clone(), routes.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if client.read(&mut byte).await.unwrap_or(0) == 0 { return; }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8(head).unwrap();
                    log.lock().unwrap().push(head.clone());
                    let line: Vec<&str> = head.lines().next().unwrap().split(' ').collect();
                    let (connect, target) = (line[0] == "CONNECT", line[1]);
                    let authority = if connect { target.to_string() } else {
                        let rest = target.trim_start_matches("http://");
                        rest.split('/').next().unwrap().to_string()
                    };
                    let Some((_, route)) = routes.iter().find(|(name, _)| *name == authority) else { return };
                    let mut server = TcpStream::connect(route).await.unwrap();
                    if connect {
                        client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
                    } else {
                        let path = format!("/{}", target.trim_start_matches("http://").splitn(2, '/').nth(1).unwrap_or(""));
                        let origin_form = head.replacen(target, &path, 1);
                        server.write_all(origin_form.as_bytes()).await.unwrap();
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                });
            }
        });
        let userinfo = credentials.map_or(String::new(), |c| format!("{c}@"));
        FakeProxy { url: format!("http://{userinfo}{address}"), seen }
    }

    impl FakeProxy {
        fn heads(&self) -> Vec<String> { self.seen.lock().unwrap().clone() }
        fn first_lines(&self) -> Vec<String> {
            self.heads().iter().map(|head| head.lines().next().unwrap().to_string()).collect()
        }
    }

    fn client_through(proxy: &FakeProxy) -> reqwest::Client {
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(pinned_proxy_url(&proxy.url, false)).unwrap())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn plain_http_reaches_the_proxy_as_the_checked_address_with_the_original_host() {
        name("rebind-http.test", "93.184.216.34");
        let (public, _) = origin("public").await;
        let (internal, internal_hits) = origin("internal").await;
        let proxy = fake_proxy(vec![
            ("93.184.216.34:80".into(), public),
            ("rebind-http.test".into(), internal),
            ("rebind-http.test:80".into(), internal),
        ], None).await;

        let body = client_through(&proxy).get("http://rebind-http.test/page?q=1").send().await.unwrap().text().await.unwrap();

        assert_eq!(body, "public");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
        let heads = proxy.heads();
        assert_eq!(proxy.first_lines(), vec!["GET http://93.184.216.34:80/page?q=1 HTTP/1.1"]);
        assert!(heads[0].to_ascii_lowercase().contains("host: rebind-http.test\r\n"), "{}", heads[0]);
        assert!(!heads[0].to_ascii_lowercase().contains("proxy-authorization"), "the loopback token must not reach the proxy: {}", heads[0]);
    }

    #[tokio::test]
    async fn https_tunnels_to_the_checked_address_and_keeps_the_tls_name() {
        name("rebind-tls.test", "93.184.216.34");
        let (public, ca) = tls_origin("rebind-tls.test", "public over tls").await;
        let (internal, internal_hits) = origin("internal").await;
        let proxy = fake_proxy(vec![
            ("93.184.216.34:443".into(), public),
            ("rebind-tls.test:443".into(), internal),
        ], None).await;
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(pinned_proxy_url(&proxy.url, false)).unwrap())
            .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes()).unwrap())
            .build()
            .unwrap();

        // The certificate names only rebind-tls.test, so the handshake proves
        // the client still verified that name through the pinned tunnel.
        let body = client.get("https://rebind-tls.test/").send().await.unwrap().text().await.unwrap();

        assert_eq!(body, "public over tls");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
        assert_eq!(proxy.first_lines(), vec!["CONNECT 93.184.216.34:443 HTTP/1.1"]);
    }

    #[tokio::test]
    async fn a_name_with_any_forbidden_address_never_reaches_the_proxy() {
        name("internal-only.test", "127.0.0.1");
        name("mixed.test", "93.184.216.34");
        name("mixed.test", "169.254.169.254");
        let (internal, internal_hits) = origin("internal").await;
        let proxy = fake_proxy(vec![
            ("internal-only.test:80".into(), internal),
            ("internal-only.test:443".into(), internal),
            ("mixed.test:80".into(), internal),
        ], None).await;
        let client = client_through(&proxy);

        for url in ["http://internal-only.test/", "https://internal-only.test/", "http://mixed.test/", "http://127.0.0.1:9/", "http://[::1]:9/"] {
            let result = client.get(url).send().await;
            assert!(result.is_err(), "{url} must fail in the client, not arrive as a response: {result:?}");
        }
        assert!(proxy.heads().is_empty(), "{:?}", proxy.heads());
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_loopback_proxy_needs_its_token_and_forwards_the_real_credentials() {
        name("credentials.test", "93.184.216.34");
        let (public, _) = origin("public").await;
        let proxy = fake_proxy(vec![("93.184.216.34:80".into(), public)], Some("user:p%40ss")).await;
        let pinned = pinned_proxy_url(&proxy.url, false);
        assert_eq!(pinned_proxy_url(&proxy.url, false), pinned, "one loopback proxy per upstream");

        let anonymous = Url::parse(&pinned).unwrap();
        let without_token = format!("http://{}:{}", anonymous.host_str().unwrap(), anonymous.port().unwrap());
        let refused = reqwest::Client::builder().proxy(reqwest::Proxy::all(without_token).unwrap()).build().unwrap()
            .get("http://credentials.test/").send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::PROXY_AUTHENTICATION_REQUIRED);
        assert!(proxy.heads().is_empty());

        let body = client_through(&proxy).get("http://credentials.test/").send().await.unwrap().text().await.unwrap();
        assert_eq!(body, "public");
        let expected = format!("proxy-authorization: basic {}\r\n", base64::engine::general_purpose::STANDARD.encode("user:p@ss").to_ascii_lowercase());
        assert!(proxy.heads()[0].to_ascii_lowercase().contains(&expected), "{}", proxy.heads()[0]);
    }

    #[test]
    fn only_http_proxies_with_the_guard_on_are_pinned() {
        for unchanged in ["socks5://127.0.0.1:1080", "socks5h://127.0.0.1:1080", "socks4a://127.0.0.1:1080", "not a url"] {
            assert_eq!(pinned_proxy_url(unchanged, false), unchanged);
        }
        assert_eq!(pinned_proxy_url("http://127.0.0.1:3128", true), "http://127.0.0.1:3128");
    }

    #[tokio::test]
    async fn onion_names_are_left_for_the_proxy_to_resolve() {
        assert_eq!(pin(&lookup(), "example2zxdsjqbe.onion", 80).await.unwrap(), "example2zxdsjqbe.onion:80");
        assert_eq!(pin(&lookup(), "93.184.216.34", 443).await.unwrap(), "93.184.216.34:443");
        assert!(pin(&lookup(), "0.0.0.0", 80).await.is_err());
    }

    /// Answers every request with a redirect to `location`.
    async fn redirecting_origin(location: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let response = format!("HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        address
    }

    /// A page on `first.test` that redirects to `second.test`; the proxy would
    /// send either name to the internal server.
    async fn redirect_through_proxy(first: &str, second: &'static str) -> (FakeProxy, Arc<AtomicUsize>) {
        name(first, "93.184.216.34");
        name(second.trim_start_matches("http://").trim_end_matches('/'), "93.184.216.35");
        let redirect = redirecting_origin(second).await;
        let (target, _) = origin("redirect target").await;
        let (internal, internal_hits) = origin("internal").await;
        let routes = vec![
            ("93.184.216.34:80".to_string(), redirect),
            ("93.184.216.35:80".to_string(), target),
            (format!("{first}:80"), internal),
            (format!("{first}"), internal),
            (format!("{}:80", second.trim_start_matches("http://").trim_end_matches('/')), internal),
        ];
        (fake_proxy(routes, None).await, internal_hits)
    }

    #[tokio::test]
    async fn navigation_and_its_redirects_are_pinned() {
        let (proxy, internal_hits) = redirect_through_proxy("nav-first.test", "http://nav-second.test/").await;
        let client = crate::ObscuraHttpClient::with_full_options(Arc::new(crate::CookieJar::new()), Some(&proxy.url), false);

        let response = client.fetch(&Url::parse("http://nav-first.test/").unwrap()).await.unwrap();

        assert_eq!(String::from_utf8_lossy(&response.body), "redirect target");
        assert_eq!(response.url.as_str(), "http://nav-second.test/");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
        assert_eq!(proxy.first_lines(), vec![
            "GET http://93.184.216.34:80/ HTTP/1.1",
            "GET http://93.184.216.35:80/ HTTP/1.1",
        ]);
    }

    /// Scripted fetch()/XHR in a page uses the context's own request client.
    #[tokio::test]
    async fn the_scripted_fetch_client_is_pinned() {
        name("script.test", "93.184.216.34");
        let (public, _) = origin("public").await;
        let (internal, internal_hits) = origin("internal").await;
        let proxy = fake_proxy(vec![("93.184.216.34:80".into(), public), ("script.test:80".into(), internal)], None).await;
        let client = crate::ObscuraHttpClient::with_full_options(Arc::new(crate::CookieJar::new()), Some(&proxy.url), false);

        let body = client.request_client().await.get("http://script.test/api").send().await.unwrap().text().await.unwrap();

        assert_eq!(body, "public");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
        assert_eq!(proxy.first_lines(), vec!["GET http://93.184.216.34:80/api HTTP/1.1"]);
    }

    #[cfg(feature = "stealth")]
    #[tokio::test]
    async fn the_stealth_client_and_its_redirects_are_pinned() {
        let (proxy, internal_hits) = redirect_through_proxy("stealth-first.test", "http://stealth-second.test/").await;
        let client = crate::StealthHttpClient::with_proxy(Arc::new(crate::CookieJar::new()), Some(&proxy.url), false);

        let response = client.fetch(&Url::parse("http://stealth-first.test/").unwrap()).await.unwrap();

        assert_eq!(String::from_utf8_lossy(&response.body), "redirect target");
        assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
        assert_eq!(proxy.first_lines(), vec![
            "GET http://93.184.216.34:80/ HTTP/1.1",
            "GET http://93.184.216.35:80/ HTTP/1.1",
        ]);
    }

    /// An HTTPS origin whose certificate names only `host`; returns its address and CA.
    async fn tls_origin(host: &str, body: &'static str) -> (SocketAddr, String) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec![host.to_string()]).unwrap().signed_by(&leaf_key, &ca, &ca_key).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![rustls::pki_types::CertificateDer::from(leaf.der().to_vec())],
                rustls::pki_types::PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into()),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(stream).await else { return };
                    let mut buf = [0u8; 4096];
                    let _ = tls.read(&mut buf).await;
                    let response = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    let _ = tls.write_all(response.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        (address, ca.pem())
    }
}
