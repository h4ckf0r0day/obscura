use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use deno_core::op2;
use deno_core::OpState;
use deno_core::Extension;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use obscura_dom::{DomTree, NodeData, NodeId};
use obscura_net::{CookieJar, ObscuraHttpClient};
use tokio::sync::Mutex;

pub type InterceptCallback = Arc<Mutex<Option<Box<dyn Fn(String, String, String) -> Option<(u16, String, String)> + Send + Sync>>>>;

#[derive(Debug)]
pub enum InterceptResolution {
    Continue {
        url: Option<String>,
        method: Option<String>,
        headers: Option<HashMap<String, String>>,
        body: Option<String>,
    },
    Fulfill {
        status: u16,
        headers: HashMap<String, String>,
        body: String,
    },
    Fail { reason: String },
}

pub struct InterceptedRequest {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub resource_type: String,
    pub resolver: tokio::sync::oneshot::Sender<InterceptResolution>,
}

pub struct ObscuraState {
    pub dom: Option<DomTree>,
    pub url: String,
    pub title: String,
    pub blocked_urls: Vec<String>,
    pub cookie_jar: Option<Arc<CookieJar>>,
    pub http_client: Option<Arc<ObscuraHttpClient>>,
    pub pending_navigation: Option<(String, String, String)>,
    pub intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<InterceptedRequest>>,
    pub intercept_counter: u64,
    pub intercept_enabled: bool,
}

impl ObscuraState {
    pub fn new() -> Self {
        ObscuraState {
            dom: None,
            url: "about:blank".to_string(),
            title: String::new(),
            blocked_urls: Vec::new(),
            cookie_jar: None,
            http_client: None,
            pending_navigation: None,
            intercept_tx: None,
            intercept_counter: 0,
            intercept_enabled: false,
        }
    }
}

pub type SharedState = Rc<RefCell<ObscuraState>>;

#[op2]
#[string]
fn op_dom(state: &OpState, #[string] cmd: String, #[string] arg1: String, #[string] arg2: String) -> String {
    let gs = state.borrow::<SharedState>().clone();
    let gs = gs.borrow();
    let dom = match &gs.dom {
        Some(d) => d,
        None => return "null".to_string(),
    };

    match cmd.as_str() {
        "document_node_id" => dom.document().index().to_string(),
        "document_title" => serde_json::to_string(&gs.title).unwrap_or("\"\"".into()),
        "document_url" => serde_json::to_string(&gs.url).unwrap_or("\"\"".into()),
        "document_element" => {
            for cid in dom.children(dom.document()) {
                if let Some(n) = dom.get_node(cid) {
                    if n.as_element().map(|name| name.local.as_ref() == "html").unwrap_or(false) {
                        return cid.index().to_string();
                    }
                }
            }
            "-1".into()
        }
        "document_doctype" => {
            for cid in dom.children(dom.document()) {
                if let Some(n) = dom.get_node(cid) {
                    if let obscura_dom::NodeData::Doctype { name, public_id, system_id } = &n.data {
                        return serde_json::json!({
                            "name": name,
                            "publicId": public_id,
                            "systemId": system_id,
                            "nodeId": cid.index(),
                        }).to_string();
                    }
                }
            }
            "null".into()
        }
        "get_element_by_id" => {
            dom.get_element_by_id(&arg1).map(|id| id.index().to_string()).unwrap_or("-1".into())
        }
        "query_selector" => {
            dom.query_selector(&arg1).ok().flatten().map(|id| id.index().to_string()).unwrap_or("-1".into())
        }
        "query_selector_all" => {
            let ids: Vec<i32> = dom.query_selector_all(&arg1).ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect()).unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "node_type" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.get_node(NodeId::new(nid)).map(|n| match &n.data {
                NodeData::Document => "9", NodeData::Element { .. } => "1", NodeData::Text { .. } => "3",
                NodeData::Comment { .. } => "8", NodeData::Doctype { .. } => "10", NodeData::ProcessingInstruction { .. } => "7",
            }).unwrap_or("0").into()
        }
        "node_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name: String = dom.get_node(NodeId::new(nid)).map(|n| match &n.data {
                NodeData::Document => "#document".to_string(), NodeData::Element { name, .. } => name.local.as_ref().to_ascii_uppercase(),
                NodeData::Text { .. } => "#text".to_string(), NodeData::Comment { .. } => "#comment".to_string(),
                NodeData::Doctype { name, .. } => name.clone(), NodeData::ProcessingInstruction { target, .. } => target.clone(),
            }).unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "text_content" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            serde_json::to_string(&dom.text_content(NodeId::new(nid))).unwrap_or("\"\"".into())
        }
        "parent_node" | "first_child" | "last_child" | "next_sibling" | "prev_sibling" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.get_node(NodeId::new(nid)).and_then(|n| match cmd.as_str() {
                "parent_node" => n.parent, "first_child" => n.first_child,
                "last_child" => n.last_child, "next_sibling" => n.next_sibling,
                "prev_sibling" => n.prev_sibling, _ => None,
            }).map(|id| id.index().to_string()).unwrap_or("-1".into())
        }
        "child_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom.children(NodeId::new(nid)).iter().map(|id| id.index() as i32).collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "tag_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name = dom.get_node(NodeId::new(nid)).and_then(|n| n.as_element().map(|name| name.local.as_ref().to_ascii_uppercase())).unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "get_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom.get_node(NodeId::new(nid)).and_then(|n| n.get_attribute(&arg2).map(|s| s.to_string()));
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "set_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            if let Some((name, value)) = arg2.split_once('\0') {
                if name == "id" {
                    let old_id = dom.get_node(node_id).and_then(|n| n.get_attribute("id").map(|s| s.to_string()));
                    dom.with_node_mut(node_id, |n| n.set_attribute(name, value.to_string()));
                    dom.update_id_index(node_id, old_id.as_deref(), Some(value));
                } else {
                    dom.with_node_mut(node_id, |n| n.set_attribute(name, value.to_string()));
                }
            }
            "true".into()
        }
        "inner_html" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            serde_json::to_string(&dom.inner_html(NodeId::new(nid))).unwrap_or("\"\"".into())
        }
        "outer_html" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            serde_json::to_string(&dom.outer_html(NodeId::new(nid))).unwrap_or("\"\"".into())
        }
        "append_child" => {
            let parent = arg1.parse::<u32>().unwrap_or(0);
            let child = arg2.parse::<u32>().unwrap_or(0);
            dom.append_child(NodeId::new(parent), NodeId::new(child));
            "true".into()
        }
        "remove_child" => {
            let child = arg1.parse::<u32>().unwrap_or(0);
            dom.detach(NodeId::new(child));
            "true".into()
        }
        "insert_before" => {
            let new_node = arg1.parse::<u32>().unwrap_or(0);
            let ref_node = arg2.parse::<u32>().unwrap_or(0);
            dom.insert_before(NodeId::new(ref_node), NodeId::new(new_node));
            "true".into()
        }
        "remove_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node_mut(NodeId::new(nid), |n| {
                if let NodeData::Element { attrs, .. } = &mut n.data {
                    attrs.retain(|a| a.name.local.as_ref() != arg2.as_str());
                }
            });
            "true".into()
        }
        "set_inner_html" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let target = NodeId::new(nid);
            let children = dom.children(target);
            for child in children {
                dom.detach(child);
            }
            if !arg2.is_empty() {
                let fragment = obscura_dom::parse_fragment(&arg2);
                let import_root = fragment.find_body_or_root();
                dom.import_children_from(target, &fragment, import_root);
            }
            "true".into()
        }
        "set_text_content" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node_mut(NodeId::new(nid), |n| {
                match &mut n.data {
                    NodeData::Text { contents } => { *contents = arg2.clone(); }
                    NodeData::Comment { contents } => { *contents = arg2.clone(); }
                    _ => {}
                }
            });
            "true".into()
        }
        "create_document_fragment" => {
            dom.new_node(NodeData::Document).index().to_string()
        }
        "create_element" => {
            dom.new_node(NodeData::Element {
                name: html5ever::QualName::new(None, html5ever::ns!(html), html5ever::LocalName::from(arg1.as_str())),
                attrs: vec![], template_contents: None, mathml_annotation_xml_integration_point: false,
            }).index().to_string()
        }
        "create_text_node" => {
            dom.new_node(NodeData::Text { contents: arg1.clone() }).index().to_string()
        }
        "create_comment_node" => {
            dom.new_node(NodeData::Comment { contents: arg1.clone() }).index().to_string()
        }
        "element_children" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom.children(NodeId::new(nid)).iter()
                .filter(|&&id| dom.get_node(id).map(|n| n.is_element()).unwrap_or(false))
                .map(|id| id.index() as i32).collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "has_child_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.get_node(NodeId::new(nid)).map(|n| n.first_child.is_some()).unwrap_or(false).to_string()
        }
        "contains" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let other = arg2.parse::<u32>().unwrap_or(0);
            dom.descendants(NodeId::new(nid)).contains(&NodeId::new(other)).to_string()
        }
        _ => "null".into(),
    }
}

#[op2(fast)]
fn op_console_msg(state: &OpState, #[string] level: &str, #[string] msg: &str) {
    let _ = state;
    match level {
        "warn" => tracing::warn!(target: "obscura::console", "{}", msg),
        "error" => tracing::error!(target: "obscura::console", "{}", msg),
        _ => tracing::info!(target: "obscura::console", "{}", msg),
    }
}

static SHARED_HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn get_shared_client() -> &'static reqwest::Client {
    SHARED_HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .build()
            .expect("failed to build shared reqwest::Client")
    })
}

#[op2(async)]
#[string]
async fn op_fetch_url(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[string] method: String,
    #[string] headers_json: String,
    #[string] body: String,
    #[string] origin: String,
    #[string] mode: String,
) -> Result<String, deno_error::JsErrorBox> {
    tracing::debug!("op_fetch_url called: {} {} (intercept check pending)", method, url);

    if let Ok(parsed_url) = url::Url::parse(&url) {
        if let Err(e) = validate_fetch_url(&parsed_url) {
            return Ok(serde_json::json!({
                "status": 0,
                "body": "",
                "url": url,
                "headers": {},
                "blocked": true,
                "error": e,
            }).to_string());
        }
    }

    let (cookie_jar, in_flight, intercept_tx) = {
        let state_borrow = state.borrow();
        let gs = state_borrow.borrow::<SharedState>().clone();
        let mut gs = gs.borrow_mut();
        for pattern in &gs.blocked_urls {
            if pattern == "*" || url.contains(pattern) || glob_match(pattern, &url) {
                return Ok(serde_json::json!({
                    "status": 0,
                    "body": "",
                    "url": url,
                    "headers": {},
                    "blocked": true,
                }).to_string());
            }
        }
        let jar = gs.cookie_jar.clone();
        let in_flight = gs.http_client.as_ref().map(|c| c.in_flight.clone());
        tracing::debug!("op_fetch_url: intercept_enabled={}, has_tx={}", gs.intercept_enabled, gs.intercept_tx.is_some());
        let itx = if gs.intercept_enabled {
            gs.intercept_counter += 1;
            gs.intercept_tx.clone().map(|tx| (tx, format!("intercept-{}", gs.intercept_counter)))
        } else {
            None
        };
        (jar, in_flight, itx)
    };

    if let Some((tx, request_id)) = intercept_tx {
        let custom_headers: HashMap<String, String> = serde_json::from_str(&headers_json).unwrap_or_default();
        let (resolve_tx, resolve_rx) = tokio::sync::oneshot::channel();
        let intercepted = InterceptedRequest {
            request_id: request_id.clone(),
            url: url.clone(),
            method: method.clone(),
            headers: custom_headers.clone(),
            resource_type: "Fetch".to_string(),
            resolver: resolve_tx,
        };
        if tx.send(intercepted).is_ok() {
            match resolve_rx.await {
                Ok(InterceptResolution::Fulfill { status, headers: h, body: b }) => {
                    let resp_headers: HashMap<String, String> = h;
                    return Ok(serde_json::json!({
                        "status": status,
                        "body": b,
                        "url": url,
                        "headers": resp_headers,
                    }).to_string());
                }
                Ok(InterceptResolution::Fail { reason }) => {
                    return Ok(serde_json::json!({
                        "status": 0,
                        "body": "",
                        "url": url,
                        "headers": {},
                        "blocked": true,
                        "error": reason,
                    }).to_string());
                }
                Ok(InterceptResolution::Continue { url: _new_url, method: _new_method, headers: _new_headers, body: _new_body }) => {
                    tracing::debug!("Interception: continue request {}", url);
                }
                Err(_) => {
                }
            }
        }
    }

    let client = get_shared_client();

    let request_origin = url::Url::parse(&url)
        .ok()
        .map(|u| {
            let host = u.host_str().unwrap_or("");
            match u.port() {
                Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
                None => format!("{}://{}", u.scheme(), host),
            }
        })
        .unwrap_or_default();
    let page_origin = if origin.is_empty() { request_origin.clone() } else { origin.clone() };
    let is_cross_origin = !page_origin.is_empty() && request_origin != page_origin;

    let req_method: reqwest::Method = method.parse().unwrap_or(reqwest::Method::GET);

    let custom_headers: std::collections::HashMap<String, String> =
        serde_json::from_str(&headers_json).unwrap_or_default();

    let needs_preflight = is_cross_origin
        && mode == "cors"
        && (req_method != reqwest::Method::GET
            && req_method != reqwest::Method::HEAD
            && req_method != reqwest::Method::POST
            || custom_headers.keys().any(|k| {
                let kl = k.to_lowercase();
                kl != "accept" && kl != "accept-language" && kl != "content-language"
                    && kl != "content-type"
            }));

    if needs_preflight {
        let preflight = client
            .request(reqwest::Method::OPTIONS, &url)
            .header("Origin", &page_origin)
            .header("Access-Control-Request-Method", method.as_str())
            .header(
                "Access-Control-Request-Headers",
                custom_headers.keys().cloned().collect::<Vec<_>>().join(", "),
            )
            .send()
            .await
            .map_err(|e| deno_error::JsErrorBox::generic(format!("CORS preflight failed: {}", e)))?;

        let allowed_origin = preflight
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if allowed_origin != "*" && allowed_origin != page_origin {
            return Err(deno_error::JsErrorBox::generic(format!(
                "CORS preflight: Origin '{}' not allowed by Access-Control-Allow-Origin '{}'",
                page_origin, allowed_origin
            )));
        }
    }

    let mut req = client.request(req_method, &url);

    if is_cross_origin {
        req = req.header("Origin", &page_origin);
    }

    if !is_cross_origin {
        if let Some(ref jar) = cookie_jar {
            if let Ok(parsed_url) = url::Url::parse(&url) {
                let cookie_header = jar.get_cookie_header(&parsed_url);
                if !cookie_header.is_empty() {
                    req = req.header("Cookie", &cookie_header);
                }
            }
        }
    }

    for (k, v) in &custom_headers {
        req = req.header(k.as_str(), v.as_str());
    }

    if !body.is_empty() {
        req = req.body(body);
    }

    if let Some(ref counter) = in_flight {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    let response = req
        .send()
        .await
        .map_err(|e| {
            if let Some(ref counter) = in_flight {
                counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            deno_error::JsErrorBox::generic(e.to_string())
        })?;

    if let Some(ref counter) = in_flight {
        counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }

    let status = response.status().as_u16();

    if let Some(ref jar) = cookie_jar {
        if let Ok(parsed_url) = url::Url::parse(&url) {
            for val in response.headers().get_all(reqwest::header::SET_COOKIE) {
                if let Ok(s) = val.to_str() {
                    jar.set_cookie(s, &parsed_url);
                }
            }
        }
    }

    let resp_headers: std::collections::HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    if is_cross_origin && mode == "cors" {
        let allowed = resp_headers
            .get("access-control-allow-origin")
            .map(|s| s.as_str())
            .unwrap_or("");

        if allowed != "*" && allowed != page_origin {
            return Ok(serde_json::json!({
                "status": 0,
                "body": "",
                "url": url,
                "headers": {},
                "corsBlocked": true,
                "corsError": format!("CORS error: Origin '{}' not in Access-Control-Allow-Origin '{}'", page_origin, allowed),
            })
            .to_string());
        }
    }

    let resp_bytes = response
        .bytes()
        .await
        .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?;
    let resp_body = String::from_utf8_lossy(&resp_bytes).to_string();
    let resp_body_base64 = BASE64.encode(&resp_bytes);

    tracing::debug!("op_fetch_url completed: {} {} ({} bytes)", method, url, resp_body.len());

    Ok(serde_json::json!({
        "status": status,
        "body": resp_body,
        "bodyBase64": resp_body_base64,
        "url": url,
        "headers": resp_headers,
    })
    .to_string())
}

fn glob_match(pattern: &str, url: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.starts_with('*') && pattern.ends_with('*') {
        return url.contains(&pattern[1..pattern.len() - 1]);
    }
    if pattern.starts_with('*') {
        return url.ends_with(&pattern[1..]);
    }
    if pattern.ends_with('*') {
        return url.starts_with(&pattern[..pattern.len() - 1]);
    }
    url == pattern
}

fn validate_fetch_url(url: &url::Url) -> Result<(), String> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" && scheme != "file" {
        return Err(format!(
            "Forbidden URL scheme '{}' - only http, https, and file are allowed",
            scheme
        ));
    }

    if scheme == "file" {
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
                    return Err(format!(
                        "Access to private/internal IP address {} is not allowed",
                        ip
                    ));
                }
            }
            url::Host::Ipv6(ip) => {
                if ip.is_loopback() || ip.is_unicast_link_local() {
                    return Err(format!(
                        "Access to private/internal IPv6 address {} is not allowed",
                        ip
                    ));
                }
            }
            url::Host::Domain(domain) => {
                let lower_domain = domain.to_lowercase();
                if lower_domain == "localhost"
                    || lower_domain.ends_with(".localhost")
                    || lower_domain == "127.0.0.1"
                    || lower_domain == "::1"
                {
                    return Err(format!(
                        "Access to localhost domain '{}' is not allowed",
                        domain
                    ));
                }
            }
        }
    }

    Ok(())
}

#[op2]
#[string]
fn op_get_cookies(state: &OpState) -> String {
    let gs = state.borrow::<SharedState>().clone();
    let gs = gs.borrow();
    let jar = match &gs.cookie_jar {
        Some(j) => j,
        None => return String::new(),
    };
    let url = match url::Url::parse(&gs.url) {
        Ok(u) => u,
        Err(_) => return String::new(),
    };
    jar.get_js_visible_cookies(&url)
}

#[op2(fast)]
fn op_set_cookie(state: &OpState, #[string] cookie_str: &str) {
    let gs = state.borrow::<SharedState>().clone();
    let gs = gs.borrow();
    let jar = match &gs.cookie_jar {
        Some(j) => j,
        None => return,
    };
    let url = match url::Url::parse(&gs.url) {
        Ok(u) => u,
        Err(_) => return,
    };
    jar.set_cookie_from_js(cookie_str, &url);
}

#[op2(fast)]
fn op_navigate(state: &OpState, #[string] url: &str, #[string] method: &str, #[string] body: &str) {
    let gs = state.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();
    gs.pending_navigation = Some((url.to_string(), method.to_string(), body.to_string()));
}

pub fn build_extension() -> Extension {
    Extension {
        name: "obscura_dom",
        ops: std::borrow::Cow::Owned(vec![
            op_dom(),
            op_console_msg(),
            op_fetch_url(),
            op_get_cookies(),
            op_set_cookie(),
            op_navigate(),
        ]),
        ..Default::default()
    }
}
