use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use deno_core::op2;
use deno_core::Extension;
use deno_core::OpState;
use obscura_dom::{DomTree, NodeData, NodeId};
use obscura_net::{CookieJar, ObscuraHttpClient};
use tokio::sync::Mutex;

pub type InterceptCallback = Arc<
    Mutex<
        Option<Box<dyn Fn(String, String, String) -> Option<(u16, String, String)> + Send + Sync>>,
    >,
>;

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
    Fail {
        reason: String,
    },
}

pub struct InterceptedRequest {
    pub page_id: Option<String>,
    pub page_url: String,
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub resource_type: String,
    pub pause: bool,
    pub resolver: tokio::sync::oneshot::Sender<InterceptResolution>,
    pub response_rx: tokio::sync::oneshot::Receiver<InterceptedResponse>,
}

pub struct InterceptedResponse {
    pub url: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub base64_encoded: bool,
    pub encoded_data_length: usize,
}

pub struct ObscuraState {
    pub dom: Option<DomTree>,
    pub page_id: Option<String>,
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
            page_id: None,
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
fn op_dom(
    state: &OpState,
    #[string] cmd: String,
    #[string] arg1: String,
    #[string] arg2: String,
) -> String {
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
                    if n.as_element()
                        .map(|name| name.local.as_ref() == "html")
                        .unwrap_or(false)
                    {
                        return cid.index().to_string();
                    }
                }
            }
            "-1".into()
        }
        "document_doctype" => {
            for cid in dom.children(dom.document()) {
                if let Some(n) = dom.get_node(cid) {
                    if let obscura_dom::NodeData::Doctype {
                        name,
                        public_id,
                        system_id,
                    } = &n.data
                    {
                        return serde_json::json!({
                            "name": name,
                            "publicId": public_id,
                            "systemId": system_id,
                            "nodeId": cid.index(),
                        })
                        .to_string();
                    }
                }
            }
            "null".into()
        }
        "get_element_by_id" => dom
            .get_element_by_id(&arg1)
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into()),
        "query_selector" => dom
            .query_selector(&arg1)
            .ok()
            .flatten()
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into()),
        "query_selector_all" => {
            let ids: Vec<i32> = dom
                .query_selector_all(&arg1)
                .ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect())
                .unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "node_type" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.get_node(NodeId::new(nid))
                .map(|n| match &n.data {
                    NodeData::Document => "9",
                    NodeData::Element { .. } => "1",
                    NodeData::Text { .. } => "3",
                    NodeData::Comment { .. } => "8",
                    NodeData::Doctype { .. } => "10",
                    NodeData::ProcessingInstruction { .. } => "7",
                })
                .unwrap_or("0")
                .into()
        }
        "node_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name: String = dom
                .get_node(NodeId::new(nid))
                .map(|n| match &n.data {
                    NodeData::Document => "#document".to_string(),
                    NodeData::Element { name, .. } => name.local.as_ref().to_ascii_uppercase(),
                    NodeData::Text { .. } => "#text".to_string(),
                    NodeData::Comment { .. } => "#comment".to_string(),
                    NodeData::Doctype { name, .. } => name.clone(),
                    NodeData::ProcessingInstruction { target, .. } => target.clone(),
                })
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "text_content" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            serde_json::to_string(&dom.text_content(NodeId::new(nid))).unwrap_or("\"\"".into())
        }
        "parent_node" | "first_child" | "last_child" | "next_sibling" | "prev_sibling" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.get_node(NodeId::new(nid))
                .and_then(|n| match cmd.as_str() {
                    "parent_node" => n.parent,
                    "first_child" => n.first_child,
                    "last_child" => n.last_child,
                    "next_sibling" => n.next_sibling,
                    "prev_sibling" => n.prev_sibling,
                    _ => None,
                })
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        "child_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom
                .children(NodeId::new(nid))
                .iter()
                .map(|id| id.index() as i32)
                .collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "tag_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name = dom
                .get_node(NodeId::new(nid))
                .and_then(|n| {
                    n.as_element()
                        .map(|name| name.local.as_ref().to_ascii_uppercase())
                })
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "get_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .get_node(NodeId::new(nid))
                .and_then(|n| n.get_attribute(&arg2).map(|s| s.to_string()));
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "set_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            if let Some((name, value)) = arg2.split_once('\0') {
                if name == "id" {
                    let old_id = dom
                        .get_node(node_id)
                        .and_then(|n| n.get_attribute("id").map(|s| s.to_string()));
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
            dom.with_node_mut(NodeId::new(nid), |n| match &mut n.data {
                NodeData::Text { contents } => {
                    *contents = arg2.clone();
                }
                NodeData::Comment { contents } => {
                    *contents = arg2.clone();
                }
                _ => {}
            });
            "true".into()
        }
        "create_document_fragment" => dom.new_node(NodeData::Document).index().to_string(),
        "create_element" => dom
            .new_node(NodeData::Element {
                name: html5ever::QualName::new(
                    None,
                    html5ever::ns!(html),
                    html5ever::LocalName::from(arg1.as_str()),
                ),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            })
            .index()
            .to_string(),
        "create_text_node" => dom
            .new_node(NodeData::Text {
                contents: arg1.clone(),
            })
            .index()
            .to_string(),
        "create_comment_node" => dom
            .new_node(NodeData::Comment {
                contents: arg1.clone(),
            })
            .index()
            .to_string(),
        "element_children" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom
                .children(NodeId::new(nid))
                .iter()
                .filter(|&&id| dom.get_node(id).map(|n| n.is_element()).unwrap_or(false))
                .map(|id| id.index() as i32)
                .collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "has_child_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.get_node(NodeId::new(nid))
                .map(|n| n.first_child.is_some())
                .unwrap_or(false)
                .to_string()
        }
        "contains" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let other = arg2.parse::<u32>().unwrap_or(0);
            dom.descendants(NodeId::new(nid))
                .contains(&NodeId::new(other))
                .to_string()
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
    tracing::debug!(
        "op_fetch_url called: {} {} (intercept check pending)",
        method,
        url
    );

    if let Some((body, content_type)) = decode_data_url(&url) {
        let body_text = String::from_utf8_lossy(&body).to_string();
        return Ok(serde_json::json!({
            "status": 200,
            "body": body_text,
            "bodyBase64": BASE64.encode(&body),
            "url": url,
            "headers": {
                "content-type": content_type,
            },
        })
        .to_string());
    }

    if let Ok(parsed_url) = url::Url::parse(&url) {
        if let Err(e) = validate_fetch_url(&parsed_url) {
            return Ok(serde_json::json!({
                "status": 0,
                "body": "",
                "url": url,
                "headers": {},
                "blocked": true,
                "error": e,
            })
            .to_string());
        }
    }

    let (cookie_jar, in_flight, intercept_tx, http_client, page_url, page_id) = {
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
                })
                .to_string());
            }
        }
        let jar = gs.cookie_jar.clone();
        let in_flight = gs.http_client.as_ref().map(|c| c.in_flight.clone());
        let http_client = gs.http_client.clone();
        let page_url = gs.url.clone();
        let page_id = gs.page_id.clone();
        tracing::debug!(
            "op_fetch_url: intercept_enabled={}, has_tx={}",
            gs.intercept_enabled,
            gs.intercept_tx.is_some()
        );
        let should_pause = gs.intercept_enabled;
        let itx = if gs.intercept_tx.is_some() {
            gs.intercept_counter += 1;
            let request_prefix = page_id
                .as_deref()
                .filter(|id| !id.is_empty())
                .unwrap_or("intercept");
            gs.intercept_tx.clone().map(|tx| {
                (
                    tx,
                    format!("{}-intercept-{}", request_prefix, gs.intercept_counter),
                    should_pause,
                )
            })
        } else {
            None
        };
        (jar, in_flight, itx, http_client, page_url, page_id)
    };

    let mut custom_headers: HashMap<String, String> =
        serde_json::from_str(&headers_json).unwrap_or_default();
    apply_browser_fetch_headers(&mut custom_headers, &url, &page_url, http_client.as_ref()).await;

    let mut response_tx = None;
    if let Some((tx, request_id, should_pause)) = intercept_tx {
        let (resolve_tx, mut resolve_rx) = tokio::sync::oneshot::channel();
        let (completion_tx, response_rx) = tokio::sync::oneshot::channel();
        let intercepted = InterceptedRequest {
            page_id: page_id.clone(),
            page_url: page_url.clone(),
            request_id: request_id.clone(),
            url: url.clone(),
            method: method.clone(),
            headers: custom_headers.clone(),
            body: body.clone(),
            resource_type: "Fetch".to_string(),
            pause: should_pause,
            resolver: resolve_tx,
            response_rx,
        };
        if tx.send(intercepted).is_ok() {
            response_tx = Some(completion_tx);
            match resolve_rx.try_recv() {
                Ok(InterceptResolution::Fulfill {
                    status,
                    headers: h,
                    body: b,
                }) => {
                    let resp_headers: HashMap<String, String> = h;
                    return Ok(serde_json::json!({
                        "status": status,
                        "body": b,
                        "url": url,
                        "headers": resp_headers,
                    })
                    .to_string());
                }
                Ok(InterceptResolution::Fail { reason }) => {
                    return Ok(serde_json::json!({
                        "status": 0,
                        "body": "",
                        "url": url,
                        "headers": {},
                        "blocked": true,
                        "error": reason,
                    })
                    .to_string());
                }
                Ok(InterceptResolution::Continue { .. }) => {
                    tracing::debug!("Interception: continue request {}", url);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    tracing::debug!(
                        "Interception event emitted; continuing request without blocking {}",
                        url
                    );
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
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
    let page_origin = if origin.is_empty() {
        request_origin.clone()
    } else {
        origin.clone()
    };
    let is_cross_origin = !page_origin.is_empty() && request_origin != page_origin;

    let req_method: reqwest::Method = method.parse().unwrap_or(reqwest::Method::GET);

    let needs_preflight = is_cross_origin
        && mode == "cors"
        && (req_method != reqwest::Method::GET
            && req_method != reqwest::Method::HEAD
            && req_method != reqwest::Method::POST
            || custom_headers
                .keys()
                .any(|k| !is_cors_safelisted_or_browser_header(k)));

    if needs_preflight {
        let preflight = client
            .request(reqwest::Method::OPTIONS, &url)
            .header("Origin", &page_origin)
            .header("Access-Control-Request-Method", method.as_str())
            .header(
                "Access-Control-Request-Headers",
                custom_headers
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .send()
            .await
            .map_err(|e| {
                deno_error::JsErrorBox::generic(format!("CORS preflight failed: {}", e))
            })?;

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

    let response = req.send().await.map_err(|e| {
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

    if let Some(tx) = response_tx {
        let _ = tx.send(InterceptedResponse {
            url: url.clone(),
            status,
            headers: resp_headers.clone(),
            body: resp_body.clone(),
            base64_encoded: false,
            encoded_data_length: resp_bytes.len(),
        });
    }

    tracing::debug!(
        "op_fetch_url completed: {} {} ({} bytes)",
        method,
        url,
        resp_body.len()
    );

    Ok(serde_json::json!({
        "status": status,
        "body": resp_body,
        "bodyBase64": resp_body_base64,
        "url": url,
        "headers": resp_headers,
    })
    .to_string())
}

async fn apply_browser_fetch_headers(
    headers: &mut HashMap<String, String>,
    request_url: &str,
    page_url: &str,
    http_client: Option<&Arc<ObscuraHttpClient>>,
) {
    let (ua, extra_headers) = if let Some(client) = http_client {
        (
            client.user_agent.read().await.clone(),
            client.extra_headers.read().await.clone(),
        )
    } else {
        (obscura_net::DEFAULT_USER_AGENT.to_string(), HashMap::new())
    };
    let accept_language = header_value_case_insensitive(&extra_headers, "accept-language")
        .unwrap_or_else(|| "en-US,en;q=0.9".to_string());
    let sec_ch_platform = header_value_case_insensitive(&extra_headers, "sec-ch-ua-platform")
        .unwrap_or_else(|| obscura_net::DEFAULT_SEC_CH_UA_PLATFORM.to_string());
    let sec_ch_platform_version =
        header_value_case_insensitive(&extra_headers, "sec-ch-ua-platform-version")
            .unwrap_or_else(|| obscura_net::DEFAULT_SEC_CH_UA_PLATFORM_VERSION.to_string());

    insert_header_if_absent(headers, "user-agent", ua);
    insert_header_if_absent(headers, "accept", "*/*");
    insert_header_if_absent(headers, "accept-language", accept_language);
    insert_header_if_absent(headers, "sec-ch-ua", obscura_net::DEFAULT_SEC_CH_UA);
    insert_header_if_absent(
        headers,
        "sec-ch-ua-full-version-list",
        obscura_net::DEFAULT_SEC_CH_UA_FULL_VERSION_LIST,
    );
    insert_header_if_absent(headers, "sec-ch-ua-model", "\"\"");
    insert_header_if_absent(headers, "sec-ch-ua-mobile", "?0");
    insert_header_if_absent(headers, "sec-ch-ua-platform", sec_ch_platform);
    insert_header_if_absent(
        headers,
        "sec-ch-ua-platform-version",
        sec_ch_platform_version,
    );
    insert_header_if_absent(headers, "sec-ch-prefers-color-scheme", "dark");
    insert_header_if_absent(headers, "sec-fetch-dest", "empty");
    insert_header_if_absent(headers, "sec-fetch-mode", "cors");

    if page_url.starts_with("http://") || page_url.starts_with("https://") {
        insert_header_if_absent(headers, "referer", page_url);
        let fetch_site = match (url::Url::parse(request_url), url::Url::parse(page_url)) {
            (Ok(request), Ok(page)) if request.origin() == page.origin() => "same-origin",
            (Ok(request), Ok(page)) if request.domain() == page.domain() => "same-site",
            (Ok(_), Ok(_)) => "cross-site",
            _ => "none",
        };
        insert_header_if_absent(headers, "sec-fetch-site", fetch_site);
    }
}

fn header_value_case_insensitive(headers: &HashMap<String, String>, key: &str) -> Option<String> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.clone())
}

fn insert_header_if_absent(
    headers: &mut HashMap<String, String>,
    key: &str,
    value: impl Into<String>,
) {
    if !headers
        .keys()
        .any(|existing| existing.eq_ignore_ascii_case(key))
    {
        headers.insert(key.to_string(), value.into());
    }
}

fn decode_data_url(url: &str) -> Option<(Vec<u8>, String)> {
    if !url.starts_with("data:") {
        return None;
    }

    let comma = url.find(',')?;
    let (metadata, data) = url.split_at(comma);
    let data = &data[1..];
    let metadata_lower = metadata.to_ascii_lowercase();
    let content_type = metadata
        .strip_prefix("data:")
        .and_then(|m| m.split(';').next())
        .filter(|m| !m.is_empty())
        .unwrap_or("text/plain;charset=US-ASCII")
        .to_string();
    let bytes = if metadata_lower.contains(";base64") {
        BASE64.decode(data).ok()?
    } else {
        percent_decode(data)
    };

    Some((bytes, content_type))
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
        } else if bytes[i] == b'+' {
            decoded.push(b' ');
            i += 1;
            continue;
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

fn is_cors_safelisted_or_browser_header(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "accept"
        || lower == "accept-language"
        || lower == "content-language"
        || lower == "content-type"
        || lower == "user-agent"
        || lower == "referer"
        || lower == "origin"
        || lower.starts_with("sec-fetch-")
        || lower.starts_with("sec-ch-")
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

#[op2(async)]
async fn op_sleep(#[smi] delay_ms: i32) -> Result<(), deno_error::JsErrorBox> {
    let delay_ms = delay_ms.clamp(0, 60_000) as u64;
    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
    Ok(())
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
            op_sleep(),
        ]),
        ..Default::default()
    }
}
