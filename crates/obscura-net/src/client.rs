use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose, Engine as _};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Client, Method};
use tokio::sync::RwLock;
use url::Url;

use crate::cookies::CookieJar;
use crate::interceptor::{InterceptAction, RequestInterceptor};

pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
pub const DEFAULT_SEC_CH_UA: &str =
    "\"Google Chrome\";v=\"124\", \"Not.A/Brand\";v=\"8\", \"Chromium\";v=\"124\"";
pub const DEFAULT_SEC_CH_UA_FULL_VERSION_LIST: &str = "\"Google Chrome\";v=\"124.0.0.0\", \"Not.A/Brand\";v=\"8.0.0.0\", \"Chromium\";v=\"124.0.0.0\"";
pub const DEFAULT_SEC_CH_UA_PLATFORM: &str = "\"macOS\"";
pub const DEFAULT_SEC_CH_UA_PLATFORM_VERSION: &str = "\"14.4.1\"";

#[derive(Debug, Clone)]
pub struct Response {
    pub url: Url,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub set_cookie_headers: Vec<String>,
    pub body: Vec<u8>,
    pub redirected_from: Vec<Url>,
}

impl Response {
    pub fn text(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.body.clone())
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
    pub body: Vec<u8>,
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

fn validate_url(url: &Url) -> Result<(), ObscuraNetError> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" && scheme != "file" && scheme != "data" {
        return Err(ObscuraNetError::Network(format!(
            "Forbidden URL scheme '{}' - only http, https, file, and data are allowed",
            scheme
        )));
    }

    if scheme == "file" || scheme == "data" {
        return Ok(());
    }

    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if ip.is_loopback()
                    || ip.is_private()
                    || ip.is_link_local()
                    || ip.is_broadcast()
                    || ip.is_documentation()
                {
                    return Err(ObscuraNetError::Network(format!(
                        "Access to private/internal IP address {} is not allowed",
                        ip
                    )));
                }
            }
            url::Host::Ipv6(ip) => {
                if ip.is_loopback() || ip.is_unicast_link_local() {
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
        set_cookie_headers: Vec::new(),
        body,
        redirected_from: Vec::new(),
    })
}

fn fetch_data_url(url: &Url) -> Result<Response, ObscuraNetError> {
    let raw = url.as_str();
    let Some(rest) = raw.strip_prefix("data:") else {
        return Err(ObscuraNetError::Network("Invalid data URL".to_string()));
    };
    let comma = rest
        .find(',')
        .ok_or_else(|| ObscuraNetError::Network("Invalid data URL".to_string()))?;
    let metadata = &rest[..comma];
    let data = &rest[comma + 1..];
    let metadata_lower = metadata.to_ascii_lowercase();
    let is_base64 = metadata_lower
        .split(';')
        .any(|part| part.trim() == "base64");
    let content_type = metadata
        .split(';')
        .find(|part| part.contains('/'))
        .filter(|part| !part.is_empty())
        .unwrap_or("text/plain;charset=US-ASCII");
    let body = if is_base64 {
        general_purpose::STANDARD
            .decode(data)
            .map_err(|e| ObscuraNetError::Network(format!("Invalid data URL base64: {}", e)))?
    } else {
        percent_decode(data)
    };
    let mut headers = HashMap::new();
    headers.insert("content-type".to_string(), content_type.to_string());
    Ok(Response {
        url: url.clone(),
        status: 200,
        headers,
        set_cookie_headers: Vec::new(),
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
}

impl ObscuraHttpClient {
    pub fn new() -> Self {
        Self::with_cookie_jar(Arc::new(CookieJar::new()))
    }

    pub fn with_cookie_jar(cookie_jar: Arc<CookieJar>) -> Self {
        Self::with_options(cookie_jar, None)
    }

    pub fn with_options(cookie_jar: Arc<CookieJar>, proxy_url: Option<&str>) -> Self {
        ObscuraHttpClient {
            client: tokio::sync::OnceCell::new(),
            proxy_url: proxy_url.map(|s| s.to_string()),
            cookie_jar,
            user_agent: RwLock::new(DEFAULT_USER_AGENT.to_string()),
            extra_headers: RwLock::new(HashMap::new()),
            interceptor: RwLock::new(None),
            on_request: RwLock::new(Vec::new()),
            on_response: RwLock::new(Vec::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            timeout: Duration::from_secs(30),
            block_trackers: false,
        }
    }

    async fn get_client(&self) -> &Client {
        self.client
            .get_or_init(|| async {
                let mut builder = Client::builder()
                    .redirect(Policy::none())
                    .timeout(self.timeout)
                    .danger_accept_invalid_certs(false);

                if let Some(ref proxy) = self.proxy_url {
                    if let Ok(p) = reqwest::Proxy::all(proxy.as_str()) {
                        builder = builder.proxy(p);
                    }
                }

                builder.build().expect("failed to build HTTP client")
            })
            .await
    }

    pub async fn fetch(&self, url: &Url) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::GET, url, None).await
    }

    pub async fn post_form(&self, url: &Url, body: &str) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method(Method::POST, url, Some(body.as_bytes().to_vec()))
            .await
    }

    pub async fn request_bytes_once(
        &self,
        method: Method,
        url: &Url,
        headers: HashMap<String, String>,
        body: Option<Vec<u8>>,
        resource_type: ResourceType,
    ) -> Result<Response, ObscuraNetError> {
        self.request_bytes_once_inner(method, url, headers, body, resource_type, true)
            .await
    }

    async fn request_bytes_once_inner(
        &self,
        method: Method,
        url: &Url,
        headers: HashMap<String, String>,
        body: Option<Vec<u8>>,
        resource_type: ResourceType,
        validate: bool,
    ) -> Result<Response, ObscuraNetError> {
        let mut headers = normalize_header_names(headers);
        if validate {
            validate_url(url)?;
        }
        if url.scheme() == "data" {
            return fetch_data_url(url);
        }
        if url.scheme() == "file" {
            return fetch_file_url(url).await;
        }
        if self.block_trackers
            && url
                .host_str()
                .map(crate::blocklist::is_blocked)
                .unwrap_or(false)
        {
            return Ok(Response {
                status: 0,
                url: url.clone(),
                headers: HashMap::new(),
                set_cookie_headers: Vec::new(),
                body: Vec::new(),
                redirected_from: Vec::new(),
            });
        }

        let mut request_info = RequestInfo {
            url: url.clone(),
            method: method.to_string(),
            headers: headers.clone(),
            body: body.clone().unwrap_or_default(),
            resource_type,
        };
        if let Some(interceptor) = self.interceptor.read().await.as_ref() {
            match interceptor.intercept(&request_info).await {
                InterceptAction::Continue => {}
                InterceptAction::Block => {
                    return Err(ObscuraNetError::Blocked(url.to_string()));
                }
                InterceptAction::Fulfill(response) => return Ok(response),
                InterceptAction::ModifyHeaders(modified) => {
                    headers.extend(normalize_header_names(modified));
                    request_info.headers = headers.clone();
                }
            }
        }
        for callback in self.on_request.read().await.iter() {
            callback(&request_info);
        }

        let mut header_map = HeaderMap::new();
        for (name, value) in &headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                header_map.insert(name, value);
            }
        }
        let mut builder = self
            .get_client()
            .await
            .request(method, url.as_str())
            .headers(header_map);
        if let Some(body) = body {
            builder = builder.body(body);
        }

        self.in_flight
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let response = builder.send().await.map_err(|error| {
            self.in_flight
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            ObscuraNetError::Network(format!("{}: {}", url, error))
        })?;
        self.in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

        let status = response.status().as_u16();
        let response_headers = response
            .headers()
            .iter()
            .filter(|(name, _)| *name != reqwest::header::SET_COOKIE)
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or("").to_string(),
                )
            })
            .collect::<HashMap<_, _>>();
        let set_cookie_headers = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_string))
            .collect();
        let is_redirect = matches!(status, 301 | 302 | 303 | 307 | 308)
            && response_headers.contains_key("location");
        let response_body = if is_redirect {
            Vec::new()
        } else {
            response
                .bytes()
                .await
                .map_err(|error| {
                    ObscuraNetError::Network(format!("Failed to read body: {}", error))
                })?
                .to_vec()
        };
        let response = Response {
            url: url.clone(),
            status,
            headers: response_headers,
            set_cookie_headers,
            body: response_body,
            redirected_from: Vec::new(),
        };
        for callback in self.on_response.read().await.iter() {
            callback(&request_info, &response);
        }
        Ok(response)
    }

    pub async fn fetch_with_method(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
    ) -> Result<Response, ObscuraNetError> {
        self.fetch_with_method_and_headers(initial_method, url, initial_body, HashMap::new())
            .await
    }

    pub async fn fetch_with_method_and_headers(
        &self,
        initial_method: Method,
        url: &Url,
        initial_body: Option<Vec<u8>>,
        initial_headers: HashMap<String, String>,
    ) -> Result<Response, ObscuraNetError> {
        validate_url(url)?;
        let mut method = initial_method;
        let mut body = initial_body;
        let mut request_headers = normalize_header_names(initial_headers);
        strip_browser_managed_request_headers(&mut request_headers);
        let mut current_url = url.clone();
        let mut redirects = Vec::new();

        for _ in 0..20 {
            let configured_ua = self.user_agent.read().await.clone();
            let mut headers = normalize_header_names(self.extra_headers.read().await.clone());
            headers.extend(request_headers.clone());
            headers.insert(
                "user-agent".to_string(),
                if configured_ua.trim().is_empty() {
                    DEFAULT_USER_AGENT.to_string()
                } else {
                    configured_ua
                },
            );
            headers
                .entry("accept".to_string())
                .or_insert_with(|| "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7".to_string());
            headers
                .entry("accept-language".to_string())
                .or_insert_with(|| "en-US,en;q=0.9".to_string());
            headers.insert("sec-ch-ua".to_string(), DEFAULT_SEC_CH_UA.to_string());
            headers.insert(
                "sec-ch-ua-full-version-list".to_string(),
                DEFAULT_SEC_CH_UA_FULL_VERSION_LIST.to_string(),
            );
            headers.insert("sec-ch-ua-mobile".to_string(), "?0".to_string());
            headers.insert(
                "sec-ch-ua-platform".to_string(),
                DEFAULT_SEC_CH_UA_PLATFORM.to_string(),
            );
            headers.insert(
                "sec-ch-ua-platform-version".to_string(),
                DEFAULT_SEC_CH_UA_PLATFORM_VERSION.to_string(),
            );
            headers.insert("sec-fetch-dest".to_string(), "document".to_string());
            headers.insert("sec-fetch-mode".to_string(), "navigate".to_string());
            headers.insert("sec-fetch-site".to_string(), "none".to_string());
            headers.insert("sec-fetch-user".to_string(), "?1".to_string());
            headers.insert("upgrade-insecure-requests".to_string(), "1".to_string());
            let cookie_header = self.cookie_jar.get_cookie_header(&current_url);
            if !cookie_header.is_empty() {
                headers.insert("cookie".to_string(), cookie_header);
            }
            if body.is_some()
                && method == Method::POST
                && !headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case("content-type"))
            {
                headers.insert(
                    "content-type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                );
            }

            let mut response = self
                .request_bytes_once(
                    method.clone(),
                    &current_url,
                    headers,
                    body.clone(),
                    ResourceType::Document,
                )
                .await?;
            for set_cookie in &response.set_cookie_headers {
                self.cookie_jar.set_cookie(set_cookie, &current_url);
            }

            if let Some(location) = response.header("location") {
                if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                    let next_url = current_url.join(location).map_err(|error| {
                        ObscuraNetError::Network(format!("Invalid redirect URL: {}", error))
                    })?;
                    validate_http_redirect(&current_url, &next_url)?;
                    if current_url.origin() != next_url.origin() {
                        request_headers.remove("authorization");
                        request_headers.remove("proxy-authorization");
                    }
                    redirects.push(current_url);
                    current_url = next_url;
                    if apply_redirect_method(response.status, &mut method, &mut body) {
                        request_headers.retain(|name, _| {
                            !matches!(
                                name.as_str(),
                                "content-type" | "content-encoding" | "content-language"
                            )
                        });
                    }
                    continue;
                }
            }

            response.url = current_url;
            response.redirected_from = redirects;
            return Ok(response);
        }

        Err(ObscuraNetError::TooManyRedirects(current_url.to_string()))
    }

    pub async fn set_user_agent(&self, ua: &str) {
        *self.user_agent.write().await = if ua.trim().is_empty() {
            DEFAULT_USER_AGENT.to_string()
        } else {
            ua.to_string()
        };
    }

    pub async fn set_extra_headers(&self, headers: HashMap<String, String>) {
        *self.extra_headers.write().await = normalize_header_names(headers);
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

fn normalize_header_names(headers: HashMap<String, String>) -> HashMap<String, String> {
    headers
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect()
}

fn strip_browser_managed_request_headers(headers: &mut HashMap<String, String>) {
    headers.retain(|name, _| {
        !matches!(
            name.as_str(),
            "cookie"
                | "content-length"
                | "host"
                | "origin"
                | "referer"
                | "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "user-agent"
                | "accept-language"
        ) && !name.starts_with("sec-")
            && !name.starts_with("proxy-")
    });
}

fn validate_http_redirect(current: &Url, next: &Url) -> Result<(), ObscuraNetError> {
    validate_url(next)?;
    if matches!(current.scheme(), "http" | "https") && !matches!(next.scheme(), "http" | "https") {
        return Err(ObscuraNetError::Network(format!(
            "HTTP redirect to forbidden scheme '{}'",
            next.scheme()
        )));
    }
    Ok(())
}

fn apply_redirect_method(status: u16, method: &mut Method, body: &mut Option<Vec<u8>>) -> bool {
    let switch_to_get = status == 303 && *method != Method::HEAD
        || matches!(status, 301 | 302) && *method == Method::POST;
    if switch_to_get {
        *method = Method::GET;
        *body = None;
    }
    switch_to_get
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                decoded.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }

    decoded
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_user_agent_uses_default_for_blank_value() {
        let client = ObscuraHttpClient::new();
        client.set_user_agent("").await;
        assert_eq!(
            client.user_agent.read().await.as_str(),
            DEFAULT_USER_AGENT,
            "blank UA overrides must not produce empty User-Agent headers"
        );
    }

    #[tokio::test]
    async fn fetch_supports_data_url_documents() {
        let client = ObscuraHttpClient::new();
        let url = Url::parse("data:text/html,%3Ctitle%3EData%3C/title%3E").unwrap();

        let response = client.fetch(&url).await.expect("data URL should load");

        assert_eq!(response.status, 200);
        assert_eq!(response.content_type(), Some("text/html"));
        assert_eq!(response.text().unwrap(), "<title>Data</title>");
    }

    #[tokio::test]
    async fn byte_transport_sends_and_receives_non_utf8_losslessly() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected = vec![0, 255, 1, 128, 7];
        let expected_for_server = expected.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 1024];
            let (header_end, content_length) = loop {
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                received.extend_from_slice(&buffer[..count]);
                if let Some(index) = received.windows(4).position(|window| window == b"\r\n\r\n") {
                    let header_end = index + 4;
                    let headers = String::from_utf8_lossy(&received[..index]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if received.len() >= header_end + content_length {
                        break (header_end, content_length);
                    }
                }
            };
            assert_eq!(
                &received[header_end..header_end + content_length],
                expected_for_server.as_slice()
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nSet-Cookie: secret=1; HttpOnly\r\nContent-Length: 3\r\nConnection: close\r\n\r\n\0\xff\x7f")
                .await
                .unwrap();
        });

        let client = ObscuraHttpClient::new();
        let url = Url::parse(&format!("http://{}/echo", address)).unwrap();
        let response = client
            .request_bytes_once_inner(
                Method::POST,
                &url,
                HashMap::from([(
                    "content-type".to_string(),
                    "application/octet-stream".to_string(),
                )]),
                Some(expected),
                ResourceType::Fetch,
                false,
            )
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(response.body, vec![0, 255, 127]);
        assert!(!response.headers.contains_key("set-cookie"));
        assert_eq!(
            response.set_cookie_headers,
            vec!["secret=1; HttpOnly".to_string()]
        );
    }

    #[test]
    fn http_redirects_cannot_target_file_or_data_urls() {
        let current = Url::parse("https://example.com/start").unwrap();
        assert!(
            validate_http_redirect(&current, &Url::parse("file:///tmp/secret").unwrap()).is_err()
        );
        assert!(
            validate_http_redirect(&current, &Url::parse("data:text/plain,secret").unwrap())
                .is_err()
        );
    }

    #[test]
    fn navigation_overrides_keep_author_headers_and_drop_managed_headers() {
        let mut headers = normalize_header_names(HashMap::from([
            ("X-Navigation-Override".to_string(), "kept".to_string()),
            ("Host".to_string(), "evil.example".to_string()),
            ("Cookie".to_string(), "forged=1".to_string()),
            ("Content-Length".to_string(), "999".to_string()),
        ]));
        strip_browser_managed_request_headers(&mut headers);

        assert_eq!(
            headers.get("x-navigation-override").map(String::as_str),
            Some("kept")
        );
        assert!(!headers.contains_key("host"));
        assert!(!headers.contains_key("cookie"));
        assert!(!headers.contains_key("content-length"));
    }

    #[test]
    fn redirect_method_rules_preserve_307_and_rewrite_post_302() {
        let original = vec![0, 255];
        let mut method = Method::POST;
        let mut body = Some(original.clone());
        assert!(!apply_redirect_method(307, &mut method, &mut body));
        assert_eq!(method, Method::POST);
        assert_eq!(body, Some(original));

        assert!(apply_redirect_method(302, &mut method, &mut body));
        assert_eq!(method, Method::GET);
        assert_eq!(body, None);
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
