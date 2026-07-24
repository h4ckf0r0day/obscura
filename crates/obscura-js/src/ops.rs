use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use deno_core::op2;
use deno_core::OpState;
use deno_core::Extension;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use obscura_dom::{DomTree, NodeData, NodeId};
use obscura_net::{CallbackRegistry, CookieJar, ObscuraHttpClient, RequestInfo, ResourceType, Response};
#[cfg(feature = "stealth")]
use obscura_net::StealthHttpClient;
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

#[derive(Debug, Clone)]
pub struct StoredNetworkResponseBody {
    pub body: String,
    pub base64_encoded: bool,
}

/// A network request made from page JS (fetch()/XHR/dynamic resource) recorded
/// so the CDP layer can emit Network.requestWillBeSent / responseReceived for
/// it. Static navigation subresources go through Page::record_network_event;
/// this is the parallel channel for script-initiated requests, which run in the
/// V8 op layer and would otherwise never surface as CDP Network events (#406).
#[derive(Debug, Clone)]
pub struct JsNetworkEvent {
    /// Matches the `fetch-{N}` id under which the body is stored, so CDP
    /// Network.getResponseBody resolves for the same request.
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub status: u16,
    pub response_headers: HashMap<String, String>,
    pub body_size: usize,
    pub timestamp: f64,
}

pub struct ObscuraState {
    pub dom: Option<DomTree>,
    pub url: String,
    /// WHATWG canonical name of the document's character encoding (e.g.
    /// "UTF-8", "EUC-JP"). Backs `document.characterSet` and the URL query
    /// encoding override for `<a>`/`<area>` hrefs in legacy-charset documents.
    pub encoding: String,
    pub title: String,
    pub blocked_urls: Vec<String>,
    pub cookie_jar: Option<Arc<CookieJar>>,
    pub http_client: Option<Arc<ObscuraHttpClient>>,
    /// The owning page's passive on_request/on_response callbacks (issue
    /// #408). Page-scoped, so scripted fetch()/XHR observation stays local to
    /// the page that registered it.
    pub callbacks: Option<Arc<CallbackRegistry>>,
    /// When set (stealth mode), scripted fetch()/XHR is routed through the wreq
    /// client so the request carries the Chrome TLS fingerprint and client
    /// hints instead of the rustls ClientHello op_fetch_url would otherwise send.
    #[cfg(feature = "stealth")]
    pub stealth_client: Option<Arc<StealthHttpClient>>,
    pub pending_navigation: Option<(String, String, String)>,
    pub intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<InterceptedRequest>>,
    pub intercept_counter: u64,
    pub intercept_enabled: bool,
    // Queue of (binding_name, payload) calls made by page JS via the
    // `op_binding_called` op. Drained by the CDP layer after each dispatch
    // and emitted as `Runtime.bindingCalled` events.
    pub pending_binding_calls: Vec<(String, String)>,
    pub network_response_bodies: HashMap<String, StoredNetworkResponseBody>,
    pub network_response_body_order: VecDeque<String>,
    pub network_response_body_counter: u64,
    // Absolute URLs requested via JS fetch() / XHR (op_fetch_url), in request
    // order. Surfaced by `--dump assets` so resources pulled in by script, not
    // just static DOM attributes, are listed (issue #301).
    pub fetched_urls: Vec<String>,
    // Network events for script-initiated requests (fetch/XHR/dynamic resource),
    // drained by the Page into its network_events so the CDP layer emits
    // Network.requestWillBeSent / responseReceived for them (issue #406).
    pub js_network_events: Vec<JsNetworkEvent>,
}

impl ObscuraState {
    pub fn new() -> Self {
        ObscuraState {
            dom: None,
            url: "about:blank".to_string(),
            encoding: "UTF-8".to_string(),
            title: String::new(),
            blocked_urls: Vec::new(),
            cookie_jar: None,
            http_client: None,
            callbacks: None,
            #[cfg(feature = "stealth")]
            stealth_client: None,
            pending_navigation: None,
            intercept_tx: None,
            intercept_counter: 0,
            intercept_enabled: false,
            pending_binding_calls: Vec::new(),
            network_response_bodies: HashMap::new(),
            network_response_body_order: VecDeque::new(),
            network_response_body_counter: 0,
            fetched_urls: Vec::new(),
            js_network_events: Vec::new(),
        }
    }
}

fn response_body_entry_limit() -> usize {
    std::env::var("OBSCURA_NETWORK_BODY_BUFFER_ENTRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

fn response_body_byte_limit() -> usize {
    std::env::var("OBSCURA_NETWORK_BODY_BUFFER_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2 * 1024 * 1024)
}

pub type SharedState = Rc<RefCell<ObscuraState>>;

#[op2]
#[string]
fn op_dom(state: &OpState, #[string] cmd: String, #[string] arg1: String, #[string] arg2: String) -> String {
    // Anti-panic boundary: a panic in a DOM op would unwind through deno_core
    // into V8's FFI frame, where V8_Fatal calls abort(3) and takes the whole
    // engine (and every CDP client) down. Catch it so one malformed selector or
    // inconsistent tree node degrades to a null result for that single call.
    // No per-call clone: on the happy path this is just a landing pad, so the
    // hot DOM path (querySelector/getAttribute/...) pays nothing measurable.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        op_dom_inner(state, cmd, arg1, arg2)
    }))
    .unwrap_or_else(|_| {
        tracing::error!("op_dom panicked; returning null");
        "null".to_string()
    })
}

fn op_dom_inner(state: &OpState, cmd: String, arg1: String, arg2: String) -> String {
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
        "document_encoding" => serde_json::to_string(&gs.encoding).unwrap_or("\"UTF-8\"".into()),
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
            // Verify the indexed node is in the live document. The id_index is best-effort:
            // it only registers nodes at creation time and doesn't update on reparent, so
            // it can point to a detached clone while the live node is elsewhere in the tree.
            let doc = dom.document();
            let nid = dom.get_element_by_id(&arg1);
            let live = nid.filter(|&n| dom.ancestors(n).contains(&doc));
            match live {
                Some(n) => n.index().to_string(),
                None => {
                    // Fall back to full scan for the live document.
                    let sel = format!("[id=\"{}\"]", arg1.replace('\\', "\\\\").replace('"', "\\\""));
                    dom.query_selector(&sel).ok().flatten()
                        .map(|id| id.index().to_string()).unwrap_or("-1".into())
                }
            }
        }
        "query_selector" => {
            dom.query_selector(&arg1).ok().flatten().map(|id| id.index().to_string()).unwrap_or("-1".into())
        }
        "query_selector_all" => {
            let ids: Vec<i32> = dom.query_selector_all(&arg1).ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect()).unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "query_selector_scoped" => {
            let root_nid = arg1.parse::<u32>().unwrap_or(0);
            dom.query_selector_from(NodeId::new(root_nid), &arg2).ok().flatten()
                .map(|id| id.index().to_string()).unwrap_or("-1".into())
        }
        "query_selector_all_scoped" => {
            let root_nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom.query_selector_all_from(NodeId::new(root_nid), &arg2).ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect()).unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "node_type" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node(NodeId::new(nid), |n| match &n.data {
                NodeData::Document => "9", NodeData::Element { .. } => "1", NodeData::Text { .. } => "3",
                NodeData::Comment { .. } => "8", NodeData::Doctype { .. } => "10", NodeData::ProcessingInstruction { .. } => "7",
            }).unwrap_or("0").into()
        }
        "node_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name: String = dom.with_node(NodeId::new(nid), |n| match &n.data {
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
            dom.with_node(NodeId::new(nid), |n| match cmd.as_str() {
                "parent_node" => n.parent, "first_child" => n.first_child,
                "last_child" => n.last_child, "next_sibling" => n.next_sibling,
                "prev_sibling" => n.prev_sibling, _ => None,
            }).flatten().map(|id| id.index().to_string()).unwrap_or("-1".into())
        }
        "next_in_subtree" => {
            let root = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let current = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            dom.next_in_subtree(root, current)
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        // Reverse document order within a subtree, for NodeIterator's backward
        // walk (which prunes nothing, so the whole step fits in the DOM layer).
        "prev_in_subtree" => {
            let root = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let current = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            dom.prev_in_subtree(root, current)
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        // Step past a whole subtree rather than into it: NodeFilter.FILTER_REJECT
        // prunes the rejected node's descendants, unlike FILTER_SKIP.
        "next_after_subtree" => {
            let root = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let current = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            dom.next_after_subtree(root, current)
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        "child_nodes" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom.children(NodeId::new(nid)).iter().map(|id| id.index() as i32).collect();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "tag_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name = dom.with_node(NodeId::new(nid), |n| n.as_element().map(|name| name.local.as_ref().to_ascii_uppercase())).flatten().unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "get_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom.with_node(NodeId::new(nid), |n| n.get_attribute(&arg2).map(|s| s.to_string())).flatten();
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "attribute_names" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let names: Vec<String> = dom
                .with_node(NodeId::new(nid), |n| {
                    n.attrs()
                        .map(|a| a.iter().map(|x| x.name.local.as_ref().to_string()).collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            serde_json::to_string(&names).unwrap_or("[]".into())
        }
        "set_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            if let Some((name, value)) = arg2.split_once('\0') {
                if name == "id" {
                    let old_id = dom.with_node(node_id, |n| n.get_attribute("id").map(|s| s.to_string())).flatten();
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
            // Reject if either nid failed to parse (was "undefined"/empty) — those
            // default to 0 which is the document root, and silently operating on it
            // corrupts the tree. Require both args to be valid positive integers.
            let parent = match arg1.parse::<u32>() { Ok(n) => n, Err(_) => return "false".into() };
            let child = match arg2.parse::<u32>() { Ok(n) => n, Err(_) => return "false".into() };
            dom.append_child(NodeId::new(parent), NodeId::new(child));
            "true".into()
        }
        "remove_child" => {
            let child = match arg1.parse::<u32>() { Ok(n) => n, Err(_) => return "false".into() };
            dom.remove_child(NodeId::new(child));
            "true".into()
        }
        "insert_before" => {
            let new_node = match arg1.parse::<u32>() { Ok(n) => n, Err(_) => return "false".into() };
            let ref_node = match arg2.parse::<u32>() { Ok(n) => n, Err(_) => return "false".into() };
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
            let nid = match arg1.parse::<u32>() {
                Ok(n) if n > 0 => n,
                // nid=0 is the document root; never allow innerHTML to clear it.
                // nid parse failure (e.g. "undefined") also falls here.
                _ => return "false".into(),
            };
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
                    NodeData::ProcessingInstruction { data, .. } => { *data = arg2.clone(); }
                    _ => {}
                }
            });
            "true".into()
        }
        // A <template>'s children live in a separate contents document, so this
        // is the only route to them from JS. Allocates one on demand for
        // templates built via createElement.
        "template_contents" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.template_contents(NodeId::new(nid))
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
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
        "create_processing_instruction" => {
            // arg1 = target, arg2 = data
            dom.new_node(NodeData::ProcessingInstruction {
                target: arg1.clone(),
                data: arg2.clone(),
            }).index().to_string()
        }
        "create_doctype" => {
            // arg1 = name, arg2 = public_id. system_id stored only in the
            // JS wrapper since neither current WPT test reads it back from
            // the underlying tree.
            dom.new_node(NodeData::Doctype {
                name: arg1.clone(),
                public_id: arg2.clone(),
                system_id: String::new(),
            }).index().to_string()
        }
        "pi_target" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom.with_node(NodeId::new(nid), |n| match &n.data {
                NodeData::ProcessingInstruction { target, .. } => Some(target.clone()),
                _ => None,
            }).flatten().unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
        "doctype_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom.with_node(NodeId::new(nid), |n| match &n.data {
                NodeData::Doctype { name, .. } => Some(name.clone()),
                _ => None,
            }).flatten().unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
        "doctype_public_id" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom.with_node(NodeId::new(nid), |n| match &n.data {
                NodeData::Doctype { public_id, .. } => Some(public_id.clone()),
                _ => None,
            }).flatten().unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
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
            dom.with_node(NodeId::new(nid), |n| n.first_child.is_some()).unwrap_or(false).to_string()
        }
        "contains" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let other = arg2.parse::<u32>().unwrap_or(0);
            dom.descendants(NodeId::new(nid)).contains(&NodeId::new(other)).to_string()
        }
        // Index of a node among its parent's children. Walks prev siblings in
        // Rust, avoiding the per-step JS->op round trips a Range comparison
        // would otherwise make.
        "node_index" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            node_child_index(dom, NodeId::new(nid)).to_string()
        }
        // Document (preorder) tree order of two nodes: -1 if a precedes b, 1 if
        // a follows b, 0 if equal. Used by the Range boundary-point algorithms.
        "compare_order" => {
            let a = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            let b = NodeId::new(arg2.parse::<u32>().unwrap_or(0));
            compare_node_order(dom, a, b).to_string()
        }
        // Root (topmost ancestor) of a node, in one op rather than an O(depth)
        // walk of parentNode ops from JS.
        "node_root" => {
            let mut cur = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            while let Some(p) = dom.with_node(cur, |x| x.parent).flatten() {
                cur = p;
            }
            cur.index().to_string()
        }
        _ => "null".into(),
    }
}

/// Index of `n` among its parent's children (0-based).
fn node_child_index(dom: &DomTree, n: NodeId) -> usize {
    let mut i = 0usize;
    let mut cur = dom.with_node(n, |x| x.prev_sibling).flatten();
    while let Some(p) = cur {
        i += 1;
        cur = dom.with_node(p, |x| x.prev_sibling).flatten();
    }
    i
}

/// Ancestor chain of `n` from the root down to `n` (root first).
fn node_ancestors_root_first(dom: &DomTree, n: NodeId) -> Vec<NodeId> {
    let mut v = vec![n];
    let mut cur = n;
    while let Some(p) = dom.with_node(cur, |x| x.parent).flatten() {
        v.push(p);
        cur = p;
    }
    v.reverse();
    v
}

/// Preorder (document) order comparison of two nodes: -1 before, 1 after, 0 same.
fn compare_node_order(dom: &DomTree, a: NodeId, b: NodeId) -> i32 {
    if a == b {
        return 0;
    }
    let aa = node_ancestors_root_first(dom, a);
    let bb = node_ancestors_root_first(dom, b);
    // Different roots: order is undefined per spec; keep it stable by node id.
    if aa[0] != bb[0] {
        return if a.index() < b.index() { -1 } else { 1 };
    }
    let mut i = 0usize;
    while i < aa.len() && i < bb.len() && aa[i] == bb[i] {
        i += 1;
    }
    if i >= aa.len() {
        return -1; // a is an ancestor of b -> a precedes
    }
    if i >= bb.len() {
        return 1; // b is an ancestor of a -> a follows
    }
    if node_child_index(dom, aa[i]) < node_child_index(dom, bb[i]) {
        -1
    } else {
        1
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

// op_fetch_url backs JS-level `fetch()` and XHR. Pre-#139 it used a
// process-wide `OnceLock<reqwest::Client>` initialised with no proxy, so
// every JS network call bypassed the configured upstream proxy. We now
// build a client per request, threading whatever `proxy_url` the page's
// ObscuraHttpClient was configured with.
//
// The per-request build cost is negligible (≪1ms) compared with the actual
// network round-trip; the simplification is worth not having to invalidate
// a cache when the proxy is reconfigured between fetches.
//
// Process-wide cache keyed by proxy URL. Previously we built a fresh
// reqwest::Client on every op_fetch_url call (every JS fetch(), XHR,
// dynamic script load). Each build re-initialised TLS roots and a
// fresh connection pool with zero reuse, costing ~5ms per fetch on top
// of any real network work. On an asset-heavy page with 30+ subresources
// that adds ~150ms of pure waste. With the cache, the first fetch on a
// given proxy pays the build cost once and every subsequent fetch reuses
// the same connection pool.
static FETCH_CLIENT_CACHE: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashMap<String, reqwest::Client>>,
> = std::sync::OnceLock::new();

/// Shared HTTP client cache for any code in obscura-js that needs a
/// reqwest::Client (op_fetch_url for JS-side fetch/XHR, the ES module
/// loader for dynamic imports). Keyed by proxy URL ("" = direct).
/// One client per distinct proxy, reused for every request, so the
/// connection pool actually warms up.
pub fn cached_request_client(proxy_url: Option<&str>) -> Result<reqwest::Client, String> {
    let key = proxy_url.unwrap_or("").to_string();
    let cache = FETCH_CLIENT_CACHE
        .get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()));
    if let Ok(read) = cache.read() {
        if let Some(client) = read.get(&key) {
            return Ok(client.clone());
        }
    }
    let client = build_request_client(proxy_url)?;
    if let Ok(mut write) = cache.write() {
        write.entry(key).or_insert_with(|| client.clone());
    }
    Ok(client)
}

fn build_request_client(proxy_url: Option<&str>) -> Result<reqwest::Client, String> {
    // Redirects are followed manually below so each hop can be re-validated
    // against the same SSRF policy as the initial URL (GHSA-8v6v-g4rh-jmcm).
    // With reqwest's default auto-follow, an attacker-controlled origin can
    // 302 to http://127.0.0.1 and read the internal-service body.
    // Per-request timeout so a scripted fetch()/XHR, or a CORS preflight OPTIONS
    // (issue #251), to a server that accepts the connection but never responds
    // cannot hang forever. Without it op_fetch_url never returns, the fetch
    // promise never settles, and the JS XHR is stuck at readyState 1 with no
    // completion event (which stranded Angular HttpClient). On timeout reqwest's
    // send().await errors, which op_fetch_url propagates and the fetch shim turns
    // into an XHR `error`/`loadend`. 30s matches the other clients in the
    // workspace; OBSCURA_FETCH_TIMEOUT_MS overrides it for tighter cloud limits.
    let timeout_ms: u64 = std::env::var("OBSCURA_FETCH_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000);
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_millis(timeout_ms))
        // SSRF guard: also reject hostnames that resolve to a private/loopback IP.
        .dns_resolver(std::sync::Arc::new(obscura_net::SsrfGuardResolver::new(false)))
        // Be explicit about pool size: default is unbounded which is fine,
        // but pool_idle_timeout default (90s) is short for SPA-heavy
        // workloads where the same origin is hit dozens of times across
        // a navigation. Keep connections warm longer.
        .pool_idle_timeout(std::time::Duration::from_secs(300))
        .tcp_keepalive(std::time::Duration::from_secs(60));
    if let Some(proxy) = proxy_url {
        let p = reqwest::Proxy::all(proxy)
            .map_err(|e| format!("Invalid op_fetch_url proxy '{}': {}", proxy, e))?;
        builder = builder.proxy(p);
    }
    builder
        .build()
        .map_err(|e| format!("failed to build reqwest::Client: {}", e))
}

/// Cap on the number of redirect hops op_fetch_url will follow.
/// Matches reqwest's default policy of 10.
const FETCH_REDIRECT_LIMIT: usize = 10;

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

    let (cookie_jar, in_flight, intercept_tx, proxy_url, callbacks) = {
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
        // Record the resource the page pulled in via fetch()/XHR so `--dump
        // assets` can list it (issue #301). URL is already absolute here, since
        // reqwest needs an absolute URL to send the request.
        gs.fetched_urls.push(url.clone());
        let jar = gs.cookie_jar.clone();
        let in_flight = gs.http_client.as_ref().map(|c| c.in_flight.clone());
        // #139: thread the configured proxy through to the per-request
        // reqwest::Client. Without this, op_fetch_url silently bypasses
        // BrowserContext.proxy_url for every JS fetch() / XHR call.
        let proxy_url = gs.http_client.as_ref().and_then(|c| c.proxy_url().map(|s| s.to_string()));
        tracing::debug!("op_fetch_url: intercept_enabled={}, has_tx={}", gs.intercept_enabled, gs.intercept_tx.is_some());
        let itx = if gs.intercept_enabled {
            gs.intercept_counter += 1;
            gs.intercept_tx.clone().map(|tx| (tx, format!("intercept-{}", gs.intercept_counter)))
        } else {
            None
        };
        (jar, in_flight, itx, proxy_url, gs.callbacks.clone())
    };

    // Slots the interception channel can override via Continue so a consumer
    // can rewrite url/method/headers/body before the request goes out.
    let mut override_url: Option<String> = None;
    let mut override_method: Option<String> = None;
    let mut override_headers: Option<HashMap<String, String>> = None;
    let mut override_body: Option<String> = None;

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
                Ok(InterceptResolution::Continue { url, method, headers, body }) => {
                    override_url = url;
                    override_method = method;
                    override_headers = headers;
                    override_body = body;
                    tracing::debug!(
                        "Interception: continue (overrides url={} method={} headers={} body={})",
                        override_url.is_some(), override_method.is_some(),
                        override_headers.is_some(), override_body.is_some()
                    );
                }
                Err(_) => {
                }
            }
        }
    }

    // Apply interception overrides (shadow the params for the rest of the op).
    // A Continue rewrite of the URL must pass the same SSRF / private-network
    // gate as the original request (checked above) and as redirects (checked
    // below). Without this re-validation a rewrite to an internal address would
    // bypass validate_fetch_url entirely.
    let url = if let Some(new_url) = override_url {
        if let Ok(parsed) = url::Url::parse(&new_url) {
            if let Err(reason) = validate_fetch_url(&parsed) {
                return Ok(serde_json::json!({
                    "status": 0,
                    "body": "",
                    "url": new_url,
                    "blocked": true,
                    "error": format!("Intercept rewrite to forbidden URL blocked: {}", reason),
                }).to_string());
            }
        }
        new_url
    } else {
        url
    };
    let method = override_method.unwrap_or(method);
    let body = override_body.unwrap_or(body);

    let client = cached_request_client(proxy_url.as_deref())
        .map_err(deno_error::JsErrorBox::generic)?;

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
        override_headers.unwrap_or_else(|| serde_json::from_str(&headers_json).unwrap_or_default());

    // Passive request observation (non-blocking). Fires for every request that
    // reaches the network (Fulfill/Fail from the interception channel short-
    // circuit earlier). on_request/on_response previously fired only for
    // navigation; this wires them for JS fetch()/XHR too.
    if let Some(ref cbs) = callbacks {
        if cbs.has_request_callbacks().await {
            if let Ok(parsed) = url::Url::parse(&url) {
                let info = RequestInfo {
                    url: parsed,
                    method: method.clone(),
                    headers: custom_headers.clone(),
                    resource_type: ResourceType::Fetch,
                };
                cbs.fire_request(&info).await;
            }
        }
    }

    // Stealth mode: route the scripted request through the wreq client so its
    // TLS fingerprint and Chrome client hints match the main navigation. The
    // rustls ClientHello plus missing client hints that op_fetch_url's reqwest
    // path sends otherwise read as a non-browser script to bot managers (the
    // AWS WAF challenge verify call, Akamai sensors, etc.).
    #[cfg(feature = "stealth")]
    {
        let stealth = {
            let st = state.borrow();
            let gs = st.borrow::<SharedState>().clone();
            let client = gs.borrow().stealth_client.clone();
            client
        };
        if let Some(stealth) = stealth {
            return stealth_fetch_all(
                stealth,
                url.clone(),
                req_method.as_str().to_string(),
                custom_headers.clone(),
                body.clone(),
                page_origin.clone(),
                is_cross_origin,
                mode.clone(),
                callbacks.clone(),
            )
            .await;
        }
    }

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

    // Follow redirects manually so the SSRF policy applies to every hop.
    // reqwest's auto-follow would bypass validate_fetch_url on the redirect
    // target and let an attacker-allowed origin 302 to http://127.0.0.1
    // (GHSA-8v6v-g4rh-jmcm).
    let mut current_url = url.clone();
    let mut current_method = req_method;
    let mut current_body = body;
    let mut redirects_followed: usize = 0;
    let response = loop {
        let mut req = client.request(current_method.clone(), &current_url);

        if is_cross_origin {
            req = req.header("Origin", &page_origin);
        }

        if !is_cross_origin {
            if let Some(ref jar) = cookie_jar {
                if let Ok(parsed_url) = url::Url::parse(&current_url) {
                    let cookie_header = jar.get_cookie_header(&parsed_url);
                    if !cookie_header.is_empty() {
                        req = req.header("Cookie", &cookie_header);
                    }
                }
            }
        }

        // Send a default User-Agent on fetch()/XHR requests (the navigation path
        // sets one, but this op did not, so scripted requests went out with no UA
        // and UA-gated servers rejected them). Honor an explicit override.
        if !custom_headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent")) {
            req = req.header(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
            );
        }

        for (k, v) in &custom_headers {
            req = req.header(k.as_str(), v.as_str());
        }

        if !current_body.is_empty() {
            req = req.body(current_body.clone());
        }

        if let Some(ref counter) = in_flight {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let resp = req.send().await.map_err(|e| {
            if let Some(ref counter) = in_flight {
                counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            deno_error::JsErrorBox::generic(e.to_string())
        })?;

        if let Some(ref counter) = in_flight {
            counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }

        if let Some(ref jar) = cookie_jar {
            if let Ok(parsed_url) = url::Url::parse(&current_url) {
                for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                    if let Ok(s) = val.to_str() {
                        jar.set_cookie(s, &parsed_url);
                    }
                }
            }
        }

        if !resp.status().is_redirection() {
            break resp;
        }

        let location_header = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let Some(location) = location_header else {
            // 3xx without a Location header is not actually a redirect.
            break resp;
        };

        let base = match url::Url::parse(&current_url) {
            Ok(b) => b,
            Err(_) => break resp,
        };
        let next_url = match base.join(&location) {
            Ok(u) => u,
            Err(_) => break resp,
        };

        // Re-validate every redirect target against the SSRF policy.
        if let Err(reason) = validate_fetch_url(&next_url) {
            return Ok(serde_json::json!({
                "status": 0,
                "body": "",
                "url": next_url.to_string(),
                "headers": {},
                "blocked": true,
                "error": format!("Redirect to forbidden URL blocked: {}", reason),
            })
            .to_string());
        }

        redirects_followed += 1;
        if redirects_followed > FETCH_REDIRECT_LIMIT {
            return Ok(serde_json::json!({
                "status": 0,
                "body": "",
                "url": next_url.to_string(),
                "headers": {},
                "blocked": true,
                "error": format!("Too many redirects (>{})", FETCH_REDIRECT_LIMIT),
            })
            .to_string());
        }

        // Browser semantics: 301/302/303 downgrade to GET with no body.
        // 307/308 preserve method and body.
        let status_code = resp.status().as_u16();
        if status_code == 301 || status_code == 302 || status_code == 303 {
            current_method = reqwest::Method::GET;
            current_body.clear();
        }

        current_url = next_url.to_string();
    };

    let status = response.status().as_u16();

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
    if let Some(ref cbs) = callbacks {
        if cbs.has_response_callbacks().await {
            let resp = fetch_response(&url, status, resp_headers.clone(), resp_bytes.to_vec());
            let info = RequestInfo {
                url: resp.url.clone(),
                method: method.clone(),
                headers: resp_headers.clone(),
                resource_type: ResourceType::Fetch,
            };
            cbs.fire_response(&info, &resp).await;
        }
    }
    let response_request_id = {
        let state_borrow = state.borrow();
        let gs = state_borrow.borrow::<SharedState>().clone();
        let mut gs = gs.borrow_mut();
        gs.network_response_body_counter += 1;
        let request_id = format!("fetch-{}", gs.network_response_body_counter);
        let max_entries = response_body_entry_limit();
        let max_bytes = response_body_byte_limit();
        if max_entries > 0 && max_bytes > 0 && resp_bytes.len() <= max_bytes {
            gs.network_response_bodies.insert(
                request_id.clone(),
                StoredNetworkResponseBody {
                    body: resp_body.clone(),
                    base64_encoded: false,
                },
            );
            gs.network_response_body_order.push_back(request_id.clone());
            while gs.network_response_body_order.len() > max_entries {
                if let Some(oldest) = gs.network_response_body_order.pop_front() {
                    gs.network_response_bodies.remove(&oldest);
                }
            }
        }
        // Record a network event so the CDP layer emits requestWillBeSent /
        // responseReceived for this script-initiated request (#406). Keyed by
        // the same fetch-{N} id as the stored body so Network.getResponseBody
        // resolves. Capped to keep a long-lived page from growing unbounded.
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        gs.js_network_events.push(JsNetworkEvent {
            request_id: request_id.clone(),
            url: url.clone(),
            method: method.clone(),
            status,
            response_headers: resp_headers.clone(),
            body_size: resp_bytes.len(),
            timestamp,
        });
        const MAX_JS_NETWORK_EVENTS: usize = 4096;
        if gs.js_network_events.len() > MAX_JS_NETWORK_EVENTS {
            let overflow = gs.js_network_events.len() - MAX_JS_NETWORK_EVENTS;
            gs.js_network_events.drain(0..overflow);
        }
        request_id
    };

    tracing::debug!("op_fetch_url completed: {} {} ({} bytes)", method, url, resp_body.len());

    Ok(serde_json::json!({
        "status": status,
        "body": resp_body,
        "bodyBase64": resp_body_base64,
        "requestId": response_request_id,
        "url": url,
        "headers": resp_headers,
    })
    .to_string())
}

/// Assemble a `Response` for the on_response interception callbacks from the
/// parts op_fetch_url already holds. Navigation gets a Response straight from
/// the http client, but the JS fetch path builds the pieces itself.
fn fetch_response(url: &str, status: u16, headers: HashMap<String, String>, body: Vec<u8>) -> Response {
    Response {
        url: url::Url::parse(url).unwrap_or_else(|_| url::Url::parse("http://0.0.0.0/").unwrap()),
        status,
        headers,
        body,
        redirected_from: Vec::new(),
    }
}

/// Stealth-mode scripted fetch()/XHR: mirrors op_fetch_url's redirect, SSRF,
/// and CORS semantics but sends every hop through the wreq stealth client so
/// the request carries the Chrome TLS fingerprint and client hints. Cookie
/// handling lives inside StealthHttpClient::send_single, which shares the
/// context jar. Response bodies are not mirrored into the CDP
/// Network.getResponseBody buffer here; that is a follow-up for stealth fetches.
#[cfg(feature = "stealth")]
async fn stealth_fetch_all(
    stealth: Arc<StealthHttpClient>,
    url: String,
    method: String,
    custom_headers: HashMap<String, String>,
    body: String,
    page_origin: String,
    is_cross_origin: bool,
    mode: String,
    callbacks: Option<Arc<CallbackRegistry>>,
) -> Result<String, deno_error::JsErrorBox> {
    let mut current_url = url.clone();
    let mut current_method = method;
    let mut current_body = body;
    let mut redirects_followed: usize = 0;

    let (status, resp_headers, resp_bytes): (u16, HashMap<String, String>, Vec<u8>) = loop {
        let parsed_current = match url::Url::parse(&current_url) {
            Ok(u) => u,
            Err(_) => {
                return Ok(serde_json::json!({
                    "status": 0, "body": "", "url": current_url, "headers": {},
                })
                .to_string());
            }
        };

        let mut req_headers: HashMap<String, String> = HashMap::new();
        if is_cross_origin {
            req_headers.insert("origin".to_string(), page_origin.clone());
        }
        for (k, v) in &custom_headers {
            req_headers.insert(k.to_lowercase(), v.clone());
        }

        let r = stealth
            .send_single(&current_method, &parsed_current, &req_headers, &current_body)
            .await
            .map_err(|e| deno_error::JsErrorBox::generic(e.to_string()))?;

        if !(300..400).contains(&r.status) {
            break (r.status, r.headers, r.body);
        }
        let Some(location) = r.headers.get("location").cloned() else {
            break (r.status, r.headers, r.body);
        };
        let next_url = match parsed_current.join(&location) {
            Ok(u) => u,
            Err(_) => break (r.status, r.headers, r.body),
        };
        // Re-validate every redirect target against the SSRF policy, matching
        // op_fetch_url (GHSA-8v6v-g4rh-jmcm).
        if let Err(reason) = validate_fetch_url(&next_url) {
            return Ok(serde_json::json!({
                "status": 0, "body": "", "url": next_url.to_string(), "headers": {},
                "blocked": true,
                "error": format!("Redirect to forbidden URL blocked: {}", reason),
            })
            .to_string());
        }
        redirects_followed += 1;
        if redirects_followed > FETCH_REDIRECT_LIMIT {
            return Ok(serde_json::json!({
                "status": 0, "body": "", "url": next_url.to_string(), "headers": {},
                "blocked": true,
                "error": format!("Too many redirects (>{})", FETCH_REDIRECT_LIMIT),
            })
            .to_string());
        }
        // Browser semantics: 301/302/303 downgrade to GET with no body.
        if r.status == 301 || r.status == 302 || r.status == 303 {
            current_method = "GET".to_string();
            current_body.clear();
        }
        current_url = next_url.to_string();
    };

    if is_cross_origin && mode == "cors" {
        let allowed = resp_headers
            .get("access-control-allow-origin")
            .map(|s| s.as_str())
            .unwrap_or("");
        if allowed != "*" && allowed != page_origin {
            return Ok(serde_json::json!({
                "status": 0, "body": "", "url": url, "headers": {},
                "corsBlocked": true,
                "corsError": format!(
                    "CORS error: Origin '{}' not in Access-Control-Allow-Origin '{}'",
                    page_origin, allowed
                ),
            })
            .to_string());
        }
    }

    let resp_body = String::from_utf8_lossy(&resp_bytes).to_string();
    let resp_body_base64 = BASE64.encode(&resp_bytes);
    if let Some(ref cbs) = callbacks {
        if cbs.has_response_callbacks().await {
            let resp = fetch_response(&url, status, resp_headers.clone(), resp_bytes.clone());
            let info = RequestInfo {
                url: resp.url.clone(),
                method: current_method.clone(),
                headers: resp_headers.clone(),
                resource_type: ResourceType::Fetch,
            };
            cbs.fire_response(&info, &resp).await;
        }
    }

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

    let mut remainder = url;
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            continue;
        }

        let Some(index) = remainder.find(part) else {
            return false;
        };

        if first && !pattern.starts_with('*') && index != 0 {
            return false;
        }

        remainder = &remainder[index + part.len()..];
        first = false;
    }

    pattern.ends_with('*') || remainder.is_empty()
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn glob_match_handles_cdp_blocked_url_patterns() {
        assert!(glob_match(
            "*://*.google.com/maps/vt/*",
            "https://www.google.com/maps/vt/pb=!1m4!1m3",
        ));
        assert!(glob_match(
            "*://*.gstatic.com/*.woff2",
            "https://fonts.gstatic.com/s/inter/v18/font.woff2",
        ));
        assert!(glob_match(
            "https://example.com/assets/*",
            "https://example.com/assets/app.js",
        ));
        assert!(!glob_match(
            "https://example.com/assets/*",
            "https://cdn.example.com/assets/app.js",
        ));
        assert!(!glob_match(
            "*://*.gstatic.com/*.woff2",
            "https://fonts.gstatic.com/s/inter/v18/font.woff",
        ));
    }
}

fn validate_fetch_url(url: &url::Url) -> Result<(), String> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" && scheme != "file" {
        return Err(format!(
            "Forbidden URL scheme '{}' - only http, https, and file are allowed",
            scheme
        ));
    }

    if scheme == "file" || obscura_net::env_allows_private_network() {
        return Ok(());
    }

    if let Some(host) = url.host() {
        match host {
            url::Host::Ipv4(ip) => {
                if obscura_net::is_forbidden_ip(std::net::IpAddr::V4(ip)) {
                    return Err(format!(
                        "Access to private/internal IP address {} is not allowed",
                        ip
                    ));
                }
            }
            url::Host::Ipv6(ip) => {
                if obscura_net::is_forbidden_ip(std::net::IpAddr::V6(ip)) {
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
    gs.url = url.to_string();
    gs.pending_navigation = Some((url.to_string(), method.to_string(), body.to_string()));
}

#[op2(async)]
async fn op_sleep(#[number] millis: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
}

// Records a binding call from page JS. The CDP layer drains this queue
// after every dispatch and emits one `Runtime.bindingCalled` event per
// entry, that's how puppeteer's `page.exposeFunction` callbacks fire.
#[op2(fast)]
fn op_binding_called(state: &OpState, #[string] name: &str, #[string] payload: &str) {
    let gs = state.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();
    gs.pending_binding_calls.push((name.to_string(), payload.to_string()));
}

/// Real WebCrypto `crypto.subtle.digest`. `algorithm` is the SubtleCrypto
/// algorithm name (`SHA-1` / `SHA-256` / `SHA-384` / `SHA-512`, plus the
/// FIPS 180-4 truncated variants `SHA-512/224` and `SHA-512/256`). The JS
/// shim validates the name; any other value is unreachable.
/// Returns the raw digest bytes so the JS shim can hand them back as an ArrayBuffer.
#[op2]
#[buffer]
fn op_subtle_digest(#[string] algorithm: &str, #[buffer] data: &[u8]) -> Vec<u8> {
    use sha1::Digest as _;
    let alg = algorithm.to_ascii_uppercase();
    match alg.as_str() {
        "SHA-1" => sha1::Sha1::digest(data).to_vec(),
        "SHA-256" => sha2::Sha256::digest(data).to_vec(),
        "SHA-384" => sha2::Sha384::digest(data).to_vec(),
        "SHA-512" => sha2::Sha512::digest(data).to_vec(),
        "SHA-512/224" => sha2::Sha512_224::digest(data).to_vec(),
        "SHA-512/256" => sha2::Sha512_256::digest(data).to_vec(),
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// WebCrypto (crypto.subtle) secret-key primitives.
//
// These ops are stateless. The JS shim in bootstrap.js owns the CryptoKey
// objects and their raw key bytes; it hands the bytes plus normalized algorithm
// parameters to these ops for each operation. Only secret-key algorithms live
// here (HMAC, AES-GCM/CBC/CTR, PBKDF2, HKDF); public-key algorithms are rejected
// in the shim. A fallible op returns a JsErrorBox that the shim turns into the
// appropriate DOMException (OperationError for a bad tag or padding, etc.).
// ---------------------------------------------------------------------------

fn crypto_err(msg: impl std::fmt::Display) -> deno_error::JsErrorBox {
    deno_error::JsErrorBox::generic(msg.to_string())
}

/// HMAC sign. `hash` is a normalized SubtleCrypto hash name; any key length is
/// accepted (HMAC pads or hashes the key per RFC 2104). Returns the MAC bytes;
/// the shim does the constant-time-insensitive compare for `verify`.
#[op2]
#[buffer]
fn op_subtle_hmac(
    #[string] hash: &str,
    #[buffer] key: &[u8],
    #[buffer] data: &[u8],
) -> Result<Vec<u8>, deno_error::JsErrorBox> {
    use hmac::{Hmac, Mac};
    macro_rules! run {
        ($d:ty) => {{
            let mut mac = Hmac::<$d>::new_from_slice(key).map_err(crypto_err)?;
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }};
    }
    Ok(match hash {
        "SHA-1" => run!(sha1::Sha1),
        "SHA-256" => run!(sha2::Sha256),
        "SHA-384" => run!(sha2::Sha384),
        "SHA-512" => run!(sha2::Sha512),
        _ => return Err(crypto_err("unsupported HMAC hash")),
    })
}

/// AES-GCM encrypt/decrypt. WebCrypto's ciphertext carries the auth tag
/// appended, which is exactly RustCrypto's combined form, so this maps 1:1.
/// Restricted to a 96-bit IV and 128-bit tag (the WebCrypto defaults and the
/// overwhelming majority of real usage); the shim rejects other tag lengths.
#[op2]
#[buffer]
fn op_subtle_aes_gcm(
    encrypt: bool,
    #[buffer] key: &[u8],
    #[buffer] iv: &[u8],
    #[buffer] aad: &[u8],
    #[buffer] data: &[u8],
) -> Result<Vec<u8>, deno_error::JsErrorBox> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::aes::{Aes192, Aes256};
    use aes_gcm::{AesGcm, Nonce};
    type Aes192Gcm = AesGcm<Aes192, aes_gcm::aead::consts::U12>;
    type Aes256Gcm = AesGcm<Aes256, aes_gcm::aead::consts::U12>;

    if iv.len() != 12 {
        return Err(crypto_err("AES-GCM requires a 96-bit (12-byte) IV"));
    }
    let nonce = Nonce::from_slice(iv);
    macro_rules! run {
        ($ty:ty) => {{
            let cipher = <$ty>::new_from_slice(key).map_err(crypto_err)?;
            if encrypt {
                cipher
                    .encrypt(nonce, Payload { msg: data, aad })
                    .map_err(|_| crypto_err("AES-GCM encryption failed"))?
            } else {
                cipher
                    .decrypt(nonce, Payload { msg: data, aad })
                    .map_err(|_| crypto_err("AES-GCM decryption failed: authentication tag mismatch"))?
            }
        }};
    }
    Ok(match key.len() {
        16 => run!(aes_gcm::Aes128Gcm),
        24 => run!(Aes192Gcm),
        32 => run!(Aes256Gcm),
        _ => return Err(crypto_err("AES-GCM key must be 128, 192, or 256 bits")),
    })
}

/// AES-CBC encrypt/decrypt with PKCS#7 padding (the only padding WebCrypto
/// AES-CBC uses) and a 16-byte IV.
#[op2]
#[buffer]
fn op_subtle_aes_cbc(
    encrypt: bool,
    #[buffer] key: &[u8],
    #[buffer] iv: &[u8],
    #[buffer] data: &[u8],
) -> Result<Vec<u8>, deno_error::JsErrorBox> {
    use cbc::cipher::block_padding::Pkcs7;
    use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
    use cbc::{Decryptor, Encryptor};

    if iv.len() != 16 {
        return Err(crypto_err("AES-CBC requires a 16-byte IV"));
    }
    macro_rules! run {
        ($cipher:ty) => {{
            if encrypt {
                Encryptor::<$cipher>::new_from_slices(key, iv)
                    .map_err(crypto_err)?
                    .encrypt_padded_vec_mut::<Pkcs7>(data)
            } else {
                Decryptor::<$cipher>::new_from_slices(key, iv)
                    .map_err(crypto_err)?
                    .decrypt_padded_vec_mut::<Pkcs7>(data)
                    .map_err(|_| crypto_err("AES-CBC decryption failed: invalid padding"))?
            }
        }};
    }
    Ok(match key.len() {
        16 => run!(aes::Aes128),
        24 => run!(aes::Aes192),
        32 => run!(aes::Aes256),
        _ => return Err(crypto_err("AES-CBC key must be 128, 192, or 256 bits")),
    })
}

/// AES-CTR. Encrypt and decrypt are the same keystream XOR. `counter_length` is
/// the WebCrypto counter width in bits; it selects the RustCrypto CTR flavor so
/// only the low `counter_length` bits of the 16-byte block increment.
#[op2]
#[buffer]
fn op_subtle_aes_ctr(
    #[buffer] key: &[u8],
    #[buffer] counter: &[u8],
    counter_length: u32,
    #[buffer] data: &[u8],
) -> Result<Vec<u8>, deno_error::JsErrorBox> {
    use ctr::cipher::{KeyIvInit, StreamCipher};

    if counter.len() != 16 {
        return Err(crypto_err("AES-CTR requires a 16-byte counter block"));
    }
    let mut buf = data.to_vec();
    macro_rules! run {
        ($ty:ty) => {{
            <$ty>::new_from_slices(key, counter)
                .map_err(crypto_err)?
                .apply_keystream(&mut buf);
        }};
    }
    macro_rules! by_key {
        ($flavor:ident) => {
            match key.len() {
                16 => run!(ctr::$flavor<aes::Aes128>),
                24 => run!(ctr::$flavor<aes::Aes192>),
                32 => run!(ctr::$flavor<aes::Aes256>),
                _ => return Err(crypto_err("AES-CTR key must be 128, 192, or 256 bits")),
            }
        };
    }
    match counter_length {
        128 => by_key!(Ctr128BE),
        64 => by_key!(Ctr64BE),
        32 => by_key!(Ctr32BE),
        _ => return Err(crypto_err("AES-CTR supports counter lengths of 32, 64, or 128 bits")),
    }
    Ok(buf)
}

/// PBKDF2 key derivation. `length` is the derived-bits output in bytes.
#[op2]
#[buffer]
fn op_subtle_pbkdf2(
    #[string] hash: &str,
    #[buffer] password: &[u8],
    #[buffer] salt: &[u8],
    iterations: u32,
    length: u32,
) -> Result<Vec<u8>, deno_error::JsErrorBox> {
    use pbkdf2::pbkdf2_hmac;
    let mut dk = vec![0u8; length as usize];
    match hash {
        "SHA-1" => pbkdf2_hmac::<sha1::Sha1>(password, salt, iterations, &mut dk),
        "SHA-256" => pbkdf2_hmac::<sha2::Sha256>(password, salt, iterations, &mut dk),
        "SHA-384" => pbkdf2_hmac::<sha2::Sha384>(password, salt, iterations, &mut dk),
        "SHA-512" => pbkdf2_hmac::<sha2::Sha512>(password, salt, iterations, &mut dk),
        _ => return Err(crypto_err("unsupported PBKDF2 hash")),
    }
    Ok(dk)
}

/// HKDF key derivation. `length` is the output length in bytes. An empty salt
/// behaves as RFC 5869 specifies (HMAC zero-pads it to the block size, which is
/// what browsers do).
#[op2]
#[buffer]
fn op_subtle_hkdf(
    #[string] hash: &str,
    #[buffer] ikm: &[u8],
    #[buffer] salt: &[u8],
    #[buffer] info: &[u8],
    length: u32,
) -> Result<Vec<u8>, deno_error::JsErrorBox> {
    use hkdf::Hkdf;
    let mut okm = vec![0u8; length as usize];
    macro_rules! run {
        ($d:ty) => {
            Hkdf::<$d>::new(Some(salt), ikm)
                .expand(info, &mut okm)
                .map_err(|_| crypto_err("HKDF: requested key length is too long"))?
        };
    }
    match hash {
        "SHA-1" => run!(sha1::Sha1),
        "SHA-256" => run!(sha2::Sha256),
        "SHA-384" => run!(sha2::Sha384),
        "SHA-512" => run!(sha2::Sha512),
        _ => return Err(crypto_err("unsupported HKDF hash")),
    }
    Ok(okm)
}

/// Fill `len` bytes from the OS CSPRNG. Backs `crypto.getRandomValues`,
/// `crypto.randomUUID`, and `generateKey`, replacing the old Math.random shim
/// (which was neither uniform across typed-array widths nor cryptographically
/// random, and was a fingerprinting tell).
#[op2]
#[buffer]
fn op_random_bytes(len: u32) -> Result<Vec<u8>, deno_error::JsErrorBox> {
    let mut buf = vec![0u8; len as usize];
    getrandom::getrandom(&mut buf).map_err(|e| crypto_err(format!("getrandom failed: {e}")))?;
    Ok(buf)
}

/// Serialize a parsed URL into the WHATWG IDL component shape consumed by the
/// `URL` class in bootstrap.js. Getters read these fields directly so no op
/// call happens per property access.
fn url_components(u: &url::Url) -> serde_json::Value {
    let port = u.port().map(|p| p.to_string()).unwrap_or_default();
    let hostname = u.host_str().unwrap_or("").to_string();
    let host = if hostname.is_empty() {
        String::new()
    } else if port.is_empty() {
        hostname.clone()
    } else {
        format!("{hostname}:{port}")
    };
    // WHATWG search/hash getters return "" for a null OR empty component.
    let search = match u.query() {
        Some(q) if !q.is_empty() => format!("?{q}"),
        _ => String::new(),
    };
    let hash = match u.fragment() {
        Some(f) if !f.is_empty() => format!("#{f}"),
        _ => String::new(),
    };
    serde_json::json!({
        "ok": true,
        "href": u.as_str(),
        "protocol": format!("{}:", u.scheme()),
        "username": u.username(),
        "password": u.password().unwrap_or(""),
        "host": host,
        "hostname": hostname,
        "port": port,
        "pathname": u.path(),
        "search": search,
        "hash": hash,
        "origin": u.origin().ascii_serialization(),
    })
}

/// Parse `href` (optionally resolved against `base`) with the WHATWG-compliant
/// `url` crate. Returns the component JSON, or `{"ok":false}` when the input is
/// not a valid URL (the JS side turns that into a TypeError, per spec).
#[op2]
#[string]
fn op_url_parse(#[string] href: &str, #[string] base: &str) -> String {
    // The url crate can panic on a few pathological inputs (internal range
    // slicing); catch it so a bad URL never aborts the process.
    std::panic::catch_unwind(|| {
        let parsed = if base.is_empty() {
            url::Url::parse(href)
        } else {
            url::Url::parse(base).and_then(|b| b.join(href))
        };
        match parsed {
            Ok(u) => url_components(&u).to_string(),
            Err(_) => "{\"ok\":false}".to_string(),
        }
    })
    .unwrap_or_else(|_| "{\"ok\":false}".to_string())
}

/// Apply a WHATWG URL setter (`part` = href/protocol/username/password/host/
/// hostname/port/pathname/search/hash) to `href` and return the new components.
fn url_set_inner(href: &str, part: &str, value: &str) -> Option<serde_json::Value> {
    let mut u = url::Url::parse(href).ok()?;
    match part {
        "href" => {
            let nu = url::Url::parse(value).ok()?;
            return Some(url_components(&nu));
        }
        "protocol" => {
            let _ = u.set_scheme(value.trim_end_matches(':'));
        }
        "username" => {
            let _ = u.set_username(value);
        }
        "password" => {
            let _ = u.set_password(if value.is_empty() { None } else { Some(value) });
        }
        "host" => set_host_port(&mut u, value),
        "hostname" => {
            if !value.is_empty() {
                let _ = u.set_host(Some(value));
            }
        }
        "port" => {
            if value.is_empty() {
                let _ = u.set_port(None);
            } else if let Ok(p) = value.parse::<u16>() {
                let _ = u.set_port(Some(p));
            }
        }
        "pathname" => u.set_path(value),
        "search" => {
            let q = value.strip_prefix('?').unwrap_or(value);
            u.set_query(if q.is_empty() { None } else { Some(q) });
        }
        "hash" => {
            let f = value.strip_prefix('#').unwrap_or(value);
            u.set_fragment(if f.is_empty() { None } else { Some(f) });
        }
        _ => {}
    }
    Some(url_components(&u))
}

#[op2]
#[string]
fn op_url_set(#[string] href: &str, #[string] part: &str, #[string] value: &str) -> String {
    // Some url-crate setters panic on pathological inputs (the url-setters WPT
    // tests exercise these). Catch the unwind and treat it as a no-op setter,
    // returning the URL unchanged, which matches WHATWG "do nothing on invalid".
    match std::panic::catch_unwind(|| url_set_inner(href, part, value)) {
        Ok(Some(v)) => v.to_string(),
        _ => match url::Url::parse(href) {
            Ok(u) => url_components(&u).to_string(),
            Err(_) => "{\"ok\":false}".to_string(),
        },
    }
}

/// Best-effort `host` setter: split `host[:port]` (handling bracketed IPv6) and
/// apply hostname and port separately, since `url::Url::set_host` rejects a port.
fn set_host_port(u: &mut url::Url, value: &str) {
    // IPv6 literals are bracketed; never split inside the brackets.
    if value.starts_with('[') {
        if let Some(close) = value.find(']') {
            let host = &value[..=close];
            let rest = &value[close + 1..];
            if u.set_host(Some(host)).is_ok() {
                if let Some(p) = rest.strip_prefix(':') {
                    if let Ok(pn) = p.parse::<u16>() {
                        let _ = u.set_port(Some(pn));
                    }
                }
            }
            return;
        }
    }
    if let Some(idx) = value.rfind(':') {
        let (h, p) = (&value[..idx], &value[idx + 1..]);
        if p.is_empty() || p.chars().all(|c| c.is_ascii_digit()) {
            if u.set_host(Some(h)).is_ok() {
                if p.is_empty() {
                    let _ = u.set_port(None);
                } else if let Ok(pn) = p.parse::<u16>() {
                    let _ = u.set_port(Some(pn));
                }
            }
            return;
        }
    }
    let _ = u.set_host(Some(value));
}

/// Resolve `href` against optional `base` and return only the serialized
/// absolute URL (no component breakdown). Used by the hot `a.href`/`area.href`
/// getter, which only needs the resolved string, so it avoids building and
/// re-parsing the full component JSON. Returns "" when the input is invalid.
#[op2]
#[string]
fn op_url_resolve(#[string] href: &str, #[string] base: &str) -> String {
    std::panic::catch_unwind(|| {
        let parsed = if base.is_empty() {
            url::Url::parse(href)
        } else {
            url::Url::parse(base).and_then(|b| b.join(href))
        };
        parsed.map(|u| u.as_str().to_string()).unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Canonical (lowercased) WHATWG name for a TextDecoder label, or "" if the
/// label is unknown (the JS constructor turns "" into a RangeError).
#[op2]
#[string]
fn op_encoding_for_label(#[string] label: &str) -> String {
    obscura_net::label_name(label).unwrap_or_default()
}

/// Decode bytes with a legacy/explicit encoding via encoding_rs. Returns
/// {"ok":true,"v":<string>} or {"ok":false} (unknown label, or a fatal decode
/// error). The UTF-8 non-fatal common case is handled in JS without this op.
#[op2]
#[string]
fn op_text_decode(#[string] label: &str, #[buffer] bytes: &[u8], fatal: bool, ignore_bom: bool) -> String {
    match obscura_net::decode_with_label(label, bytes, fatal, ignore_bom) {
        Some(s) => serde_json::json!({ "ok": true, "v": s }).to_string(),
        None => "{\"ok\":false}".to_string(),
    }
}

/// Re-encode a URL query component using a non-UTF-8 document encoding override
/// (the WHATWG "encoding override"). `query` is the already-UTF-8-decoded query
/// string; `label` the target charset; `special` whether the URL has a special
/// scheme (adds `'` to the percent-encode set). Returns the encoded query, or
/// the input unchanged if the label is unknown. Only called by the JS anchor
/// path when the document is non-UTF-8, so the UTF-8 hot path never reaches it.
#[op2]
#[string]
fn op_url_encode_query(#[string] query: &str, #[string] label: &str, special: bool) -> String {
    obscura_net::url_encode_query(query, label, special).unwrap_or_else(|| query.to_string())
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
            op_binding_called(),
            op_subtle_digest(),
            op_subtle_hmac(),
            op_subtle_aes_gcm(),
            op_subtle_aes_cbc(),
            op_subtle_aes_ctr(),
            op_subtle_pbkdf2(),
            op_subtle_hkdf(),
            op_random_bytes(),
            op_url_parse(),
            op_url_set(),
            op_url_resolve(),
            op_encoding_for_label(),
            op_text_decode(),
            op_url_encode_query(),
        ]),
        ..Default::default()
    }
}
