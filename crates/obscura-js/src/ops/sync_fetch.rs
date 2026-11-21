//! Synchronous XMLHttpRequest (`xhr.open(method, url, false)`), issue #908.
//!
//! A sync `send()` must finish the whole exchange before it returns, while
//! the page's JS thread is blocked. The page's own tokio runtime runs on that
//! same thread, so `op_fetch_url`'s future can never make progress inside the
//! call. The request therefore runs on a dedicated process-wide runtime, and
//! the op waits for its result. Nothing on the page's event loop is polled
//! while it waits, which is the observable behaviour of a sync XHR.
//!
//! The transport mirrors `op_fetch_url`: the page's cookie jar (send and
//! store), the context proxy and private-network policy, the SSRF gate on
//! every redirect hop, the CORS preflight and response checks, the
//! per-request timeout and the body cap. The request is recorded as a
//! JS network event, so CDP clients see it like any other XHR.
//!
//! Not covered: Fetch-domain interception (pausing the request would need the
//! CDP consumer to answer while this thread is blocked) and the stealth (wreq)
//! transport. Both fall back to the plain transport.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use base64::Engine as _;
use deno_core::OpState;
use deno_core::op2;
use obscura_net::{CookieJar, SameSiteContext, same_site};

use super::{
    BASE64, FETCH_REDIRECT_LIMIT, FetchCredentials, JsNetworkEvent, MAX_FETCHED_URLS,
    SharedState, StoredNetworkResponseBody, build_request_client, cors_response_allows,
    cors_unsafe_request_header_names, fetch_max_body_bytes, fetch_timeout, glob_match,
    is_cors_safelisted_method, next_fetch_request_id, parse_cors_header_list,
    preflight_allows_header, preflight_allows_method, push_capped, push_js_network_event,
    read_body_capped, record_network_completion, request_origin, sanitize_redirect_headers,
    sends_origin_header, validate_fetch_url, visible_response_headers,
};

const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

/// Everything the transport needs, owned and `Send`, so it can leave the
/// JS thread.
struct SyncRequest {
    url: String,
    method: reqwest::Method,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    page_origin: String,
    credentials: FetchCredentials,
    cookie_jar: Option<Arc<CookieJar>>,
    proxy_url: Option<String>,
    allow_private_network: bool,
}

enum SyncOutcome {
    Response {
        status: u16,
        url: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
        redirected: bool,
    },
    /// Refused by policy (SSRF gate, redirect limit) or a transport error.
    Failed(String),
    Cors(String),
}

fn sync_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("obscura-sync-xhr")
            .enable_all()
            .build()
            .expect("sync XHR tokio runtime")
    })
}

/// Clients are only ever used on `sync_runtime()`, so their connection pools
/// never outlive the runtime that owns them (#453).
fn sync_client(proxy_url: Option<&str>, allow_private_network: bool) -> Result<reqwest::Client, String> {
    static CLIENTS: std::sync::OnceLock<std::sync::Mutex<HashMap<(String, bool), reqwest::Client>>> =
        std::sync::OnceLock::new();
    let key = (proxy_url.unwrap_or("").to_string(), allow_private_network);
    let clients = CLIENTS.get_or_init(Default::default);
    if let Some(client) = clients.lock().ok().and_then(|map| map.get(&key).cloned()) {
        return Ok(client);
    }
    let client = build_request_client(proxy_url, allow_private_network)?;
    if let Ok(mut map) = clients.lock() {
        map.entry(key).or_insert_with(|| client.clone());
    }
    Ok(client)
}

fn json_error(url: &str, kind: &str, message: &str) -> String {
    serde_json::json!({ "status": 0, "body": "", "url": url, "headers": {}, kind: true, "error": message })
        .to_string()
}

/// Returns the JSON shape `op_fetch_url` uses, with the body as `bodyBase64`.
/// A failed request returns `failed` or `corsBlocked`; the JS side turns
/// either into the NetworkError a sync XHR throws.
#[op2]
#[string]
pub(super) fn op_fetch_url_sync(
    state: &mut OpState,
    #[string] url: String,
    #[string] method: String,
    #[string] headers_json: String,
    #[buffer] body: &[u8],
    #[string] origin: String,
    #[string] credentials: String,
) -> String {
    let gs = state.borrow::<SharedState>().clone();
    let (cookie_jar, proxy_url, allow_private_network, page_in_flight, request_id) = {
        let mut gs = gs.borrow_mut();
        if gs
            .blocked_urls
            .iter()
            .any(|pattern| pattern == "*" || url.contains(pattern.as_str()) || glob_match(pattern, &url))
        {
            return json_error(&url, "blocked", "blocked by URL pattern");
        }
        push_capped(&mut gs.fetched_urls, url.clone(), MAX_FETCHED_URLS);
        if gs.intercept_enabled && gs.intercept_tx.is_some() {
            tracing::debug!("sync XHR {} bypasses Fetch interception", url);
        }
        let client = gs.http_client.as_ref();
        (
            gs.cookie_jar.clone(),
            client.and_then(|c| c.proxy_url().map(str::to_string)),
            client.is_some_and(|c| c.allow_private_network),
            Arc::clone(&gs.page_in_flight),
            next_fetch_request_id(),
        )
    };

    let gate_error = match url::Url::parse(&url) {
        Ok(parsed) => validate_fetch_url(&parsed, allow_private_network).err(),
        Err(err) => Some(format!("Unparsable URL blocked: {}", err)),
    };
    if let Some(error) = gate_error {
        return json_error(&url, "blocked", &error);
    }

    let req_method: reqwest::Method = method.parse().unwrap_or(reqwest::Method::GET);
    let page_origin = if origin.is_empty() { request_origin(&url).unwrap_or_default() } else { origin };
    let credentials = FetchCredentials::parse(&credentials);
    let request = SyncRequest {
        url: url.clone(),
        method: req_method.clone(),
        headers: serde_json::from_str(&headers_json).unwrap_or_default(),
        body: body.to_vec(),
        page_origin: page_origin.clone(),
        credentials,
        cookie_jar,
        proxy_url,
        allow_private_network,
    };

    page_in_flight.fetch_add(1, Ordering::Relaxed);
    let outcome = run_blocking(request);
    page_in_flight.fetch_sub(1, Ordering::Relaxed);

    let mut event = JsNetworkEvent {
        request_id: request_id.clone(),
        intercepted: false,
        url: url.clone(),
        method: req_method.as_str().to_string(),
        resource_type: "XHR".to_string(),
        status: 0,
        response_headers: HashMap::new(),
        body_size: 0,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64(),
        error_text: Some("net::ERR_FAILED".to_string()),
    };
    let (status, final_url, headers, body, redirected) = match outcome {
        SyncOutcome::Response { status, url, headers, body, redirected } => (status, url, headers, body, redirected),
        SyncOutcome::Failed(error) => {
            push_js_network_event(&mut gs.borrow_mut(), event);
            return json_error(&url, "failed", &error);
        }
        SyncOutcome::Cors(error) => {
            push_js_network_event(&mut gs.borrow_mut(), event);
            return json_error(&url, "corsBlocked", &error);
        }
    };

    event.url = final_url.clone();
    event.status = status;
    event.response_headers = headers.clone();
    event.body_size = body.len();
    event.error_text = None;
    record_network_completion(&mut gs.borrow_mut(), event, || StoredNetworkResponseBody {
        body: String::from_utf8_lossy(&body).into_owned(),
        base64_encoded: false,
    });

    let final_is_cross_origin = request_origin(&final_url).is_some_and(|o| o != page_origin);
    serde_json::json!({
        "status": status,
        "statusText": reqwest::StatusCode::from_u16(status).ok().and_then(|s| s.canonical_reason()).unwrap_or(""),
        "requestId": request_id,
        "url": final_url,
        "redirected": redirected,
        "headers": visible_response_headers(&headers, final_is_cross_origin, credentials),
        "bodyBase64": BASE64.encode(&body),
    })
    .to_string()
}

/// Run the transport on `sync_runtime()` and block this thread until it
/// finishes. Preflight plus up to FETCH_REDIRECT_LIMIT hops are each bounded
/// by fetch_timeout(); the whole exchange is capped too, so a slow redirect
/// chain cannot hold the page thread for minutes.
fn run_blocking(request: SyncRequest) -> SyncOutcome {
    let deadline = fetch_timeout() * 2;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    sync_runtime().spawn(async move {
        let outcome = match tokio::time::timeout(deadline, transport(request)).await {
            Ok(outcome) => outcome,
            Err(_) => SyncOutcome::Failed("synchronous XHR timed out".to_string()),
        };
        let _ = tx.send(outcome);
    });
    rx.recv_timeout(deadline + std::time::Duration::from_secs(1))
        .unwrap_or_else(|_| SyncOutcome::Failed("synchronous XHR timed out".to_string()))
}

async fn transport(request: SyncRequest) -> SyncOutcome {
    let SyncRequest {
        url, method, headers, body, page_origin, credentials, cookie_jar, proxy_url,
        allow_private_network,
    } = request;
    let client = match sync_client(proxy_url.as_deref(), allow_private_network) {
        Ok(client) => client,
        Err(error) => return SyncOutcome::Failed(error),
    };
    let is_cross_origin = request_origin(&url).is_some_and(|o| o != page_origin);

    // XHR always runs in cors mode.
    let unsafe_header_names = if is_cross_origin { cors_unsafe_request_header_names(&headers) } else { Vec::new() };
    if is_cross_origin && (!is_cors_safelisted_method(&method) || !unsafe_header_names.is_empty()) {
        if let Err(error) =
            preflight(&client, &url, &method, &unsafe_header_names, &page_origin, credentials).await
        {
            return SyncOutcome::Cors(error);
        }
    }

    // Redirects are followed by hand so every hop passes the SSRF gate.
    let cookie_initiator = url::Url::parse(&page_origin).ok();
    let mut current_url = url;
    let mut current_method = method;
    let mut current_body = body;
    let mut current_headers = headers;
    let mut redirects_followed = 0usize;
    let response = loop {
        let mut req = client.request(current_method.clone(), &current_url).timeout(fetch_timeout());
        let hop_cross_origin = request_origin(&current_url).is_some_and(|o| o != page_origin);
        if sends_origin_header(current_method.as_str(), hop_cross_origin) {
            req = req.header("Origin", &page_origin);
        }
        let credentials_allowed = credentials.allows(&page_origin, &current_url);
        let parsed_url = url::Url::parse(&current_url).ok();
        if let (true, Some(jar), Some(target)) = (credentials_allowed, cookie_jar.as_ref(), parsed_url.as_ref()) {
            let context = if cookie_initiator.as_ref().is_some_and(|source| same_site(source, target)) {
                SameSiteContext::SameSite
            } else {
                SameSiteContext::CrossSite
            };
            let cookie_header = jar.get_cookie_header_in_context(target, context);
            if !cookie_header.is_empty() {
                req = req.header("Cookie", cookie_header);
            }
        }
        if !current_headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent")) {
            req = req.header("User-Agent", DEFAULT_USER_AGENT);
        }
        for (k, v) in &current_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if !current_body.is_empty() {
            req = req.body(current_body.clone());
        }
        let resp = match req.send().await {
            Ok(resp) => resp,
            Err(error) => return SyncOutcome::Failed(error.to_string()),
        };
        if let (true, Some(jar), Some(target)) = (credentials_allowed, cookie_jar.as_ref(), parsed_url.as_ref()) {
            for value in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                if let Ok(value) = value.to_str() {
                    jar.set_cookie(value, target);
                }
            }
        }
        if hop_cross_origin {
            let allowed = header_str(resp.headers(), "access-control-allow-origin");
            let allow_credentials = header_str(resp.headers(), "access-control-allow-credentials");
            if !cors_response_allows(credentials, &page_origin, allowed, allow_credentials) {
                return SyncOutcome::Cors(format!(
                    "CORS error: Origin '{}' not in Access-Control-Allow-Origin '{}'",
                    page_origin, allowed
                ));
            }
        }
        if !resp.status().is_redirection() {
            break resp;
        }
        let Some(location) = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
        else {
            break resp;
        };
        let Some(base) = parsed_url else { break resp };
        let Ok(next_url) = base.join(&location) else { break resp };
        if let Err(reason) = validate_fetch_url(&next_url, allow_private_network) {
            return SyncOutcome::Failed(format!("Redirect to forbidden URL blocked: {}", reason));
        }
        redirects_followed += 1;
        if redirects_followed > FETCH_REDIRECT_LIMIT {
            return SyncOutcome::Failed(format!("Too many redirects (>{})", FETCH_REDIRECT_LIMIT));
        }
        let status = resp.status().as_u16();
        let downgraded_to_get = matches!(status, 301 | 302 | 303);
        if downgraded_to_get {
            current_method = reqwest::Method::GET;
            current_body.clear();
        }
        sanitize_redirect_headers(&mut current_headers, base.origin() != next_url.origin(), downgraded_to_get);
        current_url = next_url.to_string();
    };

    let status = response.status().as_u16();
    let headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    match read_body_capped(response, fetch_max_body_bytes()).await {
        Ok(body) => SyncOutcome::Response {
            status,
            url: current_url,
            headers,
            body,
            redirected: redirects_followed > 0,
        },
        Err(error) => SyncOutcome::Failed(error.to_string()),
    }
}

async fn preflight(
    client: &reqwest::Client,
    url: &str,
    method: &reqwest::Method,
    unsafe_header_names: &[String],
    page_origin: &str,
    credentials: FetchCredentials,
) -> Result<(), String> {
    let mut request = client
        .request(reqwest::Method::OPTIONS, url)
        .timeout(fetch_timeout())
        .header("Origin", page_origin)
        .header("Access-Control-Request-Method", method.as_str());
    if !unsafe_header_names.is_empty() {
        request = request.header("Access-Control-Request-Headers", unsafe_header_names.join(","));
    }
    let response = request.send().await.map_err(|e| format!("CORS preflight failed: {}", e))?;
    if !response.status().is_success() {
        return Err(format!("CORS preflight returned HTTP {}", response.status()));
    }
    let headers = response.headers();
    let allowed_origin = header_str(headers, "access-control-allow-origin");
    let allow_credentials = header_str(headers, "access-control-allow-credentials");
    if !cors_response_allows(credentials, page_origin, allowed_origin, allow_credentials) {
        return Err(format!(
            "CORS preflight: Origin '{}' not allowed by Access-Control-Allow-Origin '{}'",
            page_origin, allowed_origin
        ));
    }
    let credentialed = credentials == FetchCredentials::Include;
    let allowed_methods = parse_cors_header_list(headers, "access-control-allow-methods")
        .ok_or("CORS preflight returned an invalid Access-Control-Allow-Methods value")?;
    if !preflight_allows_method(method, &allowed_methods, credentialed) {
        return Err(format!("CORS preflight did not allow method '{}'", method));
    }
    let allowed_headers = parse_cors_header_list(headers, "access-control-allow-headers")
        .ok_or("CORS preflight returned an invalid Access-Control-Allow-Headers value")?;
    if let Some(name) = unsafe_header_names
        .iter()
        .find(|name| !preflight_allows_header(name, &allowed_headers, credentialed))
    {
        return Err(format!("CORS preflight did not allow request header '{}'", name));
    }
    Ok(())
}

fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}
