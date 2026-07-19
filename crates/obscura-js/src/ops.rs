use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use deno_core::op2;
use deno_core::Extension;
use deno_core::JsBuffer;
use deno_core::OpState;
use obscura_dom::{DomTree, NodeData, NodeId};
use obscura_net::{CookieJar, ObscuraHttpClient, ResourceType};
use reqwest::Method;
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
        body: Option<Vec<u8>>,
    },
    Fulfill {
        status: u16,
        headers: HashMap<String, String>,
        body: Vec<u8>,
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
    pub body: Vec<u8>,
    pub resource_type: String,
    pub pause: bool,
    pub resolver: tokio::sync::oneshot::Sender<InterceptResolution>,
    pub response_rx: tokio::sync::oneshot::Receiver<InterceptedResponse>,
}

pub struct InterceptedResponse {
    pub url: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub encoded_data_length: usize,
}

const FETCH_INTERCEPT_RESOLUTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    pub intercept_patterns: Vec<String>,
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
            intercept_patterns: Vec::new(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialsMode {
    Include,
    SameOrigin,
    Omit,
}

impl CredentialsMode {
    fn parse(value: &str) -> Result<Self, deno_error::JsErrorBox> {
        match value {
            "include" => Ok(Self::Include),
            "same-origin" => Ok(Self::SameOrigin),
            "omit" => Ok(Self::Omit),
            _ => Err(deno_error::JsErrorBox::type_error(format!(
                "Invalid credentials mode: {}",
                value
            ))),
        }
    }

    fn allows(self, request_url: &url::Url, page_origin: Option<&url::Url>) -> bool {
        match self {
            Self::Include => true,
            Self::Omit => false,
            Self::SameOrigin => page_origin
                .map(|page| page.origin() == request_url.origin())
                .unwrap_or(false),
        }
    }
}

#[op2(async)]
#[string]
async fn op_fetch_url(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[string] method: String,
    #[string] headers_json: String,
    #[buffer] body: JsBuffer,
    #[string] origin: String,
    #[string] mode: String,
    #[string] credentials: String,
) -> Result<String, deno_error::JsErrorBox> {
    tracing::debug!(
        "op_fetch_url called: {} {} (intercept check pending)",
        method,
        url
    );
    let credentials = CredentialsMode::parse(&credentials)?;
    let initial_method: Method = method.parse().map_err(|error| {
        deno_error::JsErrorBox::type_error(format!("Invalid HTTP method: {}", error))
    })?;
    let mut initial_author_headers =
        normalize_header_names(serde_json::from_str(&headers_json).unwrap_or_default());
    strip_browser_managed_headers(&mut initial_author_headers);
    validate_no_cors_request(&mode, &initial_method, &initial_author_headers)?;

    if let Some((body, content_type)) = decode_data_url(&url) {
        return Ok(fetch_response_json(
            200,
            &url,
            HashMap::from([("content-type".to_string(), content_type)]),
            &body,
            false,
        ));
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

    let (cookie_jar, intercept_tx, http_client, page_url, page_id) = {
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
        let http_client = gs.http_client.clone();
        let page_url = gs.url.clone();
        let page_id = gs.page_id.clone();
        tracing::debug!(
            "op_fetch_url: intercept_enabled={}, has_tx={}",
            gs.intercept_enabled,
            gs.intercept_tx.is_some()
        );
        let should_pause = gs.intercept_enabled
            && intercept_patterns_should_pause_url(&gs.intercept_patterns, &url);
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
        (jar, itx, http_client, page_url, page_id)
    };

    let page_origin_url = url::Url::parse(&origin)
        .ok()
        .or_else(|| url::Url::parse(&page_url).ok());
    let mut request_url = url;
    let mut request_method = method;
    let mut request_body = body.to_vec();
    let mut custom_headers = initial_author_headers;
    if let (Ok(parsed_url), Ok(parsed_method)) = (
        url::Url::parse(&request_url),
        request_method.parse::<Method>(),
    ) {
        validate_no_cors_request(&mode, &parsed_method, &custom_headers)?;
        apply_browser_fetch_headers(
            &mut custom_headers,
            &parsed_url,
            &page_url,
            page_origin_url.as_ref(),
            &parsed_method,
            &mode,
            credentials,
            cookie_jar.as_ref(),
            http_client.as_ref(),
        )
        .await;
    }

    let mut response_tx = None;
    if let Some((tx, request_id, should_pause)) = intercept_tx {
        let (resolve_tx, mut resolve_rx) = tokio::sync::oneshot::channel();
        let (completion_tx, response_rx) = tokio::sync::oneshot::channel();
        let intercepted = InterceptedRequest {
            page_id: page_id.clone(),
            page_url: page_url.clone(),
            request_id: request_id.clone(),
            url: request_url.clone(),
            method: request_method.clone(),
            headers: custom_headers.clone(),
            body: request_body.clone(),
            resource_type: "Fetch".to_string(),
            pause: should_pause,
            resolver: resolve_tx,
            response_rx,
        };
        if tx.send(intercepted).is_ok() {
            response_tx = Some(completion_tx);
            let resolution = if should_pause {
                match tokio::time::timeout(FETCH_INTERCEPT_RESOLUTION_TIMEOUT, resolve_rx).await {
                    Ok(Ok(resolution)) => Some(resolution),
                    Ok(Err(_)) => None,
                    Err(_) => {
                        tracing::warn!(
                            "Interception timed out waiting for Fetch resolution {}; continuing original request",
                            request_url
                        );
                        None
                    }
                }
            } else {
                match resolve_rx.try_recv() {
                    Ok(resolution) => Some(resolution),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                        tracing::debug!(
                            "Interception event emitted; continuing request without blocking {}",
                            request_url
                        );
                        None
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => None,
                }
            };

            match resolution {
                Some(InterceptResolution::Continue {
                    url,
                    method,
                    headers,
                    body,
                }) => {
                    if let Some(url) = url {
                        request_url = url;
                    }
                    if let Some(method) = method {
                        request_method = method;
                    }
                    if let Some(headers) = headers {
                        custom_headers = normalize_header_names(headers);
                    }
                    if let Some(body) = body {
                        request_body = body;
                    }
                    tracing::debug!("Interception: continue request {}", request_url);
                }
                Some(InterceptResolution::Fail { reason }) => {
                    return Ok(serde_json::json!({
                        "status": 0,
                        "body": "",
                        "url": request_url,
                        "headers": {},
                        "blocked": true,
                        "error": reason,
                    })
                    .to_string());
                }
                Some(InterceptResolution::Fulfill {
                    status,
                    headers: resp_headers,
                    body,
                }) => {
                    if let Some(tx) = response_tx.take() {
                        let _ = tx.send(InterceptedResponse {
                            url: request_url.clone(),
                            status,
                            headers: resp_headers.clone(),
                            encoded_data_length: body.len(),
                            body: body.clone(),
                        });
                    }
                    return Ok(fetch_response_json(
                        status,
                        &request_url,
                        resp_headers,
                        &body,
                        false,
                    ));
                }
                None => {}
            }
        }
    }

    strip_browser_managed_headers(&mut custom_headers);
    let http_client = http_client.unwrap_or_else(|| Arc::new(ObscuraHttpClient::new()));
    let mut current_url = url::Url::parse(&request_url)
        .map_err(|error| deno_error::JsErrorBox::type_error(error.to_string()))?;
    let mut current_method: Method = request_method.parse().map_err(|error| {
        deno_error::JsErrorBox::type_error(format!("Invalid HTTP method: {}", error))
    })?;
    validate_no_cors_request(&mode, &current_method, &custom_headers)?;
    let mut redirected = false;
    let mut final_response = None;

    for _ in 0..20 {
        validate_fetch_url(&current_url).map_err(deno_error::JsErrorBox::generic)?;
        let is_cross_origin = page_origin_url
            .as_ref()
            .map(|page| page.origin() != current_url.origin())
            .unwrap_or(false);
        let mut headers = custom_headers.clone();
        apply_browser_fetch_headers(
            &mut headers,
            &current_url,
            &page_url,
            page_origin_url.as_ref(),
            &current_method,
            &mode,
            credentials,
            cookie_jar.as_ref(),
            Some(&http_client),
        )
        .await;

        let needs_preflight = is_cross_origin
            && mode == "cors"
            && (current_method != Method::GET
                && current_method != Method::HEAD
                && current_method != Method::POST
                || custom_headers
                    .iter()
                    .any(|(name, value)| !is_cors_safelisted_or_browser_header(name, value)));
        if needs_preflight {
            let mut preflight_headers = HashMap::new();
            if let Some(page) = page_origin_url.as_ref() {
                preflight_headers.insert("origin".to_string(), page.origin().ascii_serialization());
            }
            preflight_headers.insert(
                "access-control-request-method".to_string(),
                current_method.as_str().to_string(),
            );
            preflight_headers.insert(
                "access-control-request-headers".to_string(),
                custom_headers
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            let preflight = http_client
                .request_bytes_once(
                    Method::OPTIONS,
                    &current_url,
                    preflight_headers,
                    None,
                    ResourceType::Fetch,
                )
                .await
                .map_err(|error| deno_error::JsErrorBox::generic(error.to_string()))?;
            if !(200..300).contains(&preflight.status) {
                return Err(deno_error::JsErrorBox::generic(format!(
                    "CORS preflight failed with HTTP {}",
                    preflight.status
                )));
            }
            validate_cors_response(
                &preflight.headers,
                page_origin_url.as_ref(),
                credentials,
                "CORS preflight",
            )?;
            validate_preflight_permissions(
                &preflight.headers,
                &current_method,
                custom_headers.iter(),
                credentials,
            )?;
        }

        let response = http_client
            .request_bytes_once(
                current_method.clone(),
                &current_url,
                headers,
                Some(request_body.clone()),
                ResourceType::Fetch,
            )
            .await
            .map_err(|error| deno_error::JsErrorBox::generic(error.to_string()))?;

        if credentials.allows(&current_url, page_origin_url.as_ref()) {
            if let Some(jar) = cookie_jar.as_ref() {
                for set_cookie in &response.set_cookie_headers {
                    jar.set_cookie(set_cookie, &current_url);
                }
            }
        }
        if is_cross_origin && mode == "cors" {
            validate_cors_response(
                &response.headers,
                page_origin_url.as_ref(),
                credentials,
                "CORS response",
            )?;
        }

        if let Some(location) = response.header("location") {
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                let next_url = current_url.join(location).map_err(|error| {
                    deno_error::JsErrorBox::generic(format!("Invalid redirect URL: {}", error))
                })?;
                validate_fetch_redirect(&current_url, &next_url)
                    .map_err(deno_error::JsErrorBox::generic)?;
                if current_url.origin() != next_url.origin() {
                    strip_cross_origin_sensitive_headers(&mut custom_headers);
                }
                if apply_fetch_redirect(response.status, &mut current_method, &mut request_body) {
                    custom_headers.retain(|name, _| {
                        !matches!(
                            name.to_ascii_lowercase().as_str(),
                            "content-type" | "content-encoding" | "content-language"
                        )
                    });
                }
                current_url = next_url;
                redirected = true;
                continue;
            }
        }

        final_response = Some(response);
        break;
    }

    let response = final_response.ok_or_else(|| {
        deno_error::JsErrorBox::generic(format!("Too many redirects: {}", current_url))
    })?;
    let status = response.status;
    let resp_headers = response.headers;
    let resp_bytes = response.body;
    request_url = current_url.to_string();

    if page_origin_url
        .as_ref()
        .map(|page| page.origin() != current_url.origin())
        .unwrap_or(false)
        && mode == "cors"
    {
        if let Err(error) = validate_cors_response(
            &resp_headers,
            page_origin_url.as_ref(),
            credentials,
            "CORS response",
        ) {
            return Ok(serde_json::json!({
                "status": 0,
                "url": request_url,
                "headers": {},
                "corsBlocked": true,
                "corsError": error.to_string(),
            })
            .to_string());
        }
    }

    if let Some(tx) = response_tx {
        let _ = tx.send(InterceptedResponse {
            url: request_url.clone(),
            status,
            headers: resp_headers.clone(),
            encoded_data_length: resp_bytes.len(),
            body: resp_bytes.clone(),
        });
    }

    tracing::debug!(
        "op_fetch_url completed: {} {} ({} bytes)",
        request_method,
        request_url,
        resp_bytes.len()
    );

    Ok(fetch_response_json(
        status,
        &request_url,
        resp_headers,
        &resp_bytes,
        redirected,
    ))
}

async fn apply_browser_fetch_headers(
    headers: &mut HashMap<String, String>,
    request_url: &url::Url,
    page_url: &str,
    page_origin: Option<&url::Url>,
    method: &Method,
    mode: &str,
    credentials: CredentialsMode,
    cookie_jar: Option<&Arc<CookieJar>>,
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
    for (name, value) in &extra_headers {
        if !is_browser_managed_header(name)
            && !headers
                .keys()
                .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            headers.insert(name.to_ascii_lowercase(), value.clone());
        }
    }
    let accept_language = header_value_case_insensitive(&extra_headers, "accept-language")
        .unwrap_or_else(|| "en-US,en;q=0.9".to_string());
    let sec_ch_platform = header_value_case_insensitive(&extra_headers, "sec-ch-ua-platform")
        .unwrap_or_else(|| obscura_net::DEFAULT_SEC_CH_UA_PLATFORM.to_string());
    let sec_ch_platform_version =
        header_value_case_insensitive(&extra_headers, "sec-ch-ua-platform-version")
            .unwrap_or_else(|| obscura_net::DEFAULT_SEC_CH_UA_PLATFORM_VERSION.to_string());

    headers.insert("user-agent".to_string(), ua);
    headers
        .entry("accept".to_string())
        .or_insert_with(|| "*/*".to_string());
    headers.insert("accept-language".to_string(), accept_language);
    headers.insert(
        "sec-ch-ua".to_string(),
        obscura_net::DEFAULT_SEC_CH_UA.to_string(),
    );
    headers.insert(
        "sec-ch-ua-full-version-list".to_string(),
        obscura_net::DEFAULT_SEC_CH_UA_FULL_VERSION_LIST.to_string(),
    );
    headers.insert("sec-ch-ua-model".to_string(), "\"\"".to_string());
    headers.insert("sec-ch-ua-mobile".to_string(), "?0".to_string());
    headers.insert("sec-ch-ua-platform".to_string(), sec_ch_platform);
    headers.insert(
        "sec-ch-ua-platform-version".to_string(),
        sec_ch_platform_version,
    );
    headers.insert(
        "sec-ch-prefers-color-scheme".to_string(),
        "dark".to_string(),
    );
    headers.insert("sec-fetch-dest".to_string(), "empty".to_string());
    headers.insert("sec-fetch-mode".to_string(), mode.to_string());

    let fetch_site = match page_origin {
        Some(page) if page.origin() == request_url.origin() => "same-origin",
        Some(page) if obscura_net::is_schemeful_same_site(page, request_url) => "same-site",
        Some(_) => "cross-site",
        None => "none",
    };
    headers.insert("sec-fetch-site".to_string(), fetch_site.to_string());

    if let Ok(mut referer) = url::Url::parse(page_url) {
        if matches!(referer.scheme(), "http" | "https") {
            referer.set_fragment(None);
            headers.insert("referer".to_string(), referer.to_string());
        }
    }
    if let Some(page) = page_origin {
        if *method != Method::GET && *method != Method::HEAD
            || page.origin() != request_url.origin()
        {
            headers.insert("origin".to_string(), page.origin().ascii_serialization());
        }
    }
    if credentials.allows(request_url, page_origin) {
        if let Some(jar) = cookie_jar {
            let cookie =
                jar.get_cookie_header_for_request(request_url, page_origin, false, method.as_str());
            if !cookie.is_empty() {
                headers.insert("cookie".to_string(), cookie);
            }
        }
    }
}

fn header_value_case_insensitive(headers: &HashMap<String, String>, key: &str) -> Option<String> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.clone())
}

fn normalize_header_names(headers: HashMap<String, String>) -> HashMap<String, String> {
    headers
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect()
}

fn strip_browser_managed_headers(headers: &mut HashMap<String, String>) {
    headers.retain(|name, _| !is_browser_managed_header(name));
}

fn is_browser_managed_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
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
    ) || name.starts_with("sec-")
        || name.starts_with("proxy-")
}

fn strip_cross_origin_sensitive_headers(headers: &mut HashMap<String, String>) {
    headers.remove("authorization");
    headers.remove("proxy-authorization");
}

fn apply_fetch_redirect(status: u16, method: &mut Method, body: &mut Vec<u8>) -> bool {
    let switch_to_get = status == 303 && *method != Method::HEAD
        || matches!(status, 301 | 302) && *method == Method::POST;
    if switch_to_get {
        *method = Method::GET;
        body.clear();
    }
    switch_to_get
}

fn validate_cors_response(
    headers: &HashMap<String, String>,
    page_origin: Option<&url::Url>,
    credentials: CredentialsMode,
    context: &str,
) -> Result<(), deno_error::JsErrorBox> {
    let Some(page_origin) = page_origin else {
        return Ok(());
    };
    let expected = page_origin.origin().ascii_serialization();
    let allowed =
        header_value_case_insensitive(headers, "access-control-allow-origin").unwrap_or_default();
    let origin_allowed =
        allowed == expected || allowed == "*" && credentials != CredentialsMode::Include;
    if !origin_allowed {
        return Err(deno_error::JsErrorBox::generic(format!(
            "{}: Origin '{}' not allowed by Access-Control-Allow-Origin '{}'",
            context, expected, allowed
        )));
    }
    if credentials == CredentialsMode::Include
        && !header_value_case_insensitive(headers, "access-control-allow-credentials")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    {
        return Err(deno_error::JsErrorBox::generic(format!(
            "{}: credentialed response did not allow credentials",
            context
        )));
    }
    Ok(())
}

fn validate_preflight_permissions<'a>(
    headers: &HashMap<String, String>,
    method: &Method,
    requested_headers: impl Iterator<Item = (&'a String, &'a String)>,
    credentials: CredentialsMode,
) -> Result<(), deno_error::JsErrorBox> {
    let allowed_methods =
        header_value_case_insensitive(headers, "access-control-allow-methods").unwrap_or_default();
    let wildcard_allowed = credentials != CredentialsMode::Include;
    if !allowed_methods.split(',').map(str::trim).any(|allowed| {
        allowed.eq_ignore_ascii_case(method.as_str()) || wildcard_allowed && allowed == "*"
    }) {
        return Err(deno_error::JsErrorBox::generic(format!(
            "CORS preflight did not allow method {}",
            method
        )));
    }
    let allowed_headers =
        header_value_case_insensitive(headers, "access-control-allow-headers").unwrap_or_default();
    let allowed_headers = allowed_headers
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    for (requested, value) in requested_headers {
        if is_cors_safelisted_or_browser_header(requested, value) {
            continue;
        }
        let wildcard_matches = wildcard_allowed
            && !requested.eq_ignore_ascii_case("authorization")
            && allowed_headers.iter().any(|allowed| allowed == "*");
        if !wildcard_matches
            && !allowed_headers
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(requested))
        {
            return Err(deno_error::JsErrorBox::generic(format!(
                "CORS preflight did not allow header {}",
                requested
            )));
        }
    }
    Ok(())
}

fn fetch_response_json(
    status: u16,
    url: &str,
    headers: HashMap<String, String>,
    body: &[u8],
    redirected: bool,
) -> String {
    let mut response = serde_json::json!({
        "status": status,
        "bodyBase64": BASE64.encode(body),
        "bodyByteLength": body.len(),
        "url": url,
        "headers": headers,
        "redirected": redirected,
    });
    if let Ok(text) = std::str::from_utf8(body) {
        response["body"] = serde_json::Value::String(text.to_string());
    }
    response.to_string()
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

fn validate_no_cors_request(
    mode: &str,
    method: &Method,
    headers: &HashMap<String, String>,
) -> Result<(), deno_error::JsErrorBox> {
    if mode != "no-cors" {
        return Ok(());
    }
    if !matches!(*method, Method::GET | Method::HEAD | Method::POST) {
        return Err(deno_error::JsErrorBox::type_error(format!(
            "Request method {} is not allowed in no-cors mode",
            method
        )));
    }
    if let Some((name, _)) = headers
        .iter()
        .find(|(name, value)| !is_cors_safelisted_or_browser_header(name, value))
    {
        return Err(deno_error::JsErrorBox::type_error(format!(
            "Request header {} is not allowed in no-cors mode",
            name
        )));
    }
    Ok(())
}

fn is_cors_safelisted_or_browser_header(key: &str, value: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "accept" | "accept-language" | "content-language"
    ) {
        return true;
    }
    if lower == "content-type" {
        let mime = value.split(';').next().unwrap_or("").trim();
        return matches!(
            mime.to_ascii_lowercase().as_str(),
            "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
        );
    }
    lower == "user-agent"
        || lower == "referer"
        || lower == "origin"
        || lower.starts_with("sec-fetch-")
        || lower.starts_with("sec-ch-")
}

fn glob_match(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }

    wildcard_match(pattern, url)
}

fn intercept_patterns_should_pause_url(patterns: &[String], url: &str) -> bool {
    patterns.is_empty() || patterns.iter().any(|pattern| glob_match(pattern, url))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None;
    let mut star_text = 0usize;

    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

fn validate_fetch_redirect(current: &url::Url, next: &url::Url) -> Result<(), String> {
    validate_fetch_url(next)?;
    if matches!(current.scheme(), "http" | "https") && !matches!(next.scheme(), "http" | "https") {
        return Err(format!(
            "HTTP redirect to forbidden scheme '{}'",
            next.scheme()
        ));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercept_pause_patterns_support_playwright_wildcards() {
        let patterns = vec!["**/graphql/query**".to_string(), "**/*.mp4**".to_string()];

        assert!(intercept_patterns_should_pause_url(
            &patterns,
            "https://www.instagram.com/graphql/query/?variables=abc"
        ));
        assert!(intercept_patterns_should_pause_url(
            &patterns,
            "https://scontent.example/video.mp4?bytestart=0&byteend=1000"
        ));
        assert!(!intercept_patterns_should_pause_url(
            &patterns,
            "https://www.instagram.com/ajax/bz?__a=1"
        ));
    }

    #[test]
    fn fetch_intercept_resolution_timeout_preserves_route_handler_window() {
        assert!(FETCH_INTERCEPT_RESOLUTION_TIMEOUT >= std::time::Duration::from_secs(30));
    }

    #[test]
    fn credentials_modes_use_exact_origin_equality() {
        let page = url::Url::parse("https://example.com/page").unwrap();
        let same = url::Url::parse("https://example.com:443/api").unwrap();
        let cross = url::Url::parse("https://api.example.com/api").unwrap();

        assert!(CredentialsMode::Include.allows(&cross, Some(&page)));
        assert!(CredentialsMode::SameOrigin.allows(&same, Some(&page)));
        assert!(!CredentialsMode::SameOrigin.allows(&cross, Some(&page)));
        assert!(!CredentialsMode::Omit.allows(&same, Some(&page)));
    }

    #[tokio::test]
    async fn browser_managed_headers_and_cookies_are_recomputed() {
        let page = url::Url::parse("https://www.example.com/page").unwrap();
        let request = url::Url::parse("https://api.example.com/upload").unwrap();
        let jar = Arc::new(CookieJar::new());
        jar.set_cookie("session=abc; Domain=example.com; Path=/; Secure", &request);
        let client = Arc::new(ObscuraHttpClient::new());
        let mut headers = HashMap::from([
            ("Cookie".to_string(), "attacker=1".to_string()),
            ("Content-Length".to_string(), "999".to_string()),
            ("Host".to_string(), "evil.example".to_string()),
            ("Origin".to_string(), "https://evil.example".to_string()),
            ("Referer".to_string(), "https://evil.example/".to_string()),
            ("Sec-Fetch-Site".to_string(), "none".to_string()),
            ("x-author".to_string(), "kept".to_string()),
        ]);
        strip_browser_managed_headers(&mut headers);
        apply_browser_fetch_headers(
            &mut headers,
            &request,
            page.as_str(),
            Some(&page),
            &Method::POST,
            "cors",
            CredentialsMode::Include,
            Some(&jar),
            Some(&client),
        )
        .await;

        assert_eq!(headers.get("x-author").map(String::as_str), Some("kept"));
        assert_eq!(
            headers.get("cookie").map(String::as_str),
            Some("session=abc")
        );
        assert_eq!(
            headers.get("origin").map(String::as_str),
            Some("https://www.example.com")
        );
        assert_eq!(
            headers.get("referer").map(String::as_str),
            Some("https://www.example.com/page")
        );
        assert_eq!(
            headers.get("sec-fetch-site").map(String::as_str),
            Some("same-site")
        );
        assert!(!headers.keys().any(|name| name.eq_ignore_ascii_case("host")));
        assert!(!headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("content-length")));

        strip_browser_managed_headers(&mut headers);
        apply_browser_fetch_headers(
            &mut headers,
            &request,
            page.as_str(),
            Some(&page),
            &Method::GET,
            "cors",
            CredentialsMode::SameOrigin,
            Some(&jar),
            Some(&client),
        )
        .await;
        assert!(!headers.contains_key("cookie"));
    }

    #[test]
    fn cors_safelist_checks_content_type_values_and_preflight_permissions() {
        assert!(is_cors_safelisted_or_browser_header(
            "content-type",
            "text/plain;charset=UTF-8"
        ));
        assert!(!is_cors_safelisted_or_browser_header(
            "content-type",
            "application/json"
        ));
        let allowed = HashMap::from([
            (
                "access-control-allow-methods".to_string(),
                "POST, PUT".to_string(),
            ),
            (
                "access-control-allow-headers".to_string(),
                "content-type, x-upload-id".to_string(),
            ),
        ]);
        let requested = HashMap::from([
            ("content-type".to_string(), "application/json".to_string()),
            ("x-upload-id".to_string(), "1".to_string()),
        ]);
        assert!(validate_preflight_permissions(
            &allowed,
            &Method::POST,
            requested.iter(),
            CredentialsMode::Omit,
        )
        .is_ok());
        assert!(validate_preflight_permissions(
            &allowed,
            &Method::DELETE,
            requested.iter(),
            CredentialsMode::Omit,
        )
        .is_err());

        let wildcard = HashMap::from([
            ("access-control-allow-methods".to_string(), "*".to_string()),
            ("access-control-allow-headers".to_string(), "*".to_string()),
        ]);
        let authorization =
            HashMap::from([("authorization".to_string(), "Bearer secret".to_string())]);
        assert!(validate_preflight_permissions(
            &wildcard,
            &Method::PUT,
            authorization.iter(),
            CredentialsMode::Omit,
        )
        .is_err());
        assert!(validate_preflight_permissions(
            &wildcard,
            &Method::PUT,
            requested.iter(),
            CredentialsMode::Include,
        )
        .is_err());
    }

    #[test]
    fn no_cors_rejects_non_simple_methods_and_headers() {
        assert!(validate_no_cors_request(
            "no-cors",
            &Method::POST,
            &HashMap::from([("content-type".to_string(), "text/plain".to_string())]),
        )
        .is_ok());
        assert!(validate_no_cors_request("no-cors", &Method::PUT, &HashMap::new(),).is_err());
        assert!(validate_no_cors_request(
            "no-cors",
            &Method::POST,
            &HashMap::from([("x-custom".to_string(), "1".to_string())]),
        )
        .is_err());
        assert!(validate_no_cors_request(
            "no-cors",
            &Method::POST,
            &HashMap::from([("content-type".to_string(), "application/json".to_string(),)]),
        )
        .is_err());
    }

    #[test]
    fn http_redirects_cannot_escape_to_local_schemes() {
        let current = url::Url::parse("https://example.com/start").unwrap();
        let file = url::Url::parse("file:///tmp/secret").unwrap();
        assert!(validate_fetch_redirect(&current, &file).is_err());
    }

    #[test]
    fn redirects_preserve_or_rewrite_method_and_bytes_like_fetch() {
        let mut headers = HashMap::from([
            ("authorization".to_string(), "Bearer secret".to_string()),
            ("x-safe".to_string(), "kept".to_string()),
        ]);
        strip_cross_origin_sensitive_headers(&mut headers);
        assert!(!headers.contains_key("authorization"));
        assert_eq!(headers.get("x-safe").map(String::as_str), Some("kept"));

        let original = vec![0, 255, 7];
        let mut method = Method::POST;
        let mut body = original.clone();
        assert!(!apply_fetch_redirect(307, &mut method, &mut body));
        assert_eq!(method, Method::POST);
        assert_eq!(body, original);

        assert!(apply_fetch_redirect(302, &mut method, &mut body));
        assert_eq!(method, Method::GET);
        assert!(body.is_empty());
    }
}
