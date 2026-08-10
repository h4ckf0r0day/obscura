use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use deno_core::op2;
use deno_core::Extension;
#[cfg(feature = "render")]
use deno_core::JsBuffer;
use deno_core::v8;
use deno_core::OpState;
use obscura_dom::{DomTree, NodeData, NodeId};
use obscura_dom::tree::{AttachShadowError, ShadowRootMode};
#[cfg(feature = "render")]
use obscura_net::{RequestCredentials, RequestMode, ResourceRequest};
#[cfg(feature = "stealth")]
use obscura_net::StealthHttpClient;
use obscura_net::{
    CallbackRegistry, CookieJar, ObscuraHttpClient, RequestInfo, ResourceType, Response,
};
use tokio::sync::Mutex;

#[cfg(feature = "render")]
use serde::Deserialize;

use crate::import_map::ImportMap;

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

#[cfg(feature = "render")]
pub use obscura_render::ImageRequestProfile;

/// A live Canvas2D backing store retained from V8. `JsBuffer` owns a shared
/// reference to the ArrayBuffer backing store, so the pixels stay valid while
/// the canvas wrapper and native page state share it. Paint only borrows these
/// bytes synchronously while JavaScript is not executing.
#[cfg(feature = "render")]
pub(crate) struct CanvasBackingSurface {
    pub width: u32,
    pub height: u32,
    pub pixels: JsBuffer,
}

pub struct ObscuraState {
    pub dom: Option<DomTree>,
    pub url: String,
    /// WHATWG canonical name of the document's character encoding (e.g.
    /// "UTF-8", "EUC-JP"). Backs `document.characterSet` and the URL query
    /// encoding override for `<a>`/`<area>` hrefs in legacy-charset documents.
    pub encoding: String,
    pub title: String,
    /// URL of the document that initiated this document's navigation. Direct
    /// browser/API navigations leave this empty; document-initiated
    /// navigations set it to the source document URL.
    pub referrer: String,
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
    // Frame documents that have been fetched and are waiting for a realm.
    // Building one needs the whole runtime, which an op cannot reach, so
    // `op_frame_document_ready` queues here and the Page drains it between
    // event loop turns. Same shape as `pending_binding_calls`.
    pub pending_frames: Vec<PendingFrame>,
    pub frame_id_counter: u32,
    /// Which frame this state belongs to; 0 is the page's own realm.
    pub frame_id: u32,
    // postMessage traffic between realms, waiting to be delivered. A realm
    // cannot reach another realm's context on its own, so the message is queued
    // here and the Page dispatches it, the same way frames themselves are
    // built. Queued on the *page's* state whichever realm sent it, so one drain
    // sees the traffic of the whole tree.
    pub pending_frame_messages: Vec<PendingFrameMessage>,
    /// Bytes of payload currently queued above, tracked rather than summed so
    /// the cap costs nothing per message.
    pub pending_frame_message_bytes: usize,
    /// Requests initiated by this runtime only. Browser contexts share their
    /// transport client across pages, so the client's aggregate counter cannot
    /// be used as a page-readiness signal.
    pub page_in_flight: Arc<std::sync::atomic::AtomicU32>,
    /// Monotonic generation for observable changes to the connected document.
    /// The browser settle policy samples this to distinguish useful deferred
    /// rendering work from unrelated long-lived timers.
    pub activity_generation: u64,
    /// Monotonic identity of the currently installed document. Async resource
    /// completions use this to discard bytes and lifecycle results belonging
    /// to a navigation that has already been replaced.
    pub document_generation: u64,
    /// Final image/font-aware layout shared by CSSOM geometry and screenshots.
    /// DOM/style/viewport changes clear this value but retain resource bytes.
    #[cfg(feature = "render")]
    pub prepared_render: Option<obscura_render::PreparedRender>,
    /// CSS media type selected for the next retained layout. Live pages use
    /// screen; PDF export switches to print for one synchronous capture and
    /// restores screen before returning.
    #[cfg(feature = "render")]
    pub render_media: obscura_render::CssMediaType,
    /// Explicit document-timeline sample used by the next style/layout flush.
    /// Captures set this to either deterministic T=0 or live document time.
    #[cfg(feature = "render")]
    pub animation_sample: obscura_render::AnimationSample,
    #[cfg(feature = "render")]
    pub animation_timeline: obscura_render::AnimationTimelineState,
    #[cfg(feature = "render")]
    pub animation_timeline_origin: std::time::Instant,
    /// Host/HTML task epoch for document-timeline sampling. Geometry and
    /// computed-style reads within one task share one frozen animation frame.
    #[cfg(feature = "render")]
    pub animation_task_generation: u64,
    #[cfg(feature = "render")]
    pub animation_sampled_task_generation: u64,
    /// Connected mutations awaiting dependency-indexed retained style refresh.
    /// Tree changes carry stable node/parent ids so a later geometry read can
    /// coalesce framework DOM churn into one conservative local cascade.
    #[cfg(feature = "render")]
    pub pending_style_mutations: Vec<obscura_render::RetainedStyleMutation>,
    /// Page-lifetime raw image/font bytes. A new document resets this cache;
    /// relayout of the same document reuses it without refetching.
    #[cfg(feature = "render")]
    pub render_resources: obscura_render::RenderResourceCache,
    /// Waiters sharing an asynchronous HTMLImageElement request. The key keeps
    /// navigation identity and request credentials separate so neither stale
    /// pages nor incompatible CORS profiles share a completion.
    #[cfg(feature = "render")]
    pub render_image_in_flight:
        HashMap<(u64, String, ImageRequestProfile), Vec<tokio::sync::oneshot::Sender<()>>>,
    /// One exact-key compiled author stylesheet for this document. Connected
    /// mutations still discard `prepared_render`; the next prepare reuses only
    /// parsing/indexing when ordered CSS source and viewport remain identical.
    #[cfg(feature = "render")]
    pub stylesheet_cache: obscura_render::StylesheetCache,
    /// Script-created faces in this document's `FontFaceSet`. This is separate
    /// from the DOM so the bridge does not manufacture a selector-visible
    /// `<style>` element merely to feed the renderer.
    #[cfg(feature = "render")]
    pub dynamic_fonts: Vec<obscura_render::DynamicFontFace>,
    /// Live Canvas2D backing stores keyed by stable DOM identity. Pixel damage
    /// updates this resource independently of retained style/layout geometry.
    #[cfg(feature = "render")]
    pub(crate) canvas_surfaces: HashMap<NodeId, CanvasBackingSurface>,
    #[cfg(feature = "render")]
    pub viewport: (f32, f32),
    /// Root scrolling offset in CSS pixels. With render enabled this is
    /// clamped against the cached document overflow and is the single source
    /// read by CSSOM geometry and screenshot paint.
    #[cfg(feature = "render")]
    pub scroll_offset: (f32, f32),
    /// Element scroll offsets persist by DOM identity across relayout. Dense
    /// renderer ScrollIds are rebuild-local and are resolved only into the
    /// cached snapshot below.
    #[cfg(feature = "render")]
    pub element_scroll_offsets: HashMap<NodeId, (f32, f32)>,
    #[cfg(feature = "render")]
    pub scroll_generation: u64,
    #[cfg(feature = "render")]
    pub resolved_scroll: Option<(u64, obscura_render::ResolvedScrollState)>,
    /// Window-global import-map state shared by parser-discovered scripts,
    /// dynamically inserted import maps, and the module loader.
    pub(crate) import_map: Rc<RefCell<ImportMap>>,
    /// HTML's per-script "already started" flag.  This is native page state,
    /// rather than wrapper state, because it must survive moves and clones and
    /// because fragment parsing can create nodes before a JS wrapper exists.
    pub(crate) already_started_scripts: RefCell<HashSet<NodeId>>,
}

/// A frame document waiting to be given a realm.
pub struct PendingFrame {
    pub frame_id: u32,
    pub url: String,
    pub html: String,
    pub viewport_width: u64,
    pub viewport_height: u64,
    /// The frame that holds this one; 0 when the page does.
    pub parent_frame_id: u32,
}

/// One `postMessage` in flight between two realms.
pub struct PendingFrameMessage {
    /// Where it is going. 0 is the page's realm.
    pub target_frame_id: u32,
    /// Where it came from, so the receiver can reply through `event.source`.
    pub source_frame_id: u32,
    /// The sender's origin, for `event.origin`.
    pub origin: String,
    /// The payload, JSON encoded. Structured clone is not available across
    /// realms here, and JSON covers what postMessage is used for in practice:
    /// a widget reporting a result. Anything it cannot encode is rejected by
    /// the sender rather than silently arriving as null.
    pub data_json: String,
}

impl ObscuraState {
    pub fn new() -> Self {
        ObscuraState {
            dom: None,
            url: "about:blank".to_string(),
            encoding: "UTF-8".to_string(),
            title: String::new(),
            referrer: String::new(),
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
            pending_frames: Vec::new(),
            frame_id_counter: 0,
            frame_id: 0,
            pending_frame_messages: Vec::new(),
            pending_frame_message_bytes: 0,
            page_in_flight: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            activity_generation: 0,
            document_generation: 0,
            #[cfg(feature = "render")]
            prepared_render: None,
            #[cfg(feature = "render")]
            render_media: obscura_render::CssMediaType::Screen,
            #[cfg(feature = "render")]
            animation_sample: obscura_render::AnimationSample::default(),
            #[cfg(feature = "render")]
            animation_timeline: obscura_render::AnimationTimelineState::default(),
            #[cfg(feature = "render")]
            animation_timeline_origin: std::time::Instant::now(),
            #[cfg(feature = "render")]
            animation_task_generation: 0,
            #[cfg(feature = "render")]
            animation_sampled_task_generation: 0,
            #[cfg(feature = "render")]
            pending_style_mutations: Vec::new(),
            #[cfg(feature = "render")]
            render_resources: obscura_render::RenderResourceCache::default(),
            #[cfg(feature = "render")]
            render_image_in_flight: HashMap::new(),
            #[cfg(feature = "render")]
            stylesheet_cache: obscura_render::StylesheetCache::default(),
            #[cfg(feature = "render")]
            dynamic_fonts: Vec::new(),
            #[cfg(feature = "render")]
            canvas_surfaces: HashMap::new(),
            #[cfg(feature = "render")]
            viewport: (1280.0, 720.0),
            #[cfg(feature = "render")]
            scroll_offset: (0.0, 0.0),
            #[cfg(feature = "render")]
            element_scroll_offsets: HashMap::new(),
            #[cfg(feature = "render")]
            scroll_generation: 0,
            #[cfg(feature = "render")]
            resolved_scroll: None,
            import_map: Rc::new(RefCell::new(ImportMap::default())),
            already_started_scripts: RefCell::new(HashSet::new()),
        }
    }
}

pub(crate) fn node_is_script(dom: &DomTree, node_id: NodeId) -> bool {
    dom.with_node(node_id, |node| {
        node.as_element()
            .map(|name| name.local.as_ref().eq_ignore_ascii_case("script"))
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

fn script_nodes_including_template_contents(dom: &DomTree, root: NodeId) -> Vec<NodeId> {
    let mut scripts = Vec::new();
    let mut stack = vec![root];
    while let Some(node_id) = stack.pop() {
        if node_is_script(dom, node_id) {
            scripts.push(node_id);
        }
        let template_contents = dom
            .with_node(node_id, |node| match &node.data {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .flatten();
        if let Some(contents) = template_contents {
            stack.push(contents);
        }
        let children = dom.children(node_id);
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    scripts
}

pub(crate) fn mark_script_subtree_started(state: &ObscuraState, root: NodeId) {
    let Some(dom) = state.dom.as_ref() else {
        return;
    };
    let scripts = script_nodes_including_template_contents(dom, root);
    state.already_started_scripts.borrow_mut().extend(scripts);
}

fn propagate_script_start_state(
    dom: &DomTree,
    source_root: NodeId,
    cloned_root: NodeId,
    started: &RefCell<HashSet<NodeId>>,
) {
    let mut pairs = vec![(source_root, cloned_root)];
    let mut additions = Vec::new();
    let current = started.borrow();
    while let Some((source, cloned)) = pairs.pop() {
        if current.contains(&source) {
            additions.push(cloned);
        }

        let source_template = dom
            .with_node(source, |node| match &node.data {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .flatten();
        let cloned_template = dom
            .with_node(cloned, |node| match &node.data {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .flatten();
        if let (Some(source_contents), Some(cloned_contents)) = (source_template, cloned_template) {
            pairs.push((source_contents, cloned_contents));
        }

        let source_children = dom.children(source);
        let cloned_children = dom.children(cloned);
        for pair in source_children.into_iter().zip(cloned_children).rev() {
            pairs.push(pair);
        }
    }
    drop(current);
    started.borrow_mut().extend(additions);
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

/// Which document belongs to which realm.
///
/// An op has to read the state of the realm that *called* it. Making a realm
/// "current" around the host's own calls into it is not enough: a frame's
/// deferred work, a timer firing or a promise settling, re-enters JavaScript
/// from the event loop, where nothing had the chance to swap anything. Without
/// this a frame's `setTimeout` callback runs with the frame's globals but
/// writes to the *parent's* DOM.
#[derive(Default)]
pub struct RealmStates {
    entries: Vec<(v8::Global<v8::Context>, u32, SharedState)>,
}

impl RealmStates {
    pub fn register(
        &mut self,
        context: v8::Global<v8::Context>,
        frame_id: u32,
        state: SharedState,
    ) {
        self.entries.push((context, frame_id, state));
    }

    pub fn forget(&mut self, context: &v8::Global<v8::Context>) {
        self.entries.retain(|(known, _, _)| known != context);
    }

    fn by_frame_id(&self, frame_id: u32) -> Option<SharedState> {
        self.entries
            .iter()
            .find(|(_, id, _)| *id == frame_id)
            .map(|(_, _, state)| state.clone())
    }
}

/// The document of the realm a DOM call came from, named rather than inferred.
///
/// A wrapper's methods live on its own realm's prototypes, so the code running
/// for `parentPage.frameDoc.title` is the *frame's* getter even though the
/// caller is the page. Inferring the realm from the running context therefore
/// answers the wrong question for any cross-realm access, and would silently
/// read the page's document. Each realm's bootstrap closure knows its own frame
/// id and passes it, which is both correct here and cheaper than asking V8:
/// a page with no frames resolves on `frame_id == 0` alone.
pub fn frame_state(op_state: &OpState, frame_id: u32) -> SharedState {
    let page = || op_state.borrow::<SharedState>().clone();
    if frame_id == 0 {
        return page();
    }
    match op_state.try_borrow::<Rc<RefCell<RealmStates>>>() {
        Some(registry) => registry.borrow().by_frame_id(frame_id).unwrap_or_else(page),
        None => page(),
    }
}

/// The state of the realm running right now, or the page's when the caller is
/// the page itself.
///
/// A page with no frames pays only an `is_empty` check: looking up the current
/// context is not free, and `op_dom` is the hottest op in the system.
pub fn realm_state(scope: &mut v8::HandleScope, op_state: &OpState) -> SharedState {
    let page = || op_state.borrow::<SharedState>().clone();
    let registry = match op_state.try_borrow::<Rc<RefCell<RealmStates>>>() {
        Some(registry) => registry.clone(),
        None => return page(),
    };
    let registry = registry.borrow();
    if registry.entries.is_empty() {
        return page();
    }
    // Not `get_current_context`: an op is a native function bound in the page
    // realm, so V8 reports that realm as current no matter who called it. This
    // one answers "whose code is running", which is the question.
    let current = scope.get_entered_or_microtask_context();
    registry
        .entries
        .iter()
        .find(|(context, _, _)| *context == current)
        .map(|(_, _, state)| state.clone())
        .unwrap_or_else(page)
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderMutationImpact {
    connected: bool,
    actual_change: bool,
}

fn node_is_connected(dom: &DomTree, node: NodeId) -> bool {
    dom.is_connected(node)
}

#[cfg(feature = "render")]
fn shadow_including_connected_nodes(dom: &DomTree) -> HashSet<NodeId> {
    let mut connected = HashSet::new();
    let mut stack = vec![dom.document()];
    while let Some(node) = stack.pop() {
        if !connected.insert(node) {
            continue;
        }
        stack.extend(dom.children(node));
        if let Some(shadow_children) = dom.shadow_children(node) {
            stack.extend(shadow_children);
        }
    }
    connected
}

/// Classify whether a DOM command can make the retained document layout
/// stale. DOM construction is commonly performed in detached subtrees, and
/// frameworks also assign an attribute its current value. Neither operation
/// changes the rendered document. Chromium dirties layout when the mutation
/// reaches a connected style/layout owner, not merely because a mutating API
/// was entered.
fn render_mutation_impact(
    dom: &DomTree,
    cmd: &str,
    arg1: &str,
    arg2: &str,
) -> RenderMutationImpact {
    let node = |value: &str| value.parse::<u32>().ok().map(NodeId::new);
    match cmd {
        "set_attribute" => {
            let Some(target) = node(arg1) else {
                return RenderMutationImpact::default();
            };
            let Some((name, value)) = arg2.split_once('\0') else {
                return RenderMutationImpact::default();
            };
            let old = dom
                .with_node(target, |node| node.get_attribute(name).map(str::to_owned))
                .flatten();
            RenderMutationImpact {
                connected: node_is_connected(dom, target),
                actual_change: old.as_deref() != Some(value),
            }
        }
        "set_attribute_ns" => {
            let Some(target) = node(arg1) else {
                return RenderMutationImpact::default();
            };
            let mut parts = arg2.splitn(3, '\0');
            let namespace = parts.next().unwrap_or("");
            let qualified = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            let local = qualified
                .split_once(':')
                .map(|(_, local)| local)
                .unwrap_or(qualified);
            let old = dom
                .with_node(target, |node| {
                    node.get_attribute_ns(namespace, local).map(str::to_owned)
                })
                .flatten();
            RenderMutationImpact {
                connected: node_is_connected(dom, target),
                actual_change: old.as_deref() != Some(value),
            }
        }
        "remove_attribute" => {
            let Some(target) = node(arg1) else {
                return RenderMutationImpact::default();
            };
            let existed = dom
                .with_node(target, |node| node.get_attribute(arg2).is_some())
                .unwrap_or(false);
            RenderMutationImpact {
                connected: node_is_connected(dom, target),
                actual_change: existed,
            }
        }
        "remove_attribute_ns" => {
            let Some(target) = node(arg1) else {
                return RenderMutationImpact::default();
            };
            let (namespace, local) = arg2.split_once('\0').unwrap_or(("", arg2));
            let existed = dom
                .with_node(target, |node| {
                    node.get_attribute_ns(namespace, local).is_some()
                })
                .unwrap_or(false);
            RenderMutationImpact {
                connected: node_is_connected(dom, target),
                actual_change: existed,
            }
        }
        "append_child" => {
            let (Some(parent), Some(child)) = (node(arg1), node(arg2)) else {
                return RenderMutationImpact::default();
            };
            if dom.get_node(parent).is_none() || dom.get_node(child).is_none() {
                return RenderMutationImpact::default();
            }
            let old_parent = dom.get_node(child).and_then(|node| node.parent);
            let already_last =
                old_parent == Some(parent) && dom.children(parent).last().copied() == Some(child);
            RenderMutationImpact {
                // Moving a connected node into a detached subtree removes its
                // old box, while attaching a detached node creates a new one.
                connected: node_is_connected(dom, parent) || node_is_connected(dom, child),
                actual_change: !already_last,
            }
        }
        "remove_child" => {
            let Some(child) = node(arg1) else {
                return RenderMutationImpact::default();
            };
            RenderMutationImpact {
                connected: node_is_connected(dom, child),
                actual_change: dom.get_node(child).and_then(|node| node.parent).is_some(),
            }
        }
        "insert_before" => {
            let (Some(new_node), Some(reference)) = (node(arg1), node(arg2)) else {
                return RenderMutationImpact::default();
            };
            if dom.get_node(new_node).is_none() {
                return RenderMutationImpact::default();
            }
            let Some(reference_parent) = dom.get_node(reference).and_then(|node| node.parent)
            else {
                return RenderMutationImpact::default();
            };
            let new_was_connected = node_is_connected(dom, new_node);
            let already_immediately_before =
                dom.get_node(reference).and_then(|node| node.prev_sibling) == Some(new_node);
            RenderMutationImpact {
                connected: node_is_connected(dom, reference_parent) || new_was_connected,
                actual_change: new_node != reference && !already_immediately_before,
            }
        }
        "set_inner_html" | "set_inner_html_context" => {
            let Some(target) = node(arg1) else {
                return RenderMutationImpact::default();
            };
            RenderMutationImpact {
                connected: node_is_connected(dom, target),
                // Parsing normalizes source text, so a cheap string comparison
                // cannot prove equality. Connected replacement remains dirty.
                actual_change: dom.get_node(target).is_some(),
            }
        }
        "set_text_content" => {
            let Some(target) = node(arg1) else {
                return RenderMutationImpact::default();
            };
            let changed = dom
                .with_node(target, |node| match &node.data {
                    NodeData::Text { contents } | NodeData::Comment { contents } => {
                        contents.as_str() != arg2
                    }
                    NodeData::ProcessingInstruction { data, .. } => data.as_str() != arg2,
                    // Element/DocumentFragment textContent replaces their
                    // child structure, which can change style even when the
                    // flattened text is equal (for example `<b>x</b>` -> `x`).
                    _ => {
                        let children = dom.children(target);
                        match children.as_slice() {
                            [] => !arg2.is_empty(),
                            [child] => dom
                                .with_node(*child, |child| match &child.data {
                                    NodeData::Text { contents } => contents.as_str() != arg2,
                                    _ => true,
                                })
                                .unwrap_or(true),
                            _ => true,
                        }
                    }
                })
                .unwrap_or(false);
            RenderMutationImpact {
                connected: node_is_connected(dom, target),
                actual_change: changed,
            }
        }
        _ => RenderMutationImpact::default(),
    }
}

#[cfg(feature = "render")]
fn retained_style_mutation(
    dom: &DomTree,
    cmd: &str,
    arg1: &str,
    arg2: &str,
) -> Option<obscura_render::RetainedStyleMutation> {
    let node = NodeId::new(arg1.parse::<u32>().ok()?);
    // The retained planner and document stylesheet cache are intentionally
    // light-tree scoped. A mutation inside a connected shadow tree must still
    // invalidate rendering, but cannot be represented by that document-local
    // dirty set until scoped stylesheet invalidation is retained separately.
    if dom.containing_shadow_root(node).is_some() {
        return None;
    }
    match cmd {
        "set_attribute" => {
            let (name, value) = arg2.split_once('\0')?;
            if obscura_render::dom::retained_attribute_mutation_kind(dom, node, name)
                == obscura_render::dom::RetainedAttributeMutationKind::Full
            {
                return None;
            }
            let keeps_selector_value = !name.eq_ignore_ascii_case("style");
            Some(obscura_render::AttributeStyleMutation {
                node,
                name: name.to_string(),
                old_value: keeps_selector_value
                    .then(|| {
                        dom.with_node(node, |node| {
                            node.get_attribute(name).map(str::to_owned)
                        })
                        .flatten()
                    })
                    .flatten(),
                new_value: keeps_selector_value.then(|| value.to_string()),
            }
            .into())
        }
        "remove_attribute" => {
            if obscura_render::dom::retained_attribute_mutation_kind(dom, node, arg2)
                == obscura_render::dom::RetainedAttributeMutationKind::Full
            {
                return None;
            }
            let keeps_selector_value = !arg2.eq_ignore_ascii_case("style");
            Some(obscura_render::AttributeStyleMutation {
                node,
                name: arg2.to_string(),
                old_value: keeps_selector_value
                    .then(|| {
                        dom.with_node(node, |node| {
                            node.get_attribute(arg2).map(str::to_owned)
                        })
                        .flatten()
                    })
                    .flatten(),
                new_value: None,
            }
            .into())
        }
        "append_child" => {
            let child = NodeId::new(arg2.parse::<u32>().ok()?);
            dom.get_node(node)?;
            let old_parent = dom.get_node(child)?.parent;
            Some(
                obscura_render::TreeStyleMutation::Insert {
                    node: child,
                    old_parent,
                    new_parent: node,
                }
                .into(),
            )
        }
        "remove_child" => {
            let old_parent = dom.get_node(node)?.parent?;
            Some(
                obscura_render::TreeStyleMutation::Remove { node, old_parent }.into(),
            )
        }
        "insert_before" => {
            let reference = NodeId::new(arg2.parse::<u32>().ok()?);
            let new_parent = dom.get_node(reference)?.parent?;
            if dom.containing_shadow_root(new_parent).is_some() {
                return None;
            }
            let old_parent = dom.get_node(node)?.parent;
            Some(
                obscura_render::TreeStyleMutation::Insert {
                    node,
                    old_parent,
                    new_parent,
                }
                .into(),
            )
        }
        "set_text_content" => match &dom.get_node(node)?.data {
            NodeData::Text { .. } => Some(
                obscura_render::TreeStyleMutation::Text {
                    node,
                    parent: dom.get_node(node)?.parent,
                }
                .into(),
            ),
            // Element/fragment textContent replaces a child list. That can
            // flip :empty and structural/relational selectors, so the local
            // text fast path cannot describe the mutation safely.
            _ => None,
        },
        _ => None,
    }
}

#[cfg(feature = "render")]
// Modern hydration can touch thousands of distinct connected nodes before the
// first rendering opportunity. Keep a bounded safety valve for adversarial
// churn, but do not force a whole-document cascade at the scale of an ordinary
// React/Framer commit.
const MAX_PENDING_STYLE_MUTATIONS: usize = 4_096;

/// Queue one retained-style invalidation without letting animation frameworks
/// evict the whole prepared render merely because they rewrite the same inline
/// style more than once before the next rendering opportunity.
///
/// Rendering observes the attribute state at flush boundaries. Repeated writes
/// to the same node/name therefore retain the first old value and final new
/// value; intermediate values were never rendered and cannot affect selector
/// matching. Inline style uses the same rule without storing serialized values.
#[cfg(feature = "render")]
pub(crate) fn queue_retained_style_mutation(
    pending: &mut Vec<obscura_render::RetainedStyleMutation>,
    mutation: obscura_render::RetainedStyleMutation,
) -> bool {
    let is_resource = matches!(mutation, obscura_render::RetainedStyleMutation::Resource);
    let has_resource = pending
        .iter()
        .any(|queued| matches!(queued, obscura_render::RetainedStyleMutation::Resource));
    if is_resource && has_resource {
        return true;
    }
    if let obscura_render::RetainedStyleMutation::Animation { node } = &mutation {
        if pending.iter().any(|queued| {
            matches!(
                queued,
                obscura_render::RetainedStyleMutation::Animation { node: current }
                    if current == node
            )
        }) {
            return true;
        }
    }
    if let obscura_render::RetainedStyleMutation::WaapiAnimation { node } = &mutation {
        if pending.iter().any(|queued| {
            matches!(
                queued,
                obscura_render::RetainedStyleMutation::WaapiAnimation { node: current }
                    if current == node
            )
        }) {
            return true;
        }
    }
    if let obscura_render::RetainedStyleMutation::Attribute(next) = &mutation {
        if let Some(obscura_render::RetainedStyleMutation::Attribute(current)) =
            pending.iter_mut().find(|queued| {
                matches!(
                    queued,
                    obscura_render::RetainedStyleMutation::Attribute(current)
                        if current.node == next.node
                            && current.name.eq_ignore_ascii_case(&next.name)
                )
            })
        {
            current.new_value.clone_from(&next.new_value);
            return true;
        }
    }

    // Resource refresh is a singleton trigger, not style damage. Keep the
    // bounded safety limit on actual selector/tree/animation invalidations
    // without making a late image discard an exactly-full retained batch.
    let style_damage_len = pending.len() - usize::from(has_resource);
    if !is_resource && style_damage_len >= MAX_PENDING_STYLE_MUTATIONS {
        return false;
    }
    pending.push(mutation);
    true
}

/// Rebuild resource-dependent geometry while retaining the previous computed
/// style graph. Image intrinsic sizes and font metrics can reflow the whole
/// document, but neither changes selector matching or computed declarations.
/// Coalescing this marker also makes one shared image response invalidate once
/// rather than once for every HTMLImageElement waiter.
#[cfg(feature = "render")]
pub(crate) fn invalidate_render_resource_geometry(state: &mut ObscuraState) {
    if state.prepared_render.is_some()
        && !queue_retained_style_mutation(
            &mut state.pending_style_mutations,
            obscura_render::RetainedStyleMutation::Resource,
        )
    {
        state.prepared_render = None;
        state.pending_style_mutations.clear();
    }
    state.resolved_scroll = None;
}

#[cfg(feature = "render")]
fn render_timing_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("OBSCURA_RENDER_TIMING").is_some())
}

#[cfg(feature = "render")]
fn is_render_mutation_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "set_attribute"
            | "remove_attribute"
            | "set_attribute_ns"
            | "remove_attribute_ns"
            | "append_child"
            | "remove_child"
            | "insert_before"
            | "set_inner_html"
            | "set_inner_html_context"
            | "set_text_content"
    )
}

fn fragment_context_and_html(arg: &str) -> (html5ever::QualName, &str) {
    let mut parts = arg.splitn(3, '\0');
    let first = parts.next().unwrap_or("body");
    let second = parts.next();
    let third = parts.next();
    let (namespace, qualified, html) = match (second, third) {
        // Namespace-aware encoding used by the current bootstrap.
        (Some(qualified), Some(html)) => (first, qualified, html),
        // Backward-compatible encoding for older snapshots: `local\0html`.
        (Some(html), None) => ("http://www.w3.org/1999/xhtml", first, html),
        (None, None) => ("http://www.w3.org/1999/xhtml", "body", first),
        (None, Some(_)) => unreachable!(),
    };
    let (prefix, local) = match qualified.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => {
            (Some(html5ever::Prefix::from(prefix)), local)
        }
        _ => (None, if qualified.is_empty() { "body" } else { qualified }),
    };
    (
        html5ever::QualName::new(
            prefix,
            html5ever::Namespace::from(namespace),
            html5ever::LocalName::from(local),
        ),
        html,
    )
}

#[op2(fast)]
fn op_script_mark_started(state: &OpState, nid: u32) -> bool {
    let shared = state.borrow::<SharedState>().clone();
    let state = shared.borrow();
    let Some(dom) = state.dom.as_ref() else {
        return false;
    };
    let node_id = NodeId::new(nid);
    if !node_is_script(dom, node_id) {
        return false;
    }
    state.already_started_scripts.borrow_mut().insert(node_id);
    true
}

/// Atomically claim an executable script.  A false result means the node was
/// created inert by an HTML-string API or has already been prepared once.
#[op2(fast)]
fn op_script_try_start(state: &OpState, nid: u32) -> bool {
    let shared = state.borrow::<SharedState>().clone();
    let state = shared.borrow();
    let Some(dom) = state.dom.as_ref() else {
        return false;
    };
    let node_id = NodeId::new(nid);
    if !node_is_script(dom, node_id) {
        return false;
    }
    let newly_started = state.already_started_scripts.borrow_mut().insert(node_id);
    newly_started
}

/// Attach one native shadow-tree scope without making it part of the light
/// tree. Layout intentionally remains unaware of the detached root until
/// scoped style, slot assignment, and composed-tree paint are implemented.
#[op2(fast)]
fn op_shadow_attach(state: &OpState, host_nid: u32, #[string] mode: String) -> i32 {
    let mode = match mode.as_str() {
        "open" => ShadowRootMode::Open,
        "closed" => ShadowRootMode::Closed,
        _ => return -1,
    };
    let shared = state.borrow::<SharedState>().clone();
    let state = shared.borrow();
    let Some(dom) = state.dom.as_ref() else {
        return -1;
    };
    match dom.attach_shadow_root(NodeId::new(host_nid), mode) {
        Ok(root) => root.raw() as i32,
        Err(AttachShadowError::HostAlreadyHasShadowRoot) => -2,
        Err(_) => -1,
    }
}

/// Return native host-owned shadow identity as `root-id\0mode`. Closed roots
/// are included here; the Web-facing `Element.shadowRoot` getter applies mode
/// visibility in bootstrap.js.
#[op2]
#[string]
fn op_shadow_root_info(state: &OpState, host_nid: u32) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let state = shared.borrow();
    let Some(dom) = state.dom.as_ref() else {
        return String::new();
    };
    dom.shadow_root(NodeId::new(host_nid))
        .and_then(|root| dom.shadow_root_info(root))
        .map(|shadow| {
            let mode = match shadow.mode {
                ShadowRootMode::Open => "open",
                ShadowRootMode::Closed => "closed",
            };
            format!("{}\0{mode}", shadow.id.raw())
        })
        .unwrap_or_default()
}

#[op2]
#[string]
fn op_dom(
    state: &OpState,
    #[string] cmd: String,
    #[string] arg1: String,
    #[string] arg2: String,
    frame_id: u32,
) -> String {
    let shared = frame_state(state, frame_id);
    // Anti-panic boundary: a panic in a DOM op would unwind through deno_core
    // into V8's FFI frame, where V8_Fatal calls abort(3) and takes the whole
    // engine (and every CDP client) down. Catch it so one malformed selector or
    // inconsistent tree node degrades to a null result for that single call.
    // No per-call clone: on the happy path this is just a landing pad, so the
    // hot DOM path (querySelector/getAttribute/...) pays nothing measurable.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        op_dom_inner(shared, cmd, arg1, arg2)
    }))
    .unwrap_or_else(|_| {
        tracing::error!("op_dom panicked; returning null");
        "null".to_string()
    })
}

fn op_dom_inner(shared: SharedState, cmd: String, arg1: String, arg2: String) -> String {
    {
        // Scroll offsets belong to a node at its current tree position.
        // Temporary box/style loss keeps that latent state, but DOM removal,
        // reparenting, and subtree replacement reset the affected identities,
        // matching Chromium's lifecycle behavior.
        #[cfg(feature = "render")]
        let reset_nodes = {
            let state = shared.borrow();
            let mut roots = Vec::new();
            if let Some(dom) = state.dom.as_ref() {
                match cmd.as_str() {
                    "remove_child" => {
                        if let Ok(node) = arg1.parse::<u32>() {
                            roots.push(NodeId::new(node));
                        }
                    }
                    "append_child" => {
                        if let Ok(node) = arg2.parse::<u32>() {
                            let node = NodeId::new(node);
                            if dom.get_node(node).and_then(|node| node.parent).is_some() {
                                roots.push(node);
                            }
                        }
                    }
                    "insert_before" => {
                        if let Ok(node) = arg1.parse::<u32>() {
                            let node = NodeId::new(node);
                            if dom.get_node(node).and_then(|node| node.parent).is_some() {
                                roots.push(node);
                            }
                        }
                    }
                    "set_inner_html" | "set_inner_html_context" | "set_text_content" => {
                        if let Ok(node) = arg1.parse::<u32>() {
                            roots.extend(dom.children(NodeId::new(node)));
                        }
                    }
                    _ => {}
                }
                roots
                    .into_iter()
                    .flat_map(|root| {
                        let mut nodes = vec![root];
                        nodes.extend(dom.descendants(root));
                        nodes
                    })
                    .collect::<HashSet<_>>()
            } else {
                HashSet::new()
            }
        };
        // Any changed attribute on a connected node can participate in an
        // author selector. Detached subtree construction, failed operations,
        // and no-op value assignments cannot change live layout and preserve
        // the prepared render. The next relevant mutation invalidates once;
        // subsequent writes are coalesced until geometry is read again.
        let mut state = shared.borrow_mut();
        let impact = state
            .dom
            .as_ref()
            .map(|dom| render_mutation_impact(dom, &cmd, &arg1, &arg2))
            .unwrap_or_default();
        #[cfg(feature = "render")]
        let retained_style_mutation = state
            .dom
            .as_ref()
            .and_then(|dom| retained_style_mutation(dom, &cmd, &arg1, &arg2));
        let invalidate = impact.connected && impact.actual_change;
        if invalidate {
            state.activity_generation = state.activity_generation.wrapping_add(1);
        }
        #[cfg(feature = "render")]
        if !reset_nodes.is_empty() {
            state
                .element_scroll_offsets
                .retain(|node, _| !reset_nodes.contains(node));
            state.scroll_generation = state.scroll_generation.wrapping_add(1);
            if invalidate {
                state.animation_timeline.remove_subtree(reset_nodes.iter());
            }
        }
        #[cfg(feature = "render")]
        let had_prepared_render = state.prepared_render.is_some();
        #[cfg(feature = "render")]
        if invalidate {
            let mutation_time_ms = (state.animation_timeline_origin.elapsed().as_secs_f64()
                * 1_000.0)
                .min(f64::from(f32::MAX)) as f32;
            // Keep animation birth epochs local to the changed subtree. A
            // single document-global timestamp made a later unrelated write
            // restart every not-yet-sampled animation at the same instant.
            let direct_root = match cmd.as_str() {
                "append_child" => arg2.parse::<u32>().ok(),
                "insert_before"
                | "set_attribute"
                | "remove_attribute"
                | "set_attribute_ns"
                | "remove_attribute_ns" => arg1.parse::<u32>().ok(),
                _ => None,
            }
            .map(NodeId::new);
            let direct_nodes = direct_root
                .and_then(|root| {
                    state.dom.as_ref().map(|dom| {
                        std::iter::once(root)
                            .chain(dom.descendants(root))
                            .collect::<Vec<_>>()
                    })
                })
                .unwrap_or_default();
            for node in direct_nodes {
                state
                    .animation_timeline
                    .note_start_candidate(node, mutation_time_ms);
            }
            let scope_root = match cmd.as_str() {
                "append_child" => arg1.parse::<u32>().ok().map(NodeId::new),
                "insert_before" => arg2
                    .parse::<u32>()
                    .ok()
                    .map(NodeId::new)
                    .and_then(|reference| {
                        state.dom.as_ref()?.get_node(reference)?.parent
                    }),
                "remove_child" => arg1
                    .parse::<u32>()
                    .ok()
                    .map(NodeId::new)
                    .and_then(|child| state.dom.as_ref()?.get_node(child)?.parent),
                "set_inner_html" | "set_inner_html_context" | "set_text_content" => {
                    arg1.parse::<u32>().ok().map(NodeId::new)
                }
                _ => None,
            };
            if let Some(root) = scope_root {
                state
                    .animation_timeline
                    .note_subtree_start_candidate(root, mutation_time_ms);
            }
            if let Some(mutation) = retained_style_mutation {
                let retained = state.prepared_render.is_some()
                    && queue_retained_style_mutation(
                        &mut state.pending_style_mutations,
                        mutation,
                    );
                if !retained {
                    state.prepared_render = None;
                    state.pending_style_mutations.clear();
                }
            } else {
                state.prepared_render = None;
                state.pending_style_mutations.clear();
            }
            state.resolved_scroll = None;
        }
        #[cfg(feature = "render")]
        if had_prepared_render && is_render_mutation_command(&cmd) && render_timing_enabled() {
            static MUTATION_SEQUENCE: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            let sequence = MUTATION_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let detail = match cmd.as_str() {
                "set_attribute" => arg2.split_once('\0').map(|(name, _)| name).unwrap_or(""),
                "remove_attribute" => arg2.as_str(),
                _ => "",
            };
            eprintln!(
                "[timing] render-cache mutation sequence={} cmd={} node={} detail={} connected={} actual_change={} invalidated={}",
                sequence, cmd, arg1, detail, impact.connected, impact.actual_change, invalidate
            );
        }
    }
    let gs = shared.borrow();
    let dom = match &gs.dom {
        Some(d) => d,
        None => return "null".to_string(),
    };

    match cmd.as_str() {
        "document_node_id" => dom.document().index().to_string(),
        "document_title" => {
            // The DOM is authoritative after parsing. In particular, script
            // changes through title.textContent must be reflected by
            // document.title, not hidden behind the navigation-time snapshot.
            let title = dom
                .query_selector("title")
                .ok()
                .flatten()
                .map(|title_id| {
                    dom.text_content(title_id)
                        .split(|ch| matches!(ch, '\t' | '\n' | '\u{000C}' | '\r' | ' '))
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            serde_json::to_string(&title).unwrap_or("\"\"".into())
        }
        "document_url" => serde_json::to_string(&gs.url).unwrap_or("\"\"".into()),
        "document_referrer" => serde_json::to_string(&gs.referrer).unwrap_or("\"\"".into()),
        "document_encoding" => serde_json::to_string(&gs.encoding).unwrap_or("\"UTF-8\"".into()),
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
                    let sel = format!(
                        "[id=\"{}\"]",
                        arg1.replace('\\', "\\\\").replace('"', "\\\"")
                    );
                    dom.query_selector(&sel)
                        .ok()
                        .flatten()
                        .map(|id| id.index().to_string())
                        .unwrap_or("-1".into())
                }
            }
        }
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
        "query_selector_scoped" => {
            let root_nid = arg1.parse::<u32>().unwrap_or(0);
            dom.query_selector_from(NodeId::new(root_nid), &arg2)
                .ok()
                .flatten()
                .map(|id| id.index().to_string())
                .unwrap_or("-1".into())
        }
        "query_selector_all_scoped" => {
            let root_nid = arg1.parse::<u32>().unwrap_or(0);
            let ids: Vec<i32> = dom
                .query_selector_all_from(NodeId::new(root_nid), &arg2)
                .ok()
                .map(|ids| ids.iter().map(|id| id.index() as i32).collect())
                .unwrap_or_default();
            serde_json::to_string(&ids).unwrap_or("[]".into())
        }
        "matches_selector" => {
            let nid = NodeId::new(arg1.parse::<u32>().unwrap_or(0));
            dom.matches_selector(nid, &arg2)
                .unwrap_or(false)
                .to_string()
        }
        "node_type" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node(NodeId::new(nid), |n| match &n.data {
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
                .with_node(NodeId::new(nid), |n| match &n.data {
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
            dom.with_node(NodeId::new(nid), |n| match cmd.as_str() {
                "parent_node" => n.parent,
                "first_child" => n.first_child,
                "last_child" => n.last_child,
                "next_sibling" => n.next_sibling,
                "prev_sibling" => n.prev_sibling,
                _ => None,
            })
            .flatten()
            .map(|id| id.index().to_string())
            .unwrap_or("-1".into())
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
                .with_node(NodeId::new(nid), |n| {
                    n.as_element().map(|name| {
                        if name.ns == html5ever::ns!(html) {
                            name.local.as_ref().to_ascii_uppercase()
                        } else {
                            match &name.prefix {
                                Some(prefix) => format!("{}:{}", prefix, name.local),
                                None => name.local.to_string(),
                            }
                        }
                    })
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        "local_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let name = dom
                .with_node(NodeId::new(nid), |n| {
                    n.as_element().map(|name| name.local.to_string())
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&name).unwrap_or("\"\"".into())
        }
        // The tree builder already assigns foreign content (an <svg>/<math>
        // subtree) its own namespace; expose it so JS does not have to guess
        // the namespace from the tag name.
        "namespace_uri" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let ns = dom
                .with_node(NodeId::new(nid), |n| {
                    n.as_element().map(|name| name.ns.as_ref().to_string())
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&ns).unwrap_or("\"\"".into())
        }
        "get_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| {
                    n.get_attribute(&arg2).map(|s| s.to_string())
                })
                .flatten();
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "attribute_names" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let names: Vec<String> = dom
                .with_node(NodeId::new(nid), |n| {
                    n.attrs()
                        .map(|a| a.iter().map(|x| x.qualified_name()).collect())
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
                    let old_id = dom
                        .with_node(node_id, |n| n.get_attribute("id").map(|s| s.to_string()))
                        .flatten();
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
            let parent = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let child = match arg2.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let parent = NodeId::new(parent);
            let child = NodeId::new(child);
            dom.append_child(parent, child);
            (dom.get_node(child).and_then(|node| node.parent) == Some(parent)).to_string()
        }
        "remove_child" => {
            let child = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let child = NodeId::new(child);
            let had_parent = dom.get_node(child).is_some_and(|node| node.parent.is_some());
            dom.remove_child(child);
            (had_parent && dom.get_node(child).is_some_and(|node| node.parent.is_none())).to_string()
        }
        "insert_before" => {
            let new_node = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let ref_node = match arg2.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "false".into(),
            };
            let ref_node = NodeId::new(ref_node);
            let new_node = NodeId::new(new_node);
            let expected_parent = dom.get_node(ref_node).and_then(|node| node.parent);
            dom.insert_before(ref_node, new_node);
            (expected_parent.is_some()
                && dom.get_node(new_node).and_then(|node| node.parent) == expected_parent)
                .to_string()
        }
        "remove_attribute" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.with_node_mut(NodeId::new(nid), |n| {
                if let NodeData::Element { attrs, .. } = &mut n.data {
                    attrs.retain(|a| !a.qualified_name_eq(&arg2));
                }
            });
            "true".into()
        }
        // Namespace-aware attribute ops. arg2 packs the pieces with a NUL:
        //   get/remove: "<namespace>\0<localName>"
        //   set:        "<namespace>\0<qualifiedName>\0<value>"
        "get_attribute_ns" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let (ns, local) = arg2.split_once('\0').unwrap_or(("", arg2.as_str()));
            let val = dom
                .with_node(NodeId::new(nid), |n| n.get_attribute_ns(ns, local).map(|s| s.to_string()))
                .flatten();
            serde_json::to_string(&val).unwrap_or("null".into())
        }
        "set_attribute_ns" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            let mut parts = arg2.splitn(3, '\0');
            let ns = parts.next().unwrap_or("");
            let qualified = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            if !qualified.is_empty() {
                let local = qualified
                    .split_once(':')
                    .map(|(_, local)| local)
                    .unwrap_or(qualified);
                if ns.is_empty() && local == "id" {
                    let old_id = dom
                        .with_node(node_id, |n| n.get_attribute("id").map(str::to_owned))
                        .flatten();
                    dom.with_node_mut(node_id, |n| {
                        n.set_attribute_ns(ns, qualified, value.to_string())
                    });
                    dom.update_id_index(node_id, old_id.as_deref(), Some(value));
                } else {
                    dom.with_node_mut(node_id, |n| {
                        n.set_attribute_ns(ns, qualified, value.to_string())
                    });
                }
            }
            "true".into()
        }
        "remove_attribute_ns" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let node_id = NodeId::new(nid);
            let (ns, local) = arg2.split_once('\0').unwrap_or(("", arg2.as_str()));
            if ns.is_empty() && local == "id" {
                let old_id = dom
                    .with_node(node_id, |n| n.get_attribute("id").map(str::to_owned))
                    .flatten();
                dom.with_node_mut(node_id, |n| n.remove_attribute_ns(ns, local));
                dom.update_id_index(node_id, old_id.as_deref(), None);
            } else {
                dom.with_node_mut(node_id, |n| n.remove_attribute_ns(ns, local));
            }
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
                let context_name = dom
                    .with_node(target, |node| match &node.data {
                        NodeData::Element { name, .. } => Some(name.clone()),
                        _ => None,
                    })
                    .flatten();
                let fragment = match context_name {
                    Some(name) => obscura_dom::parse_fragment_with_context(&arg2, name),
                    None => obscura_dom::parse_fragment(&arg2),
                };
                let import_root = fragment.fragment_root();
                dom.import_children_from(target, &fragment, import_root);
                for child in dom.children(target) {
                    mark_script_subtree_started(&gs, child);
                }
            }
            "true".into()
        }
        "set_inner_html_context" => {
            let nid = match arg1.parse::<u32>() {
                Ok(n) if n > 0 => n,
                _ => return "false".into(),
            };
            let target = NodeId::new(nid);
            let (context_name, html) = fragment_context_and_html(&arg2);
            for child in dom.children(target) {
                dom.detach(child);
            }
            if !html.is_empty() {
                let fragment = obscura_dom::parse_fragment_with_context(html, context_name);
                let import_root = fragment.fragment_root();
                dom.import_children_from(target, &fragment, import_root);
                for child in dom.children(target) {
                    mark_script_subtree_started(&gs, child);
                }
            }
            "true".into()
        }
        // Range.createContextualFragment has a deliberately different script
        // policy from innerHTML: scripts remain eligible and are prepared when
        // the returned fragment is inserted into a connected document.
        "set_fragment_html_executable" => {
            let nid = match arg1.parse::<u32>() {
                Ok(n) if n > 0 => n,
                _ => return "false".into(),
            };
            let target = NodeId::new(nid);
            let (context_name, html) = fragment_context_and_html(&arg2);
            for child in dom.children(target) {
                dom.detach(child);
            }
            if !html.is_empty() {
                let fragment = obscura_dom::parse_fragment_with_context(html, context_name);
                let import_root = fragment.fragment_root();
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
                NodeData::ProcessingInstruction { data, .. } => {
                    *data = arg2.clone();
                }
                _ => {}
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
        "create_document_fragment" => dom.new_node(NodeData::Document).index().to_string(),
        "clone_node" => {
            let nid = match arg1.parse::<u32>() {
                Ok(n) => n,
                Err(_) => return "-1".into(),
            };
            let source = NodeId::new(nid);
            match dom.clone_node(source, arg2 == "true") {
                Some(cloned) => {
                    propagate_script_start_state(dom, source, cloned, &gs.already_started_scripts);
                    cloned.index().to_string()
                }
                None => "-1".into(),
            }
        }
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
        "create_element_ns" => {
            let (namespace, qualified) = arg1.split_once('\0').unwrap_or(("", arg1.as_str()));
            let (prefix, local) = match qualified.split_once(':') {
                Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => {
                    (Some(html5ever::Prefix::from(prefix)), local)
                }
                None if !qualified.is_empty() => (None, qualified),
                _ => return "-1".into(),
            };
            dom.new_node(NodeData::Element {
                name: html5ever::QualName::new(
                    prefix,
                    html5ever::Namespace::from(namespace),
                    html5ever::LocalName::from(local),
                ),
                attrs: vec![],
                template_contents: None,
                mathml_annotation_xml_integration_point: false,
            })
            .index()
            .to_string()
        }
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
        "create_processing_instruction" => {
            // arg1 = target, arg2 = data
            dom.new_node(NodeData::ProcessingInstruction {
                target: arg1.clone(),
                data: arg2.clone(),
            })
            .index()
            .to_string()
        }
        "create_doctype" => {
            // arg1 = name, arg2 = public_id. system_id stored only in the
            // JS wrapper since neither current WPT test reads it back from
            // the underlying tree.
            dom.new_node(NodeData::Doctype {
                name: arg1.clone(),
                public_id: arg2.clone(),
                system_id: String::new(),
            })
            .index()
            .to_string()
        }
        "pi_target" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| match &n.data {
                    NodeData::ProcessingInstruction { target, .. } => Some(target.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
        "doctype_name" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| match &n.data {
                    NodeData::Doctype { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
        "doctype_public_id" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            let val = dom
                .with_node(NodeId::new(nid), |n| match &n.data {
                    NodeData::Doctype { public_id, .. } => Some(public_id.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            serde_json::to_string(&val).unwrap_or("\"\"".into())
        }
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
            dom.with_node(NodeId::new(nid), |n| n.first_child.is_some())
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
        // Connectivity is maintained incrementally by DomTree. Exposing the
        // cached bit avoids an ancestor op crossing for every level when JS
        // builds a deep detached subtree.
        "is_connected" => {
            let nid = arg1.parse::<u32>().unwrap_or(0);
            dom.is_connected(NodeId::new(nid)).to_string()
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

// Fallback cache for runtimes that have no owning ObscuraHttpClient, such as
// a standalone module loader. Browser pages use their context-scoped client
// below so sequential V8 runtimes never share an async network pool (#453).
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
    let cache =
        FETCH_CLIENT_CACHE.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()));
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
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(fetch_timeout())
        // SSRF guard: also reject hostnames that resolve to a private/loopback IP.
        .dns_resolver(std::sync::Arc::new(obscura_net::SsrfGuardResolver::new(
            false,
        )))
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

fn fetch_timeout() -> std::time::Duration {
    let timeout_ms = std::env::var("OBSCURA_FETCH_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000);
    std::time::Duration::from_millis(timeout_ms)
}

/// Cap on the number of redirect hops op_fetch_url will follow.
/// Matches reqwest's default policy of 10.
const FETCH_REDIRECT_LIMIT: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FetchCredentials {
    Omit,
    SameOrigin,
    Include,
}

impl FetchCredentials {
    fn parse(value: &str) -> Self {
        match value {
            "omit" => Self::Omit,
            "include" => Self::Include,
            _ => Self::SameOrigin,
        }
    }

    fn allows(self, page_origin: &str, request_url: &str) -> bool {
        match self {
            Self::Omit => false,
            Self::Include => true,
            Self::SameOrigin => request_origin(request_url)
                .map(|origin| origin == page_origin)
                .unwrap_or(false),
        }
    }
}

fn request_origin(request_url: &str) -> Option<String> {
    url::Url::parse(request_url)
        .ok()
        .map(|url| url.origin().ascii_serialization())
}

fn cors_response_allows(
    credentials: FetchCredentials,
    page_origin: &str,
    allowed_origin: &str,
    allow_credentials: &str,
) -> bool {
    if credentials == FetchCredentials::Include {
        allowed_origin == page_origin && allow_credentials == "true"
    } else {
        allowed_origin == "*" || allowed_origin == page_origin
    }
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
    #[string] credentials: String,
) -> Result<String, deno_error::JsErrorBox> {
    tracing::debug!(
        "op_fetch_url called: {} {} (intercept check pending)",
        method,
        url
    );

    let (cookie_jar, in_flight, page_in_flight, intercept_tx, proxy_url, callbacks, http_client) = {
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
        // Record the resource the page pulled in via fetch()/XHR so `--dump
        // assets` can list it (issue #301). URL is already absolute here, since
        // reqwest needs an absolute URL to send the request.
        gs.fetched_urls.push(url.clone());
        let jar = gs.cookie_jar.clone();
        let in_flight = gs.http_client.as_ref().map(|c| c.in_flight.clone());
        // #139: thread the configured proxy through to the per-request
        // reqwest::Client. Without this, op_fetch_url silently bypasses
        // BrowserContext.proxy_url for every JS fetch() / XHR call.
        let proxy_url = gs
            .http_client
            .as_ref()
            .and_then(|c| c.proxy_url().map(|s| s.to_string()));
        tracing::debug!(
            "op_fetch_url: intercept_enabled={}, has_tx={}",
            gs.intercept_enabled,
            gs.intercept_tx.is_some()
        );
        let itx = if gs.intercept_enabled {
            gs.intercept_counter += 1;
            gs.intercept_tx
                .clone()
                .map(|tx| (tx, format!("intercept-{}", gs.intercept_counter)))
        } else {
            None
        };
        (
            jar,
            in_flight,
            Arc::clone(&gs.page_in_flight),
            itx,
            proxy_url,
            gs.callbacks.clone(),
            gs.http_client.clone(),
        )
    };
    // The private-network opt-in is a BrowserContext policy, not only a
    // process-wide environment setting.  Navigation already honours the
    // context's configured HTTP client; scripted fetch/XHR must use the same
    // policy for its initial URL and every URL it can reach below.
    let allow_private_network = http_client
        .as_ref()
        .is_some_and(|client| client.allow_private_network);
    if let Ok(parsed_url) = url::Url::parse(&url) {
        if let Err(e) = validate_fetch_url(&parsed_url, allow_private_network) {
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
    struct PageInFlightGuard(Arc<std::sync::atomic::AtomicU32>);
    impl Drop for PageInFlightGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    page_in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _page_in_flight = PageInFlightGuard(page_in_flight);

    // Slots the interception channel can override via Continue so a consumer
    // can rewrite url/method/headers/body before the request goes out.
    let mut override_url: Option<String> = None;
    let mut override_method: Option<String> = None;
    let mut override_headers: Option<HashMap<String, String>> = None;
    let mut override_body: Option<String> = None;

    if let Some((tx, request_id)) = intercept_tx {
        let custom_headers: HashMap<String, String> =
            serde_json::from_str(&headers_json).unwrap_or_default();
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
                Ok(InterceptResolution::Continue {
                    url,
                    method,
                    headers,
                    body,
                }) => {
                    override_url = url;
                    override_method = method;
                    override_headers = headers;
                    override_body = body;
                    tracing::debug!(
                        "Interception: continue (overrides url={} method={} headers={} body={})",
                        override_url.is_some(),
                        override_method.is_some(),
                        override_headers.is_some(),
                        override_body.is_some()
                    );
                }
                Err(_) => {}
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
            if let Err(reason) = validate_fetch_url(&parsed, allow_private_network) {
                return Ok(serde_json::json!({
                    "status": 0,
                    "body": "",
                    "url": new_url,
                    "blocked": true,
                    "error": format!("Intercept rewrite to forbidden URL blocked: {}", reason),
                })
                .to_string());
            }
        }
        new_url
    } else {
        url
    };
    let method = override_method.unwrap_or(method);
    let body = override_body.unwrap_or(body);

    let client = match &http_client {
        Some(client) => client.request_client().await,
        None => {
            cached_request_client(proxy_url.as_deref()).map_err(deno_error::JsErrorBox::generic)?
        }
    };

    let initial_request_origin = request_origin(&url).unwrap_or_default();
    let page_origin = if origin.is_empty() {
        initial_request_origin.clone()
    } else {
        origin.clone()
    };
    let is_cross_origin = !page_origin.is_empty() && initial_request_origin != page_origin;
    let credentials = FetchCredentials::parse(&credentials);

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

    let needs_preflight = is_cross_origin
        && mode == "cors"
        && (req_method != reqwest::Method::GET
            && req_method != reqwest::Method::HEAD
            && req_method != reqwest::Method::POST
            || custom_headers.keys().any(|k| {
                let kl = k.to_lowercase();
                kl != "accept"
                    && kl != "accept-language"
                    && kl != "content-language"
                    && kl != "content-type"
            }));

    if needs_preflight {
        let preflight = client
            .request(reqwest::Method::OPTIONS, &url)
            .timeout(fetch_timeout())
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

        let allow_credentials = preflight
            .headers()
            .get("access-control-allow-credentials")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !cors_response_allows(credentials, &page_origin, allowed_origin, allow_credentials) {
            return Err(deno_error::JsErrorBox::generic(format!(
                "CORS preflight: Origin '{}' not allowed by Access-Control-Allow-Origin '{}'",
                page_origin, allowed_origin
            )));
        }
    }

    // Stealth mode: route scripted requests through wreq after the CORS
    // preflight. stealth_fetch_all applies the credentials decision to each
    // redirect hop without losing the Chrome TLS/client-hint transport.
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
                mode.clone(),
                credentials,
                callbacks.clone(),
                allow_private_network,
            )
            .await;
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
        let mut req = client
            .request(current_method.clone(), &current_url)
            .timeout(fetch_timeout());

        let current_is_cross_origin = request_origin(&current_url)
            .map(|request_origin| request_origin != page_origin)
            .unwrap_or(false);
        if current_is_cross_origin {
            req = req.header("Origin", &page_origin);
        }

        let credentials_allowed = credentials.allows(&page_origin, &current_url);
        if credentials_allowed {
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
        if !custom_headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("user-agent"))
        {
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

        if credentials_allowed {
            if let Some(ref jar) = cookie_jar {
                if let Ok(parsed_url) = url::Url::parse(&current_url) {
                    for val in resp.headers().get_all(reqwest::header::SET_COOKIE) {
                        if let Ok(s) = val.to_str() {
                            jar.set_cookie(s, &parsed_url);
                        }
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
        if let Err(reason) = validate_fetch_url(&next_url, allow_private_network) {
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

    let final_is_cross_origin = request_origin(&current_url)
        .map(|request_origin| request_origin != page_origin)
        .unwrap_or(false);
    if final_is_cross_origin && mode == "cors" {
        let allowed = resp_headers
            .get("access-control-allow-origin")
            .map(|s| s.as_str())
            .unwrap_or("");

        let allow_credentials = resp_headers
            .get("access-control-allow-credentials")
            .map(|s| s.as_str())
            .unwrap_or("");
        if !cors_response_allows(credentials, &page_origin, allowed, allow_credentials) {
            return Ok(serde_json::json!({
                "status": 0,
                "body": "",
                "url": url,
                "headers": {},
                "corsBlocked": true,
                "corsError": if credentials == FetchCredentials::Include {
                    format!(
                        "CORS error: credentialed request requires Access-Control-Allow-Origin '{}' and Access-Control-Allow-Credentials 'true'",
                        page_origin
                    )
                } else {
                    format!("CORS error: Origin '{}' not in Access-Control-Allow-Origin '{}'", page_origin, allowed)
                },
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
        "requestId": response_request_id,
        "url": url,
        "headers": resp_headers,
    })
    .to_string())
}

/// Assemble a `Response` for the on_response interception callbacks from the
/// parts op_fetch_url already holds. Navigation gets a Response straight from
/// the http client, but the JS fetch path builds the pieces itself.
fn fetch_response(
    url: &str,
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
) -> Response {
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
    mode: String,
    credentials: FetchCredentials,
    callbacks: Option<Arc<CallbackRegistry>>,
    allow_private_network: bool,
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
        let current_is_cross_origin = parsed_current.origin().ascii_serialization() != page_origin;
        if current_is_cross_origin {
            req_headers.insert("origin".to_string(), page_origin.clone());
        }
        for (k, v) in &custom_headers {
            req_headers.insert(k.to_lowercase(), v.clone());
        }

        let credentials_allowed = credentials.allows(&page_origin, &current_url);
        let r = stealth
            .send_single(
                &current_method,
                &parsed_current,
                &req_headers,
                &current_body,
                credentials_allowed,
                credentials_allowed,
            )
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
        if let Err(reason) = validate_fetch_url(&next_url, allow_private_network) {
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

    let final_is_cross_origin = request_origin(&current_url)
        .map(|request_origin| request_origin != page_origin)
        .unwrap_or(false);
    if final_is_cross_origin && mode == "cors" {
        let allowed = resp_headers
            .get("access-control-allow-origin")
            .map(|s| s.as_str())
            .unwrap_or("");
        let allow_credentials = resp_headers
            .get("access-control-allow-credentials")
            .map(|s| s.as_str())
            .unwrap_or("");
        if !cors_response_allows(credentials, &page_origin, allowed, allow_credentials) {
            return Ok(serde_json::json!({
                "status": 0, "body": "", "url": url, "headers": {},
                "corsBlocked": true,
                "corsError": if credentials == FetchCredentials::Include {
                    format!(
                        "CORS error: credentialed request requires Access-Control-Allow-Origin '{}' and Access-Control-Allow-Credentials 'true'",
                        page_origin
                    )
                } else {
                    format!(
                        "CORS error: Origin '{}' not in Access-Control-Allow-Origin '{}'",
                        page_origin, allowed
                    )
                },
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
    use super::{cors_response_allows, glob_match, validate_fetch_url, FetchCredentials};
    use crate::runtime::ObscuraJsRuntime;
    use obscura_dom::parse_html;

    #[cfg(feature = "render")]
    use super::{
        ensure_prepared_geometry, ensure_prepared_render, node_is_connected,
        queue_retained_style_mutation, retained_style_mutation,
        shadow_including_connected_nodes, ObscuraState, MAX_PENDING_STYLE_MUTATIONS,
    };
    #[cfg(feature = "render")]
    use obscura_dom::ShadowRootMode;

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

    #[test]
    fn fetch_credentials_gate_cookie_send_and_storage_per_request_origin() {
        let page_origin = "https://www.example.com";
        let same_origin_url = "https://www.example.com/api";
        let explicit_default_port = "https://www.example.com:443/api";
        let cross_origin_url = "https://api.example.com/data";

        assert!(!FetchCredentials::Omit.allows(page_origin, same_origin_url));
        assert!(!FetchCredentials::Omit.allows(page_origin, cross_origin_url));

        assert!(FetchCredentials::SameOrigin.allows(page_origin, same_origin_url));
        assert!(FetchCredentials::SameOrigin.allows(page_origin, explicit_default_port));
        assert!(!FetchCredentials::SameOrigin.allows(page_origin, cross_origin_url));

        assert!(FetchCredentials::Include.allows(page_origin, same_origin_url));
        assert!(FetchCredentials::Include.allows(page_origin, cross_origin_url));
    }

    #[test]
    fn credentialed_cors_requires_exact_origin_and_allow_credentials() {
        let page_origin = "https://www.example.com";

        assert!(cors_response_allows(
            FetchCredentials::SameOrigin,
            page_origin,
            "*",
            "",
        ));
        assert!(!cors_response_allows(
            FetchCredentials::Include,
            page_origin,
            "*",
            "true",
        ));
        assert!(!cors_response_allows(
            FetchCredentials::Include,
            page_origin,
            page_origin,
            "",
        ));
        assert!(cors_response_allows(
            FetchCredentials::Include,
            page_origin,
            page_origin,
            "true",
        ));
    }

    #[test]
    fn fetch_url_validation_honors_per_context_private_network_opt_in() {
        let loopback = url::Url::parse("http://127.0.0.1:8080/resource").unwrap();
        assert!(validate_fetch_url(&loopback, true).is_ok());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn posted_task_chains_complete_without_zero_delay_timer_floor() {
        let mut runtime = ObscuraJsRuntime::new();
        runtime.set_dom(parse_html("<html><body></body></html>"));
        runtime.set_url("http://example.com/posted-task-test");
        runtime.run_page_init();
        runtime
            .execute_script(
                "posted-task-throughput",
                r#"
                    globalThis.__postedTaskBench = {
                        message: 0,
                        postTask: 0,
                        yields: 0,
                        started: performance.now(),
                        finished: 0,
                    };
                    const markFinished = () => {
                        if (__postedTaskBench.message === 100 &&
                            __postedTaskBench.postTask === 100 &&
                            __postedTaskBench.yields === 100) {
                            __postedTaskBench.finished = performance.now();
                        }
                    };

                    const channel = new MessageChannel();
                    channel.port2.onmessage = () => {
                        __postedTaskBench.message++;
                        if (__postedTaskBench.message < 100) channel.port1.postMessage(null);
                        else markFinished();
                    };
                    channel.port1.postMessage(null);

                    const postNext = () => scheduler.postTask(() => {
                        __postedTaskBench.postTask++;
                        if (__postedTaskBench.postTask < 100) postNext();
                        else markFinished();
                    });
                    postNext();

                    scheduler.postTask(async () => {
                        while (__postedTaskBench.yields < 100) {
                            await scheduler.yield();
                            __postedTaskBench.yields++;
                        }
                        markFinished();
                    });
                "#,
            )
            .unwrap();

        runtime.run_event_loop_bounded(100).await.unwrap();
        let result = runtime
            .evaluate(
                r#"[
                    __postedTaskBench.message,
                    __postedTaskBench.postTask,
                    __postedTaskBench.yields,
                    __postedTaskBench.finished - __postedTaskBench.started,
                ]"#,
            )
            .unwrap();
        let values = result.as_array().unwrap();
        assert!(
            values[..3].iter().all(|value| value.as_f64() == Some(100.0)),
            "posted-task chains did not finish inside the 100ms pump: {result}",
        );
        assert!(
            values[3].as_f64().is_some_and(|elapsed| elapsed >= 0.0 && elapsed < 75.0),
            "300 chained posted-task deliveries retained timer-wheel latency: {result}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shared_posted_task_queue_preserves_priority_fifo_and_microtasks() {
        let mut runtime = ObscuraJsRuntime::new();
        runtime.set_dom(parse_html("<html><body></body></html>"));
        runtime.set_url("http://example.com/posted-task-order");
        runtime.run_page_init();
        runtime
            .execute_script(
                "shared-posted-task-order",
                r#"
                    globalThis.__sharedPostedOrder = ["sync"];
                    const channel = new MessageChannel();
                    channel.port2.onmessage = event => {
                        __sharedPostedOrder.push("message-" + event.data);
                        Promise.resolve().then(() => {
                            __sharedPostedOrder.push("message-" + event.data + "-microtask");
                        });
                    };
                    channel.port1.postMessage(1);
                    scheduler.postTask(() => {
                        __sharedPostedOrder.push("visible");
                        Promise.resolve().then(() => __sharedPostedOrder.push("visible-microtask"));
                    });
                    channel.port1.postMessage(2);
                    scheduler.postTask(() => {
                        __sharedPostedOrder.push("background");
                    }, { priority: "background" });
                    scheduler.postTask(() => {
                        __sharedPostedOrder.push("blocking");
                        Promise.resolve().then(() => __sharedPostedOrder.push("blocking-microtask"));
                    }, { priority: "user-blocking" });
                    Promise.resolve().then(() => __sharedPostedOrder.push("initial-microtask"));
                "#,
            )
            .unwrap();

        runtime.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            runtime.evaluate("__sharedPostedOrder").unwrap(),
            serde_json::json!([
                "sync",
                "initial-microtask",
                "blocking",
                "blocking-microtask",
                "message-1",
                "message-1-microtask",
                "visible",
                "visible-microtask",
                "message-2",
                "message-2-microtask",
                "background",
            ]),
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn connected_shadow_nodes_invalidate_without_entering_light_tree_retention() {
        let dom = parse_html(
            r#"<x-host id="host"></x-host><div id="source"><span id="shadow-child"></span></div>"#,
        );
        let host = dom.get_element_by_id("host").unwrap();
        let source = dom.get_element_by_id("source").unwrap();
        let child = dom.get_element_by_id("shadow-child").unwrap();
        let root = dom
            .attach_shadow_root(host, ShadowRootMode::Open)
            .unwrap();
        dom.append_child(root, child);

        assert!(node_is_connected(&dom, child));
        assert!(shadow_including_connected_nodes(&dom).contains(&child));
        assert!(
            retained_style_mutation(&dom, "set_attribute", &child.index().to_string(), "class\0changed")
                .is_none(),
            "shadow mutations require a full scoped cascade"
        );

        dom.append_child(source, host);
        assert!(node_is_connected(&dom, child));
        dom.remove(source);
        assert!(!node_is_connected(&dom, child));
        assert!(!shadow_including_connected_nodes(&dom).contains(&child));
    }

    #[cfg(feature = "render")]
    #[test]
    fn repeated_inline_style_writes_share_one_retained_dirty_marker_per_node() {
        let mut pending = Vec::new();
        let style_mutation = |raw| {
            obscura_render::RetainedStyleMutation::Attribute(
                obscura_render::AttributeStyleMutation {
                    node: obscura_dom::tree::NodeId::new(raw),
                    name: "style".to_string(),
                    old_value: None,
                    new_value: None,
                },
            )
        };

        // Motion/React commonly writes a connected element's serialized style
        // twice in one commit. The old queue reached its 256-record ceiling at
        // only 128 elements and discarded the complete PreparedRender.
        for raw in 1..=200 {
            assert!(queue_retained_style_mutation(
                &mut pending,
                style_mutation(raw),
            ));
            assert!(queue_retained_style_mutation(
                &mut pending,
                style_mutation(raw),
            ));
        }
        assert_eq!(pending.len(), 200);

        // The memory bound remains real: unique dirty nodes still consume one
        // slot, while an already-recorded node remains safe at the ceiling.
        for raw in 201..=MAX_PENDING_STYLE_MUTATIONS as u32 {
            assert!(queue_retained_style_mutation(
                &mut pending,
                style_mutation(raw),
            ));
        }
        assert_eq!(pending.len(), MAX_PENDING_STYLE_MUTATIONS);
        assert!(queue_retained_style_mutation(
            &mut pending,
            style_mutation(1),
        ));
        assert!(!queue_retained_style_mutation(
            &mut pending,
            style_mutation(MAX_PENDING_STYLE_MUTATIONS as u32 + 1),
        ));
        assert_eq!(pending.len(), MAX_PENDING_STYLE_MUTATIONS);
    }

    #[cfg(feature = "render")]
    #[test]
    fn repeated_selector_attribute_writes_keep_only_the_rendered_transition() {
        let node = obscura_dom::tree::NodeId::new(7);
        let mutation = |old: &str, new: &str| {
            obscura_render::RetainedStyleMutation::Attribute(
                obscura_render::AttributeStyleMutation {
                    node,
                    name: "class".to_string(),
                    old_value: Some(old.to_string()),
                    new_value: Some(new.to_string()),
                },
            )
        };
        let mut pending = Vec::new();
        assert!(queue_retained_style_mutation(
            &mut pending,
            mutation("before", "intermediate"),
        ));
        assert!(queue_retained_style_mutation(
            &mut pending,
            mutation("intermediate", "after"),
        ));
        assert_eq!(
            pending,
            vec![obscura_render::RetainedStyleMutation::Attribute(
                obscura_render::AttributeStyleMutation {
                    node,
                    name: "class".to_string(),
                    old_value: Some("before".to_string()),
                    new_value: Some("after".to_string()),
                }
            )]
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn repeated_animation_changes_share_one_retained_dirty_marker_per_node() {
        let mut pending = Vec::new();
        let first = obscura_dom::tree::NodeId::new(1);
        let second = obscura_dom::tree::NodeId::new(2);
        for _ in 0..300 {
            assert!(queue_retained_style_mutation(
                &mut pending,
                obscura_render::RetainedStyleMutation::Animation { node: first },
            ));
        }
        assert!(queue_retained_style_mutation(
            &mut pending,
            obscura_render::RetainedStyleMutation::Animation { node: second },
        ));
        assert_eq!(
            pending,
            vec![
                obscura_render::RetainedStyleMutation::Animation { node: first },
                obscura_render::RetainedStyleMutation::Animation { node: second },
            ]
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn repeated_resource_changes_share_one_retained_refresh_marker() {
        let mut pending = vec![obscura_render::RetainedStyleMutation::Animation {
            node: obscura_dom::tree::NodeId::new(1),
        }];
        for _ in 0..300 {
            assert!(queue_retained_style_mutation(
                &mut pending,
                obscura_render::RetainedStyleMutation::Resource,
            ));
        }
        assert_eq!(
            pending,
            vec![
                obscura_render::RetainedStyleMutation::Animation {
                    node: obscura_dom::tree::NodeId::new(1),
                },
                obscura_render::RetainedStyleMutation::Resource,
            ]
        );

        let mut full_style_batch = (1..=MAX_PENDING_STYLE_MUTATIONS)
            .map(|raw| obscura_render::RetainedStyleMutation::Animation {
                node: obscura_dom::tree::NodeId::new(raw as u32),
            })
            .collect::<Vec<_>>();
        assert!(queue_retained_style_mutation(
            &mut full_style_batch,
            obscura_render::RetainedStyleMutation::Resource,
        ));
        assert_eq!(full_style_batch.len(), MAX_PENDING_STYLE_MUTATIONS + 1);
        assert!(!queue_retained_style_mutation(
            &mut full_style_batch,
            obscura_render::RetainedStyleMutation::Animation {
                node: obscura_dom::tree::NodeId::new(5_000),
            },
        ));
    }

    #[cfg(feature = "render")]
    #[test]
    fn geometry_consumer_defers_paint_only_sample_until_exact_consumer() {
        let dom = parse_html(
            r#"<style>
                @keyframes fade { from { opacity:0 } to { opacity:1 } }
                #box { width:40px;height:20px;animation:fade 1000ms linear both }
            </style><div id="box"></div>"#,
        );
        let box_node = dom.get_element_by_id("box").unwrap();
        let mut state = ObscuraState::new();
        state.dom = Some(dom);
        state.animation_sample = obscura_render::AnimationSample::document(0.0);
        ensure_prepared_render(&mut state).expect("initial render");
        assert_eq!(
            state.prepared_render.as_ref().unwrap().layout().styles[&box_node].opacity,
            Some(0.0),
        );

        state.animation_sample = obscura_render::AnimationSample::document(500.0);
        let geometry = ensure_prepared_geometry(&mut state).expect("retained geometry");
        assert_eq!(geometry.animation_sample_time().milliseconds, 0.0);
        assert_eq!(geometry.document_rect(box_node).unwrap().width, 40.0);
        assert_eq!(geometry.layout().styles[&box_node].opacity, Some(0.0));

        let exact = ensure_prepared_render(&mut state).expect("exact sampled style");
        assert_eq!(exact.animation_sample_time().milliseconds, 500.0);
        let opacity = exact.layout().styles[&box_node].opacity.unwrap();
        assert!((opacity - 0.5).abs() < 0.01, "exact opacity was {opacity}");
        assert_eq!(exact.document_rect(box_node).unwrap().width, 40.0);
    }

    #[cfg(feature = "render")]
    #[test]
    fn geometry_consumer_materializes_geometry_animation_sample() {
        let dom = parse_html(
            r#"<style>
                @keyframes grow { from { width:20px } to { width:100px } }
                #box { height:20px;animation:grow 1000ms linear both }
            </style><div id="box"></div>"#,
        );
        let box_node = dom.get_element_by_id("box").unwrap();
        let mut state = ObscuraState::new();
        state.dom = Some(dom);
        state.animation_sample = obscura_render::AnimationSample::document(0.0);
        ensure_prepared_render(&mut state).expect("initial render");

        state.animation_sample = obscura_render::AnimationSample::document(500.0);
        let geometry = ensure_prepared_geometry(&mut state).expect("sampled geometry");
        assert_eq!(geometry.animation_sample_time().milliseconds, 500.0);
        let width = geometry.document_rect(box_node).unwrap().width;
        assert!((width - 60.0).abs() < 0.1, "sampled width was {width}");
    }
}

fn validate_fetch_url(url: &url::Url, allow_private_network: bool) -> Result<(), String> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" && scheme != "file" {
        return Err(format!(
            "Forbidden URL scheme '{}' - only http, https, and file are allowed",
            scheme
        ));
    }

    if scheme == "file"
        || allow_private_network
        || obscura_net::env_allows_private_network()
    {
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
fn op_get_cookies(scope: &mut v8::HandleScope, state: &OpState) -> String {
    let gs = realm_state(scope, state);
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
fn op_set_cookie(scope: &mut v8::HandleScope, state: &OpState, #[string] cookie_str: &str) {
    let gs = realm_state(scope, state);
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

// A frame that navigates itself must not move the top document. Recording the
// navigation against the calling realm keeps it inside that frame.
#[op2(fast)]
fn op_navigate(
    scope: &mut v8::HandleScope,
    state: &OpState,
    #[string] url: &str,
    #[string] method: &str,
    #[string] body: &str,
) {
    let gs = realm_state(scope, state);
    let mut gs = gs.borrow_mut();
    gs.url = url.to_string();
    gs.pending_navigation = Some((url.to_string(), method.to_string(), body.to_string()));
}

fn frame_message_queue_entry_limit() -> usize {
    std::env::var("OBSCURA_FRAME_MESSAGE_QUEUE_ENTRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4096)
}

fn frame_message_queue_byte_limit() -> usize {
    std::env::var("OBSCURA_FRAME_MESSAGE_QUEUE_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8 * 1024 * 1024)
}

// Queues one postMessage for another realm. Always on the page's state, never
// the caller's: the Page drains a single queue, and a message sent by a nested
// frame would otherwise sit in that frame's own state and never be looked at.
//
// The queue is capped. Script can post in a synchronous loop while the host
// only drains between event loop turns, and this buffer lives on the process
// heap rather than V8's, so an unbounded queue would let a page grow memory
// without bound in the one place the heap-limit guard cannot see. Over the cap
// the newest message is dropped, keeping the earlier traffic that a widget
// handshake actually depends on.
#[op2(fast)]
fn op_post_frame_message(
    state: &OpState,
    target_frame_id: u32,
    source_frame_id: u32,
    #[string] origin: &str,
    #[string] data_json: &str,
) {
    let gs = state.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();
    let over_entries = gs.pending_frame_messages.len() >= frame_message_queue_entry_limit();
    let over_bytes = gs
        .pending_frame_message_bytes
        .saturating_add(data_json.len())
        > frame_message_queue_byte_limit();
    if over_entries || over_bytes {
        tracing::warn!(
            "dropping a postMessage for frame {}: {} already queued, {} bytes",
            target_frame_id,
            gs.pending_frame_messages.len(),
            gs.pending_frame_message_bytes,
        );
        return;
    }
    gs.pending_frame_message_bytes = gs.pending_frame_message_bytes.saturating_add(data_json.len());
    gs.pending_frame_messages.push(PendingFrameMessage {
        target_frame_id,
        source_frame_id,
        origin: origin.to_string(),
        data_json: data_json.to_string(),
    });
}

/// Resolves after `millis`, as the timer source for child frame realms.
///
/// deno_core's own timer queue is not usable from a frame: `op_timer_queue`
/// resolves per-context state that only a deno_core-created context carries,
/// and a snapshot-restored realm has none. This resolves an ordinary promise
/// instead, and V8 reports the frame as the microtask context, so the ops a
/// timer callback makes still find the frame's own document.
#[op2(async)]
async fn op_sleep(#[number] millis: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
}

// Hands a fetched frame document to the host and returns the id the frame will
// have. The realm itself is built later, by whoever owns the runtime.
#[op2(fast)]
fn op_frame_document_ready(
    scope: &mut v8::HandleScope,
    state: &OpState,
    #[string] url: &str,
    #[string] html: &str,
    #[number] viewport_width: u64,
    #[number] viewport_height: u64,
) -> u32 {
    // Whoever called this is the new frame's parent, which is how a frame
    // nested two deep gets `parent` pointing at the frame above it rather than
    // at the page.
    let parent_frame_id = realm_state(scope, state).borrow().frame_id;
    let gs = state.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();
    gs.frame_id_counter += 1;
    let frame_id = gs.frame_id_counter;
    gs.pending_frames.push(PendingFrame {
        frame_id,
        url: url.to_string(),
        html: html.to_string(),
        viewport_width,
        viewport_height,
        parent_frame_id,
    });
    frame_id
}

/// Whether async host work can be scheduled without aborting the isolate.
///
/// Some low-level embedders intentionally execute a synchronous expression
/// without entering Tokio (for example, update scroll state and immediately
/// capture). deno_core's timer queue requires a reactor even to enqueue a
/// zero-delay timer, so the bootstrap uses this probe for its sync-only
/// compatibility path.
#[op2(fast)]
fn op_async_runtime_available() -> bool {
    tokio::runtime::Handle::try_current().is_ok()
}

/// Wake one browser posted task without routing through Tokio's timer wheel.
/// `yield_now` guarantees the op cannot settle in the initiating JavaScript
/// turn, while avoiding the roughly one-millisecond floor of a zero-duration
/// timer. The bootstrap owns task priority, FIFO order, and one-at-a-time
/// delivery; this op supplies only the event-loop wake boundary.
#[op2(async)]
async fn op_posted_task() {
    tokio::task::yield_now().await;
}

// Records a binding call from page JS. The CDP layer drains this queue
// after every dispatch and emits one `Runtime.bindingCalled` event per
// entry, that's how puppeteer's `page.exposeFunction` callbacks fire.
#[op2(fast)]
fn op_binding_called(state: &OpState, #[string] name: &str, #[string] payload: &str) {
    let gs = state.borrow::<SharedState>().clone();
    let mut gs = gs.borrow_mut();
    gs.pending_binding_calls
        .push((name.to_string(), payload.to_string()));
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
                    .map_err(|_| {
                        crypto_err("AES-GCM decryption failed: authentication tag mismatch")
                    })?
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
        _ => {
            return Err(crypto_err(
                "AES-CTR supports counter lengths of 32, 64, or 128 bits",
            ))
        }
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

/// Canonicalize and validate a `document.domain` assignment.
///
/// Gecko's `Document::IsValidDomain` accepts the current effective host or a
/// dot-delimited suffix no shorter than its registrable domain.  The latter
/// check is important: a plain `ends_with` would let `foo.example.co.uk`
/// relax all the way to `co.uk`, and would incorrectly treat private suffixes
/// such as `github.io` as shared registrable domains.
///
/// An empty return value means SecurityError on the JS side.  The current host
/// is supplied by the Document rather than read from op state because repeated
/// assignments operate on the already-relaxed effective domain.
#[op2]
#[string]
fn op_document_domain_candidate(#[string] current: &str, #[string] input: &str) -> String {
    let canonical = match url::Host::parse(input) {
        Ok(host) => host.to_string().to_ascii_lowercase(),
        Err(_) => return String::new(),
    };
    let current = current.to_ascii_lowercase();

    // Gecko permits assigning the exact current host, including IP literals
    // and single-label hosts.  Neither can be relaxed to a parent.
    if canonical == current {
        return canonical;
    }
    if current.parse::<std::net::IpAddr>().is_ok()
        || canonical.parse::<std::net::IpAddr>().is_ok()
        || !current.ends_with(&format!(".{canonical}"))
    {
        return String::new();
    }

    // `domain_str` is the eTLD+1.  A candidate shorter than it is a public
    // suffix and must not become an effective domain.
    match psl::domain_str(&current) {
        Some(registrable) if canonical.len() >= registrable.len() => canonical,
        _ => String::new(),
    }
}

#[op2]
#[string]
fn op_add_import_map(
    state: &OpState,
    #[string] source: String,
    #[string] base_url: String,
) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let import_map = shared.borrow().import_map.clone();
    let parsed = match ImportMap::parse(&source, &base_url) {
        Ok(map) => map,
        Err(error) => return error,
    };
    let result = match import_map.try_borrow_mut() {
        Ok(mut current) => {
            current.merge(parsed);
            String::new()
        }
        Err(_) => "Import map is already borrowed".to_string(),
    };
    result
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
fn op_text_decode(
    #[string] label: &str,
    #[buffer] bytes: &[u8],
    fatal: bool,
    ignore_bom: bool,
) -> String {
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

#[cfg(feature = "render")]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DynamicFontFaceInput {
    family: String,
    source: String,
    style: String,
    weight: String,
    unicode_range: String,
}

/// Replace the native snapshot of `document.fonts`. The JS implementation
/// remains the source of truth for set semantics; this narrow bridge only
/// supplies resource descriptors to the render preparation path.
#[cfg(feature = "render")]
#[op2(fast)]
fn op_set_dynamic_fonts(state: &OpState, #[string] registrations: &str) -> bool {
    let Ok(inputs) = serde_json::from_str::<Vec<DynamicFontFaceInput>>(registrations) else {
        return false;
    };
    // Keep the observable registry broad enough for generated font families
    // (large applications commonly register dozens of subset faces). The
    // renderer independently caps decoded resources after ASCII filtering and
    // URL deduplication. BufferSource faces arrive as data URLs, so cap their
    // aggregate descriptor payload as well as each entry.
    if inputs.len() > 256
        || inputs
            .iter()
            .try_fold(0usize, |total, face| total.checked_add(face.source.len()))
            .map_or(true, |total| total > 64 * 1024 * 1024)
        || inputs.iter().any(|face| {
            face.family.len() > 1024
                || face.source.len() > 12 * 1024 * 1024
                || face.style.len() > 256
                || face.weight.len() > 256
                || face.unicode_range.len() > 4096
        })
    {
        return false;
    }
    let fonts = inputs
        .into_iter()
        .map(|face| obscura_render::DynamicFontFace {
            family: face.family,
            source: face.source,
            style: face.style,
            weight: face.weight,
            unicode_range: face.unicode_range,
        })
        .collect::<Vec<_>>();
    let shared = state.borrow::<SharedState>().clone();
    let mut state = shared.borrow_mut();
    if state.dynamic_fonts != fonts {
        state.dynamic_fonts = fonts;
        invalidate_render_resource_geometry(&mut state);
    }
    true
}

/// Retain the JavaScript-owned Canvas2D pixel buffer without copying it. A
/// canvas resize supplies a new fixed backing store and atomically replaces
/// the previous surface for the same DOM node.
#[cfg(feature = "render")]
#[op2]
fn op_canvas_register_surface(
    state: &OpState,
    nid: u32,
    width: u32,
    height: u32,
    #[buffer] pixels: JsBuffer,
) -> bool {
    const MAX_CANVAS_DIMENSION: u32 = 32_767;
    const MAX_CANVAS_PIXELS: usize = 67_108_864;
    const MAX_CANVAS_SURFACE_BYTES: usize = 256 * 1024 * 1024;
    let Some(expected) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return false;
    };
    if width > MAX_CANVAS_DIMENSION
        || height > MAX_CANVAS_DIMENSION
        || expected / 4 > MAX_CANVAS_PIXELS
        || pixels.len() != expected
    {
        return false;
    }

    let shared = state.borrow::<SharedState>().clone();
    let mut state = shared.borrow_mut();
    let node = NodeId::new(nid);
    let is_canvas = state
        .dom
        .as_ref()
        .and_then(|dom| dom.get_node(node))
        .is_some_and(|node| {
            node.as_element()
                .is_some_and(|name| name.local.as_ref() == "canvas")
        });
    if !is_canvas {
        return false;
    }
    let replacing = state.canvas_surfaces.get(&node).map(|surface| surface.pixels.len());
    let retained_bytes = state
        .canvas_surfaces
        .values()
        .try_fold(0usize, |total, surface| total.checked_add(surface.pixels.len()))
        .and_then(|total| total.checked_sub(replacing.unwrap_or(0)))
        .and_then(|total| total.checked_add(expected));
    if retained_bytes.is_none_or(|bytes| bytes > MAX_CANVAS_SURFACE_BYTES) {
        return false;
    }
    state.canvas_surfaces.insert(
        node,
        CanvasBackingSurface {
            width,
            height,
            pixels,
        },
    );
    true
}

/// Report one coalesced Canvas2D paint at the JavaScript task boundary. Pixel
/// bytes are already live through the retained backing store, so damage wakes
/// screencast/readiness without throwing away otherwise-valid layout.
#[cfg(feature = "render")]
#[op2(fast)]
fn op_canvas_paint_damage(state: &OpState, nid: u32) -> bool {
    let shared = state.borrow::<SharedState>().clone();
    let mut state = shared.borrow_mut();
    let node = NodeId::new(nid);
    if !state.canvas_surfaces.contains_key(&node) {
        return false;
    }
    let connected = state
        .dom
        .as_ref()
        .is_some_and(|dom| node_is_connected(dom, node));
    if connected {
        state.activity_generation = state.activity_generation.wrapping_add(1);
    }
    connected
}

pub fn build_extension() -> Extension {
    let mut ops = vec![
        op_dom(),
        op_script_mark_started(),
        op_script_try_start(),
        op_shadow_attach(),
        op_shadow_root_info(),
        op_console_msg(),
        op_fetch_url(),
        op_get_cookies(),
        op_set_cookie(),
        op_navigate(),
        op_frame_document_ready(),
        op_post_frame_message(),
        op_sleep(),
        op_async_runtime_available(),
        op_posted_task(),
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
        op_document_domain_candidate(),
        op_add_import_map(),
        op_encoding_for_label(),
        op_text_decode(),
        op_url_encode_query(),
    ];
    // Only registered when the render feature is compiled in. bootstrap.js
    // probes with typeof before calling, so the op's absence is a clean fallback.
    #[cfg(feature = "render")]
    {
        ops.push(op_begin_render_task());
        ops.push(op_set_dynamic_fonts());
        ops.push(op_canvas_register_surface());
        ops.push(op_canvas_paint_damage());
        ops.push(op_image_metadata());
        ops.push(op_load_image_metadata());
        ops.push(op_layout_geometry());
        ops.push(op_resize_observer_measurements());
        ops.push(op_intersection_observer_measurements());
        ops.push(op_computed_style());
        ops.push(op_css_supports());
        ops.push(op_layout_metrics());
        ops.push(op_element_scroll_metrics());
        ops.push(op_element_scroll_to());
        ops.push(op_scroll_offset());
        ops.push(op_scroll_to());
        ops.push(op_waapi_create());
        ops.push(op_waapi_control());
    }
    Extension {
        name: "obscura_dom",
        ops: std::borrow::Cow::Owned(ops),
        ..Default::default()
    }
}

#[cfg(feature = "render")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaapiCreateInput {
    id: u64,
    node: u32,
    keyframes: Vec<WaapiKeyframeInput>,
    duration: f32,
    delay: f32,
    iterations: f32,
    #[serde(default)]
    iterations_infinite: bool,
    fill: String,
    direction: String,
    easing_bezier: Option<[f32; 4]>,
    linear_easing: Option<Vec<f32>>,
}

#[cfg(feature = "render")]
#[derive(Deserialize)]
struct WaapiKeyframeInput {
    offset: f32,
    opacity: Option<f32>,
    transform: Option<String>,
}

#[cfg(feature = "render")]
fn waapi_document_time_ms(state: &ObscuraState) -> f32 {
    state.animation_timeline_origin.elapsed().as_secs_f32() * 1000.0
}

#[cfg(feature = "render")]
fn invalidate_waapi_render(state: &mut ObscuraState, node: NodeId) {
    // Adding or controlling one effect changes the animation cascade only for
    // its target. Keep the previous style graph available to the
    // retained planner instead of turning every animation setup into a full
    // document cascade. The bounded mutation queue remains the safety valve
    // for genuinely broad animation bursts.
    if state.prepared_render.is_some()
        && !queue_retained_style_mutation(
            &mut state.pending_style_mutations,
            obscura_render::RetainedStyleMutation::WaapiAnimation { node },
        )
    {
        state.prepared_render = None;
        state.pending_style_mutations.clear();
    }
    state.resolved_scroll = None;
    state.activity_generation = state.activity_generation.wrapping_add(1);
}

#[cfg(feature = "render")]
#[op2(fast)]
fn op_waapi_create(state: &OpState, #[string] input: &str) -> bool {
    let Ok(input) = serde_json::from_str::<WaapiCreateInput>(input) else {
        return false;
    };
    if !input.duration.is_finite()
        || input.duration < 0.0
        || !input.delay.is_finite()
        || !input.iterations.is_finite()
        || input.iterations < 0.0
        || input.keyframes.is_empty()
    {
        return false;
    }
    let shared = state.borrow::<SharedState>().clone();
    let mut state = shared.borrow_mut();
    let node = NodeId::new(input.node);
    if state.dom.as_ref().and_then(|dom| dom.get_node(node)).is_none() {
        return false;
    }
    let start_time_ms = waapi_document_time_ms(&state);
    let fill_mode = match input.fill.as_str() {
        "forwards" => obscura_render::AnimationFillMode::Forwards,
        "backwards" => obscura_render::AnimationFillMode::Backwards,
        "both" => obscura_render::AnimationFillMode::Both,
        _ => obscura_render::AnimationFillMode::None,
    };
    let direction = match input.direction.as_str() {
        "reverse" => obscura_render::AnimationDirection::Reverse,
        "alternate" => obscura_render::AnimationDirection::Alternate,
        "alternate-reverse" => obscura_render::AnimationDirection::AlternateReverse,
        _ => obscura_render::AnimationDirection::Normal,
    };
    let iterations = if input.iterations_infinite {
        f32::INFINITY
    } else {
        input.iterations
    };
    state.animation_timeline.register_waapi(obscura_render::WaapiAnimation {
        id: input.id,
        node,
        keyframes: input.keyframes.into_iter().map(|frame| obscura_render::WaapiKeyframe {
            offset: frame.offset.clamp(0.0, 1.0),
            opacity: frame.opacity.map(|value| value.clamp(0.0, 1.0)),
            transform: frame.transform,
        }).collect(),
        timing: obscura_render::AnimationTiming {
            duration_ms: input.duration,
            delay_ms: input.delay,
            iteration_count: iterations,
            direction,
            fill_mode,
            play_state: obscura_render::AnimationPlayState::Running,
        },
        easing: input.easing_bezier,
        linear_easing: input.linear_easing,
        start_time_ms,
        hold_time_ms: None,
        play_state: obscura_render::WaapiPlayState::Running,
    });
    invalidate_waapi_render(&mut state, node);
    true
}

#[cfg(feature = "render")]
#[op2(fast)]
fn op_waapi_control(
    state: &OpState,
    id: f64,
    #[string] action: &str,
    value: f64,
) -> bool {
    if !id.is_finite() || id < 0.0 {
        return false;
    }
    let shared = state.borrow::<SharedState>().clone();
    let mut state = shared.borrow_mut();
    let id = id as u64;
    let Some(node) = state.animation_timeline.waapi_node(id) else {
        return false;
    };
    let document_time = waapi_document_time_ms(&state);
    let changed = match action {
        "cancel" => state.animation_timeline.cancel_waapi(id),
        "finish" => state.animation_timeline.finish_waapi(id),
        "pause" => state.animation_timeline.set_waapi_play_state(
            id,
            obscura_render::WaapiPlayState::Paused,
            document_time,
        ),
        "play" => state.animation_timeline.set_waapi_play_state(
            id,
            obscura_render::WaapiPlayState::Running,
            document_time,
        ),
        "currentTime" if value.is_finite() => state.animation_timeline.set_waapi_current_time(
            id,
            document_time,
            value as f32,
        ),
        _ => false,
    };
    if changed {
        invalidate_waapi_render(&mut state, node);
    }
    changed
}

#[cfg(feature = "render")]
pub(crate) fn document_base_url(state: &ObscuraState) -> Option<String> {
    let document_url = url::Url::parse(&state.url).ok()?;
    let base_href = state.dom.as_ref().and_then(|dom| {
        dom.query_selector("base[href]")
            .ok()
            .flatten()
            .and_then(|id| {
                dom.get_node(id)
                    .and_then(|node| node.get_attribute("href").map(str::to_string))
            })
    });
    match base_href {
        Some(href) => document_url.join(&href).ok().map(|url| url.to_string()),
        None => Some(document_url.to_string()),
    }
}

#[cfg(feature = "render")]
pub(crate) fn ensure_prepared_render(
    state: &mut ObscuraState,
) -> Option<&obscura_render::PreparedRender> {
    let base_url = document_base_url(state);
    let viewport = state.viewport;
    let render_media = state.render_media;
    let animation_sample = state.animation_sample;
    let incompatible = state.prepared_render.as_ref().is_some_and(|prepared| {
        prepared.viewport() != viewport
            || prepared.base_url() != base_url.as_deref()
    });
    let needs_rebuild = state.prepared_render.as_ref().map_or(true, |prepared| {
        incompatible || prepared.animation_sample() != animation_sample
    }) || !state.pending_style_mutations.is_empty();
    if needs_rebuild {
        if let Some(dom) = state.dom.as_ref() {
            state
                .animation_timeline
                .materialize_start_candidates(dom);
        }
        let previous = (!incompatible && render_media == obscura_render::CssMediaType::Screen)
            .then(|| state.prepared_render.take())
            .flatten();
        let mutations = std::mem::take(&mut state.pending_style_mutations);
        let prepared = {
            let dom = state.dom.as_ref()?;
            match previous {
                Some(previous) => obscura_render::prepare_dom_with_retained_styles_with_animation_state(
                    dom,
                    viewport,
                    base_url.as_deref(),
                    &mut state.render_resources,
                    &state.dynamic_fonts,
                    &mut state.stylesheet_cache,
                    previous,
                    &mutations,
                    animation_sample,
                    &mut state.animation_timeline,
                )
                .or_else(|| {
                    obscura_render::prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
                        dom,
                        viewport,
                        base_url.as_deref(),
                        &mut state.render_resources,
                        &state.dynamic_fonts,
                        &mut state.stylesheet_cache,
                        animation_sample,
                        &mut state.animation_timeline,
                    )
                })?,
                None => match render_media {
                    obscura_render::CssMediaType::Screen => obscura_render::prepare_dom_with_dynamic_fonts_and_stylesheet_cache_with_animation_state(
                        dom,
                        viewport,
                        base_url.as_deref(),
                        &mut state.render_resources,
                        &state.dynamic_fonts,
                        &mut state.stylesheet_cache,
                        animation_sample,
                        &mut state.animation_timeline,
                    )?,
                    obscura_render::CssMediaType::Print => obscura_render::prepare_dom_with_dynamic_fonts_and_stylesheet_cache_for_media_with_animation_state(
                        dom,
                        viewport,
                        base_url.as_deref(),
                        &mut state.render_resources,
                        &state.dynamic_fonts,
                        &mut state.stylesheet_cache,
                        render_media,
                        animation_sample,
                        &mut state.animation_timeline,
                    )?,
                },
            }
        };
        if animation_sample.mode == obscura_render::AnimationSampleMode::DocumentTime {
            state.animation_timeline.clear_start_candidates();
        }
        let connected = state
            .dom
            .as_ref()
            .map(shadow_including_connected_nodes);
        if let Some(connected) = connected {
            state
                .animation_timeline
                .retain_nodes(|node| connected.contains(&node));
        }
        state.prepared_render = Some(prepared);
        state.resolved_scroll = None;
    }
    state.prepared_render.as_ref()
}

/// Prepare enough state for a geometry-only CSSOM consumer. A forward sample
/// with only paint effects may read the retained layout without resampling its
/// styles. `animation_sample` on PreparedRender remains behind intentionally,
/// making a later paint or computed-style consumer take the exact path above.
#[cfg(feature = "render")]
fn ensure_prepared_geometry(
    state: &mut ObscuraState,
) -> Option<&obscura_render::PreparedRender> {
    let base_url = document_base_url(state);
    let reusable = state.pending_style_mutations.is_empty()
        && !state.animation_timeline.has_pending_start_candidates()
        && state.prepared_render.as_ref().is_some_and(|prepared| {
            prepared.viewport() == state.viewport
                && prepared.base_url() == base_url.as_deref()
                && (prepared.animation_sample() == state.animation_sample
                    || prepared.can_reuse_geometry_for_animation_sample(state.animation_sample))
        });
    if reusable {
        return state.prepared_render.as_ref();
    }
    ensure_prepared_render(state)
}

#[cfg(feature = "render")]
pub(crate) fn sample_live_document_animations(state: &mut ObscuraState) {
    if state.animation_sampled_task_generation == state.animation_task_generation {
        return;
    }
    state.animation_sampled_task_generation = state.animation_task_generation;
    let sample = obscura_render::AnimationSample::document(
        (state.animation_timeline_origin.elapsed().as_secs_f64() * 1_000.0)
            .min(f64::from(f32::MAX)) as f32,
    );
    if state.animation_sample == sample {
        return;
    }
    if sample.time.milliseconds > state.animation_sample.time.milliseconds
        && state.animation_sample.mode == obscura_render::AnimationSampleMode::DocumentTime
        && state.pending_style_mutations.is_empty()
        && state.prepared_render.as_mut().is_some_and(|prepared| {
            prepared.advance_inactive_animation_sample_time(sample.time)
        })
    {
        state.animation_sample = sample;
        return;
    }
    let forward_document_sample =
        sample.mode == obscura_render::AnimationSampleMode::DocumentTime
        && state.animation_sample.mode == obscura_render::AnimationSampleMode::DocumentTime
        && sample.time.milliseconds > state.animation_sample.time.milliseconds;
    state.animation_sample = sample;
    if !forward_document_sample {
        state.prepared_render = None;
        state.pending_style_mutations.clear();
    }
    state.resolved_scroll = None;
}

#[cfg(feature = "render")]
pub(crate) fn begin_animation_task(state: &mut ObscuraState) {
    state.animation_task_generation = state.animation_task_generation.wrapping_add(1);
}

#[cfg(feature = "render")]
#[op2(fast)]
fn op_begin_render_task(state: &OpState) {
    let shared = state.borrow::<SharedState>().clone();
    begin_animation_task(&mut shared.borrow_mut());
}

#[cfg(feature = "render")]
pub(crate) fn ensure_resolved_scroll(state: &mut ObscuraState) -> Option<()> {
    ensure_resolved_scroll_for_consumer(state, false)
}

#[cfg(feature = "render")]
fn ensure_resolved_scroll_for_geometry(state: &mut ObscuraState) -> Option<()> {
    ensure_resolved_scroll_for_consumer(state, true)
}

#[cfg(feature = "render")]
fn ensure_resolved_scroll_for_consumer(
    state: &mut ObscuraState,
    geometry_only: bool,
) -> Option<()> {
    if geometry_only {
        ensure_prepared_geometry(state)?;
    } else {
        ensure_prepared_render(state)?;
    }
    if state
        .resolved_scroll
        .as_ref()
        .is_some_and(|(generation, _)| *generation == state.scroll_generation)
    {
        return Some(());
    }

    let valid = state
        .prepared_render
        .as_ref()?
        .scroll_container_nodes()
        .collect::<HashSet<_>>();
    let snapshot = {
        let dom = state.dom.as_ref()?;
        state.prepared_render.as_ref()?.resolve_scroll_state(
            dom,
            state.scroll_offset,
            &state.element_scroll_offsets,
        )
    };
    state.scroll_offset = snapshot.root_offset();
    for node in valid {
        let offset = state
            .prepared_render
            .as_ref()?
            .element_scroll_metrics(node, &snapshot)
            .map(|metrics| metrics.offset)
            .unwrap_or((0.0, 0.0));
        if offset == (0.0, 0.0) {
            state.element_scroll_offsets.remove(&node);
        } else {
            state.element_scroll_offsets.insert(node, offset);
        }
    }
    state.resolved_scroll = Some((state.scroll_generation, snapshot));
    Some(())
}

#[cfg(feature = "render")]
fn image_metadata_json(
    current_src: String,
    density: f32,
    known: bool,
    dimensions: Option<(f32, f32)>,
) -> String {
    if !known {
        return serde_json::json!({
            "state": "pending",
            "currentSrc": current_src,
            "density": density,
        })
        .to_string();
    }
    match dimensions {
        Some((width, height)) => serde_json::json!({
            "state": "loaded",
            "ok": true,
            "currentSrc": current_src,
            "density": density,
            "width": width,
            "height": height,
        })
        .to_string(),
        None => serde_json::json!({
            "state": "error",
            "ok": false,
            "currentSrc": current_src,
            "density": density,
        })
        .to_string(),
    }
}

#[cfg(feature = "render")]
fn image_request_profile(dom: &DomTree, node_id: NodeId) -> ImageRequestProfile {
    match dom
        .get_node(node_id)
        .and_then(|node| node.get_attribute("crossorigin").map(str::to_owned))
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("use-credentials") => ImageRequestProfile::CorsInclude,
        Some(_) => ImageRequestProfile::CorsSameOrigin,
        None => ImageRequestProfile::NoCorsInclude,
    }
}

#[cfg(feature = "render")]
fn profiled_cached_image_metadata(
    gs: &ObscuraState,
    node_id: NodeId,
) -> Option<(String, f32, bool, Option<(f32, f32)>)> {
    let dom = gs.dom.as_ref()?;
    let base_url = document_base_url(gs);
    gs.render_resources.cached_image_element_metadata(
        dom,
        node_id,
        gs.viewport,
        base_url.as_deref(),
    )
}

#[cfg(feature = "render")]
fn cached_image_metadata_for_node(gs: &ObscuraState, node_id: NodeId) -> String {
    match profiled_cached_image_metadata(gs, node_id) {
        Some((current_src, density, known, dimensions)) => {
            image_metadata_json(current_src, density, known, dimensions)
        }
        None => serde_json::json!({ "ok": false, "currentSrc": "" }).to_string(),
    }
}

/// Probe one ordinary `<img>` through the renderer's page-scoped resource
/// cache. This op is intentionally cache-only. Lifecycle getters call it
/// synchronously and must never open a socket or wait on network I/O.
#[cfg(feature = "render")]
#[op2]
#[string]
fn op_image_metadata(state: &OpState, nid: u32, _cached_only: bool) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let gs = shared.borrow();
    let node_id = NodeId::new(nid);
    let is_image = gs.dom.as_ref().is_some_and(|dom| {
        dom.get_node(node_id).is_some_and(|node| {
            node.as_element()
                .is_some_and(|element| element.local.as_ref() == "img")
        })
    });
    if !is_image {
        return serde_json::json!({ "ok": false, "currentSrc": "" }).to_string();
    }
    cached_image_metadata_for_node(&gs, node_id)
}

/// Compatibility path for standalone render runtimes which deliberately
/// install an in-memory `RenderResourceLoader` but have no owning page
/// transport. Browser pages always install `ObscuraHttpClient` before page
/// script runs and never enter this synchronous loader.
#[cfg(feature = "render")]
fn load_image_metadata_without_page_transport(gs: &mut ObscuraState, node_id: NodeId) -> String {
    let base_url = document_base_url(&gs);
    let viewport = gs.viewport;
    let previous_dimensions = gs.dom.as_ref().and_then(|dom| {
        gs.render_resources
            .cached_image_element_metadata(dom, node_id, viewport, base_url.as_deref())
            .and_then(|(_, _, known, dimensions)| known.then_some(dimensions).flatten())
    });
    let Some(dom) = gs.dom.as_ref() else {
        return serde_json::json!({ "ok": false, "currentSrc": "" }).to_string();
    };
    let Some((current_src, density, dimensions)) = gs.render_resources.image_element_metadata(
        dom,
        node_id,
        viewport,
        base_url.as_deref(),
    ) else {
        return serde_json::json!({
            "state": "error",
            "ok": false,
            "currentSrc": "",
        })
        .to_string();
    };
    if dimensions.is_some() && dimensions != previous_dimensions {
        invalidate_render_resource_geometry(gs);
    }
    image_metadata_json(current_src, density, true, dimensions)
}

#[cfg(feature = "render")]
fn finish_async_image_metadata(
    shared: &SharedState,
    node_id: NodeId,
    document_generation: u64,
    expected_url: &str,
    request_profile: ImageRequestProfile,
) -> String {
    let gs = shared.borrow();
    if gs.document_generation != document_generation {
        return serde_json::json!({ "state": "stale", "currentSrc": expected_url })
            .to_string();
    }
    let Some(dom) = gs.dom.as_ref() else {
        return serde_json::json!({ "state": "stale", "currentSrc": expected_url })
            .to_string();
    };
    if image_request_profile(dom, node_id) != request_profile {
        return serde_json::json!({ "state": "stale", "currentSrc": expected_url })
            .to_string();
    }
    let Some((current_src, density, known, dimensions)) =
        profiled_cached_image_metadata(&gs, node_id)
    else {
        return serde_json::json!({ "state": "stale", "currentSrc": expected_url })
            .to_string();
    };
    if current_src != expected_url {
        return serde_json::json!({ "state": "stale", "currentSrc": current_src }).to_string();
    }
    image_metadata_json(current_src, density, known, dimensions)
}

/// Load HTMLImageElement bytes through the owning page's async transport.
/// Network runs after every RefCell borrow is released, requests for the same
/// navigation/URL/profile share one fetch, and completion revalidates both the
/// document identity and responsive candidate before exposing lifecycle state.
#[cfg(feature = "render")]
#[op2(async)]
#[string]
async fn op_load_image_metadata(state: Rc<RefCell<OpState>>, nid: u32) -> String {
    let shared = {
        let state = state.borrow();
        state.borrow::<SharedState>().clone()
    };
    let node_id = NodeId::new(nid);
    let (
        document_generation,
        selected_url,
        request_profile,
        resource_request,
        http_client,
        callbacks,
        page_in_flight,
        blocked,
    ) = {
        let gs = shared.borrow();
        let Some(dom) = gs.dom.as_ref() else {
            return serde_json::json!({ "state": "stale", "currentSrc": "" }).to_string();
        };
        let is_image = dom.get_node(node_id).is_some_and(|node| {
            node.as_element()
                .is_some_and(|element| element.local.as_ref() == "img")
        });
        if !is_image {
            return serde_json::json!({ "state": "stale", "currentSrc": "" }).to_string();
        }
        let profile = image_request_profile(dom, node_id);
        let Some((selected_url, _, known, _)) =
            profiled_cached_image_metadata(&gs, node_id)
        else {
            return serde_json::json!({ "state": "error", "ok": false, "currentSrc": "" })
                .to_string();
        };
        if known {
            return cached_image_metadata_for_node(&gs, node_id);
        }
        let initiator = url::Url::parse(&gs.url)
            .or_else(|_| url::Url::parse(&selected_url))
            .unwrap_or_else(|_| url::Url::parse("about:blank").unwrap());
        let mut request = ResourceRequest::subresource(ResourceType::Image, &initiator);
        match profile {
            ImageRequestProfile::CorsInclude => {
                request.mode = RequestMode::Cors;
                request.credentials = RequestCredentials::Include;
            }
            ImageRequestProfile::CorsSameOrigin => {
                request.mode = RequestMode::Cors;
                request.credentials = RequestCredentials::SameOrigin;
            }
            ImageRequestProfile::NoCorsInclude => {}
        }
        let blocked = gs.blocked_urls.iter().any(|pattern| {
            pattern == "*" || selected_url.contains(pattern) || glob_match(pattern, &selected_url)
        });
        (
            gs.document_generation,
            selected_url,
            profile,
            request,
            gs.http_client.clone(),
            gs.callbacks.clone(),
            Arc::clone(&gs.page_in_flight),
            blocked,
        )
    };

    #[cfg(feature = "stealth")]
    let stealth_client = shared.borrow().stealth_client.clone();
    #[cfg(feature = "stealth")]
    let has_page_transport = http_client.is_some() || stealth_client.is_some();
    #[cfg(not(feature = "stealth"))]
    let has_page_transport = http_client.is_some();
    if !has_page_transport {
        return load_image_metadata_without_page_transport(&mut shared.borrow_mut(), node_id);
    }

    // Different CORS/credential profiles do not share an in-flight response.
    let request_key = (document_generation, selected_url.clone(), request_profile);
    let follower = {
        let mut gs = shared.borrow_mut();
        if let Some(waiters) = gs.render_image_in_flight.get_mut(&request_key) {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            waiters.push(sender);
            Some(receiver)
        } else {
            gs.render_image_in_flight.insert(request_key.clone(), Vec::new());
            None
        }
    };
    if let Some(receiver) = follower {
        let _ = receiver.await;
        return finish_async_image_metadata(
            &shared,
            node_id,
            document_generation,
            &selected_url,
            request_profile,
        );
    }

    struct PageImageInFlightGuard(Arc<std::sync::atomic::AtomicU32>);
    impl Drop for PageImageInFlightGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    page_in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _page_in_flight = PageImageInFlightGuard(page_in_flight);

    let parsed_url = url::Url::parse(&selected_url).ok();
    let response = if blocked || parsed_url.is_none() {
        None
    } else {
        let parsed_url = parsed_url.as_ref().unwrap();
        #[cfg(feature = "stealth")]
        {
            if let Some(client) = stealth_client {
                client
                    .fetch_resource_with_callbacks(
                        parsed_url,
                        resource_request.clone(),
                        callbacks.as_deref(),
                    )
                    .await
                    .ok()
            } else {
                http_client
                    .as_ref()
                    .unwrap()
                    .fetch_resource_with_callbacks(
                        parsed_url,
                        resource_request,
                        callbacks.as_deref(),
                    )
                    .await
                    .ok()
            }
        }
        #[cfg(not(feature = "stealth"))]
        {
            http_client
                .as_ref()
                .unwrap()
                .fetch_resource_with_callbacks(
                    parsed_url,
                    resource_request,
                    callbacks.as_deref(),
                )
                .await
                .ok()
        }
    };
    let bytes = response.and_then(|response| {
        (200..300)
            .contains(&response.status)
            .then_some(response.body)
    });
    let waiters = {
        let mut gs = shared.borrow_mut();
        if gs.document_generation == document_generation {
            match bytes {
                Some(bytes) => {
                    if obscura_render::image_intrinsic_dimensions(&bytes).is_some() {
                        gs.render_resources.seed_image(
                            selected_url.clone(),
                            request_profile,
                            bytes,
                        );
                        // The leader owns the unknown-to-known cache
                        // transition. Followers only observe this result and
                        // must not invalidate the retained render again.
                        invalidate_render_resource_geometry(&mut gs);
                    } else {
                        gs.render_resources
                            .seed_image_missing(selected_url.clone(), request_profile);
                    }
                }
                None => {
                    gs.render_resources
                        .seed_image_missing(selected_url.clone(), request_profile);
                }
            }
        }
        gs.render_image_in_flight
            .remove(&request_key)
            .unwrap_or_default()
    };
    for waiter in waiters {
        let _ = waiter.send(());
    }
    finish_async_image_metadata(
        &shared,
        node_id,
        document_generation,
        &selected_url,
        request_profile,
    )
}

#[cfg(feature = "render")]
pub(crate) fn clamp_scroll_offset(state: &mut ObscuraState, requested: (f32, f32)) -> (f32, f32) {
    clamp_scroll_offset_for_consumer(state, requested, false)
}

#[cfg(feature = "render")]
fn clamp_scroll_offset_for_geometry(
    state: &mut ObscuraState,
    requested: (f32, f32),
) -> (f32, f32) {
    clamp_scroll_offset_for_consumer(state, requested, true)
}

#[cfg(feature = "render")]
fn clamp_scroll_offset_for_consumer(
    state: &mut ObscuraState,
    requested: (f32, f32),
    geometry_only: bool,
) -> (f32, f32) {
    let prepared = if geometry_only {
        ensure_prepared_geometry(state)
    } else {
        ensure_prepared_render(state)
    };
    let clamped = prepared
        .map(|prepared| prepared.clamp_scroll(requested))
        .unwrap_or((0.0, 0.0));
    if state.scroll_offset != clamped {
        state.scroll_offset = clamped;
        state.activity_generation = state.activity_generation.wrapping_add(1);
        state.scroll_generation = state.scroll_generation.wrapping_add(1);
        state.resolved_scroll = None;
    }
    state.scroll_offset
}

/// Real border-box geometry for an element from the obscura-render layout
/// cache. The cache is computed lazily on first read and cleared on navigation
/// (see `set_dom`). Coordinates are viewport-relative after the shared root
/// scroll offset, except for viewport-fixed subtrees. Returns JSON
/// `{"x","y","width","height","clientWidth","clientHeight","clientRects"}`
/// in CSS pixels, or an empty string when the node has no box. The client
/// dimensions are the unscaled padding box used by CSSOM View, `clientRects`
/// retains every inline continuation, and the top-level rect is their visual
/// viewport-relative bounding union. Feature-gated.
#[cfg(feature = "render")]
#[op2]
#[string]
fn op_layout_geometry(state: &OpState, #[string] nid_str: String) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let nid: u32 = nid_str.parse().unwrap_or(0);
    let nid = obscura_dom::tree::NodeId::new(nid);
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    if ensure_resolved_scroll_for_geometry(&mut gs).is_some() {
        let Some((_, scroll)) = gs.resolved_scroll.as_ref() else {
            return String::new();
        };
        let Some(prepared) = gs.prepared_render.as_ref() else {
            return String::new();
        };
        let Some(rect) = prepared.viewport_rect_with_scroll(nid, scroll) else {
            return String::new();
        };
        let Some((client_width, client_height)) = prepared.client_size(nid) else {
            return String::new();
        };
        let Some(client_rects) = prepared.viewport_client_rects_with_scroll(nid, scroll) else {
            return String::new();
        };
        let client_rects = client_rects
            .into_iter()
            .map(|rect| {
                serde_json::json!({
                    "x": rect.x,
                    "y": rect.y,
                    "width": rect.width,
                    "height": rect.height,
                })
            })
            .collect::<Vec<_>>();
        let viewport_fixed = prepared.viewport_fixed_nodes().contains(&nid);
        return serde_json::json!({
            "x": rect.x,
            "y": rect.y,
            "width": rect.width,
            "height": rect.height,
            "clientWidth": client_width,
            "clientHeight": client_height,
            "clientRects": client_rects,
            "viewportFixed": viewport_fixed,
        })
        .to_string();
    }
    String::new()
}

/// Measure every target in one ResizeObserver rendering opportunity.
///
/// ResizeObserver gathers all observations before it invokes any callback.
/// Crossing the JS/native boundary once per target defeated that batching:
/// each read sampled the document timeline and could rebuild the retained
/// cascade/layout independently.  Accept the complete target list, freeze the
/// animation sample once, prepare/resolve layout once, and return the small
/// computed-style subset needed to derive content/border/device-pixel boxes.
/// The result is index-aligned with the input and contains `null` for targets
/// which currently generate no box (detached, `display:none`, and stale ids).
#[cfg(feature = "render")]
#[op2]
#[string]
fn op_resize_observer_measurements(state: &OpState, #[string] nids_json: String) -> String {
    let nids = serde_json::from_str::<Vec<u32>>(&nids_json).unwrap_or_default();
    if nids.is_empty() {
        return "[]".to_string();
    }

    let shared = state.borrow::<SharedState>().clone();
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    if ensure_resolved_scroll_for_geometry(&mut gs).is_none() {
        return serde_json::to_string(&vec![serde_json::Value::Null; nids.len()])
            .unwrap_or_else(|_| "[]".to_string());
    }
    let Some((_, scroll)) = gs.resolved_scroll.as_ref() else {
        return serde_json::to_string(&vec![serde_json::Value::Null; nids.len()])
            .unwrap_or_else(|_| "[]".to_string());
    };
    let Some(prepared) = gs.prepared_render.as_ref() else {
        return serde_json::to_string(&vec![serde_json::Value::Null; nids.len()])
            .unwrap_or_else(|_| "[]".to_string());
    };

    let style_value =
        |snapshot: &std::collections::HashMap<&'static str, String>, name: &'static str| {
            snapshot.get(name).cloned().unwrap_or_default()
        };
    let measurements = nids
        .into_iter()
        .map(|nid| {
            let nid = obscura_dom::tree::NodeId::new(nid);
            let rect = prepared.viewport_rect_with_scroll(nid, scroll)?;
            let (client_width, client_height) = prepared.client_size(nid)?;
            let snapshot = prepared.computed_style(nid)?;
            Some(serde_json::json!({
                "x": rect.x,
                "y": rect.y,
                "clientWidth": client_width,
                "clientHeight": client_height,
                "paddingTop": style_value(&snapshot, "padding-top"),
                "paddingRight": style_value(&snapshot, "padding-right"),
                "paddingBottom": style_value(&snapshot, "padding-bottom"),
                "paddingLeft": style_value(&snapshot, "padding-left"),
                "borderTopWidth": style_value(&snapshot, "border-top-width"),
                "borderRightWidth": style_value(&snapshot, "border-right-width"),
                "borderBottomWidth": style_value(&snapshot, "border-bottom-width"),
                "borderLeftWidth": style_value(&snapshot, "border-left-width"),
                "writingMode": style_value(&snapshot, "writing-mode"),
                "display": style_value(&snapshot, "display"),
            }))
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&measurements).unwrap_or_else(|_| "[]".to_string())
}

/// Measure the complete IntersectionObserver clip graph in one rendering
/// opportunity. The JS side supplies the unique observed targets, element
/// roots, and intervening element ancestors. Sampling animations and preparing
/// layout once here avoids turning each target/ancestor box and style read into
/// a separate retained-layout rebuild.
///
/// Results are index-aligned with the input. A `null` entry means that the node
/// currently generates no layout box (for example, it is detached or hidden).
#[cfg(feature = "render")]
#[op2]
#[string]
fn op_intersection_observer_measurements(
    state: &OpState,
    #[string] nids_json: String,
) -> String {
    let nids = serde_json::from_str::<Vec<u32>>(&nids_json).unwrap_or_default();
    if nids.is_empty() {
        return "[]".to_string();
    }

    let shared = state.borrow::<SharedState>().clone();
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    if ensure_resolved_scroll_for_geometry(&mut gs).is_none() {
        return serde_json::to_string(&vec![serde_json::Value::Null; nids.len()])
            .unwrap_or_else(|_| "[]".to_string());
    }
    let Some((_, scroll)) = gs.resolved_scroll.as_ref() else {
        return serde_json::to_string(&vec![serde_json::Value::Null; nids.len()])
            .unwrap_or_else(|_| "[]".to_string());
    };
    let Some(prepared) = gs.prepared_render.as_ref() else {
        return serde_json::to_string(&vec![serde_json::Value::Null; nids.len()])
            .unwrap_or_else(|_| "[]".to_string());
    };

    let style_value =
        |snapshot: &std::collections::HashMap<&'static str, String>, name: &'static str| {
            snapshot.get(name).cloned().unwrap_or_default()
        };
    let measurements = nids
        .into_iter()
        .map(|nid| {
            let nid = obscura_dom::tree::NodeId::new(nid);
            let rect = prepared.viewport_rect_with_scroll(nid, scroll)?;
            let (client_width, client_height) = prepared.client_size(nid)?;
            let snapshot = prepared.computed_style(nid)?;
            Some(serde_json::json!({
                "x": rect.x,
                "y": rect.y,
                "width": rect.width,
                "height": rect.height,
                "clientWidth": client_width,
                "clientHeight": client_height,
                "borderTopWidth": style_value(&snapshot, "border-top-width"),
                "borderLeftWidth": style_value(&snapshot, "border-left-width"),
                "overflowX": style_value(&snapshot, "overflow-x"),
                "overflowY": style_value(&snapshot, "overflow-y"),
            }))
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&measurements).unwrap_or_else(|_| "[]".to_string())
}

/// One renderer-computed CSS snapshot for `getComputedStyle()`. Returning all
/// supported properties together keeps a single JS style object to one native
/// call and one use of the retained prepared layout.
#[cfg(feature = "render")]
#[op2]
#[string]
fn op_computed_style(state: &OpState, #[string] nid_str: String) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let nid: u32 = nid_str.parse().unwrap_or(0);
    let nid = obscura_dom::tree::NodeId::new(nid);
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    let Some(prepared) = ensure_prepared_render(&mut gs) else {
        return String::new();
    };
    let Some(snapshot) = prepared.computed_style(nid) else {
        return String::new();
    };
    let custom = prepared.computed_custom_properties(nid).unwrap_or_default();
    let mut object = serde_json::Map::with_capacity(snapshot.len() + custom.len());
    for (name, value) in snapshot {
        object.insert(name.to_string(), serde_json::Value::String(value));
    }
    for (name, value) in custom {
        object.insert(name, serde_json::Value::String(value));
    }
    serde_json::Value::Object(object).to_string()
}

/// Use the renderer's declaration parser as the single feature-query source
/// of truth. Keeping this bridge synchronous and state-free makes the common
/// two-argument `CSS.supports()` overload a single native call.
#[cfg(feature = "render")]
#[op2(fast)]
fn op_css_supports(#[string] name: &str, #[string] value: &str) -> bool {
    obscura_render::style::supports_declaration(name, value)
}

/// Root scrolling overflow in CSS pixels. The JS CSSOM probes this op only in
/// render builds; default scraping builds retain their deliberately unbounded
/// synthetic scrolling behavior.
#[cfg(feature = "render")]
#[op2]
#[string]
fn op_layout_metrics(state: &OpState) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    let viewport = gs.viewport;
    let content = ensure_prepared_geometry(&mut gs)
        .map(|prepared| prepared.content_size())
        .unwrap_or(viewport);
    format!(
        "{{\"scrollWidth\":{},\"scrollHeight\":{},\"clientWidth\":{},\"clientHeight\":{}}}",
        content.0, content.1, viewport.0, viewport.1
    )
}

#[cfg(feature = "render")]
#[op2]
#[string]
fn op_element_scroll_metrics(state: &OpState, #[string] nid_str: String) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let nid = NodeId::new(nid_str.parse().unwrap_or(0));
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    if ensure_resolved_scroll_for_geometry(&mut gs).is_none() {
        return String::new();
    }
    let Some((_, scroll)) = gs.resolved_scroll.as_ref() else {
        return String::new();
    };
    let Some(metrics) = gs
        .prepared_render
        .as_ref()
        .and_then(|prepared| prepared.element_scroll_metrics(nid, scroll))
    else {
        // The op exists in render builds, so an unboxed/detached node must not
        // fall through to bootstrap's synthetic non-render metrics.
        return r#"{"scrollWidth":0,"scrollHeight":0,"clientWidth":0,"clientHeight":0,"x":0,"y":0,"maxX":0,"maxY":0,"hasBox":false}"#.to_string();
    };
    serde_json::json!({
        "scrollWidth": metrics.content_size.0,
        "scrollHeight": metrics.content_size.1,
        "clientWidth": metrics.client_size.0,
        "clientHeight": metrics.client_size.1,
        "x": metrics.offset.0,
        "y": metrics.offset.1,
        "maxX": metrics.max_offset.0,
        "maxY": metrics.max_offset.1,
        "hasBox": true,
    })
    .to_string()
}

#[cfg(feature = "render")]
#[op2]
#[string]
fn op_element_scroll_to(state: &OpState, #[string] nid_str: String, x: f64, y: f64) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let nid = NodeId::new(nid_str.parse().unwrap_or(0));
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    if ensure_resolved_scroll_for_geometry(&mut gs).is_none() {
        return String::new();
    }
    let current = gs.resolved_scroll.as_ref().and_then(|(_, scroll)| {
        gs.prepared_render
            .as_ref()?
            .element_scroll_metrics(nid, scroll)
    });
    let Some(current) = current else {
        return String::new();
    };
    let clamp = |value: f64, max: f32| {
        if value.is_finite() {
            obscura_render::quantize_scroll_value(value as f32, 1.0).clamp(0.0, max)
        } else {
            0.0
        }
    };
    let requested = (
        clamp(x, current.max_offset.0),
        clamp(y, current.max_offset.1),
    );
    if requested != current.offset {
        if requested == (0.0, 0.0) {
            gs.element_scroll_offsets.remove(&nid);
        } else {
            gs.element_scroll_offsets.insert(nid, requested);
        }
        gs.activity_generation = gs.activity_generation.wrapping_add(1);
        gs.scroll_generation = gs.scroll_generation.wrapping_add(1);
        gs.resolved_scroll = None;
        return format!("{{\"x\":{},\"y\":{}}}", requested.0, requested.1);
    }
    format!("{{\"x\":{},\"y\":{}}}", current.offset.0, current.offset.1)
}

#[cfg(feature = "render")]
#[op2]
#[string]
fn op_scroll_offset(state: &OpState) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    let requested = gs.scroll_offset;
    let (x, y) = clamp_scroll_offset_for_geometry(&mut gs, requested);
    format!("{{\"x\":{},\"y\":{}}}", x, y)
}

#[cfg(feature = "render")]
#[op2]
#[string]
fn op_scroll_to(state: &OpState, x: f64, y: f64) -> String {
    let shared = state.borrow::<SharedState>().clone();
    let mut gs = shared.borrow_mut();
    sample_live_document_animations(&mut gs);
    let (x, y) = clamp_scroll_offset_for_geometry(&mut gs, (x as f32, y as f32));
    format!("{{\"x\":{},\"y\":{}}}", x, y)
}
