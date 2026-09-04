use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use obscura_browser::lifecycle::LifecycleState;
use obscura_browser::{BrowserContext, Page};
use obscura_js::ops::InterceptedRequest;
use serde_json::json;

use crate::domains;
use crate::domains::fetch::FetchInterceptState;
use crate::types::{CdpEvent, CdpRequest, CdpResponse};

#[cfg(feature = "render")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScreencastFormat {
    Png,
    Jpeg,
}

#[cfg(feature = "render")]
#[derive(Clone, Debug)]
pub(crate) struct ScreencastState {
    pub format: ScreencastFormat,
    pub quality: u8,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub every_nth_frame: u32,
    pub command_frame_counter: u64,
    pub session_id: i64,
    pub frames_in_flight: u8,
    /// Last connected-document generation observed by the frame producer.
    /// The autonomous pump uses this as a cheap compositor-damage signal, so
    /// an idle screencast does not continuously rasterize identical frames.
    pub observed_activity_generation: u64,
    /// A changed generation remains pending across sampling skips and
    /// acknowledgement backpressure. Once capacity returns, the newest page
    /// state is captured instead of losing the change.
    pub autonomous_frame_pending: bool,
}

pub struct CdpContext {
    pub pages: Vec<Page>,
    pub sessions: HashMap<String, String>, // session_id -> page_id
    /// Current document loader per page. Navigation events and later
    /// script-initiated Network events must share this id; inventing a loader
    /// for each fetch breaks DevTools request grouping.
    pub current_loader_ids: HashMap<String, String>,
    pub(crate) emitted_document_lifecycle: HashMap<String, LifecycleState>,
    pub(crate) navigation_sessions: HashMap<String, Option<String>>,
    /// Child frame ids already reported to the client, per page, so each frame
    /// is announced once and a frame that goes away can be retracted.
    pub announced_frames: HashMap<String, Vec<String>>,
    pub pending_events: Vec<CdpEvent>,
    #[cfg(feature = "render")]
    pub(crate) screencasts: HashMap<String, ScreencastState>,
    #[cfg(feature = "render")]
    next_screencast_session_id: i64,
    pub default_context: Arc<BrowserContext>,
    pub browser_contexts: HashMap<String, Arc<BrowserContext>>,
    page_counter: u32,
    browser_context_counter: u32,
    target_session_counter: u64,
    pub preload_scripts: Vec<(String, String)>, // (identifier, source)
    pub preload_counter: u32,
    // Which sessions asked for each `Runtime.addBinding` name. A binding is a
    // session-scoped subscription in CDP, and a client discards any event whose
    // sessionId is not one it holds, so the call has to go back to the session
    // that registered the name rather than to whichever session of the page
    // happens to come first out of a HashMap.
    pub binding_sessions: HashMap<String, Vec<String>>, // binding name -> session ids
    /// Sessions that called Runtime.enable. Console and exception events are
    /// page-scoped but only delivered to these subscribers.
    pub runtime_enabled_sessions: HashSet<String>,
    // World names registered via Page.createIsolatedWorld. After every
    // navigation Obscura clears execution contexts (via
    // Runtime.executionContextsCleared) and must re-emit a
    // Runtime.executionContextCreated for each registered world, otherwise
    // Playwright/Puppeteer hang waiting for their utility world to come
    // back. Stored as plain Strings (not by-page) — for now we only model
    // a single page in CdpContext anyway.
    pub isolated_worlds: Vec<String>,
    // Set of executionContextIds Obscura has emitted via
    // Runtime.executionContextCreated. Pre-populated with the default-frame
    // contexts (`1`, `2`) that Runtime.enable / Page.navigate emit, then
    // extended each time Page.createIsolatedWorld assigns a fresh id.
    //
    // Runtime.evaluate / Runtime.callFunctionOn consult this set to reject
    // requests targeting an unknown context — matching real Chrome's
    // "Cannot find context with specified id" CDP error and unblocking the
    // Playwright locator path described in issue #51.
    pub valid_context_ids: HashSet<i64>,
    // Monotonic counter for isolated-world execution context ids. Issue #192:
    // both `Page.createIsolatedWorld` and `do_navigate` used to hardcode id
    // 100, so re-creating a context (e.g. Playwright opening a second page or
    // re-navigating) silently emitted the same id twice and the client's
    // bookkeeping diverged from the server's. Each fresh isolated context now
    // claims and increments from this counter so the ids real Chrome would
    // emit (incrementing, never reused) are mirrored.
    pub next_isolated_context_id: i64,
    pub fetch_intercept: FetchInterceptState,
    pub intercept_tx: Option<tokio::sync::mpsc::UnboundedSender<InterceptedRequest>>,
    // Open IO streams for Fetch.takeResponseBodyAsStream. Each holds a response
    // body taken out of the page cache so a large download is streamed
    // chunk-by-chunk via IO.read and freed on IO.close (issue #360). The store
    // caps how many bodies (and how many bytes) can be held at once, evicting
    // the oldest, so an abandoned or disconnected stream cannot leak unbounded.
    pub io_streams: crate::domains::io::IoStreamStore,
    /// Serializes V8 work within THIS connection. With the thread-per-connection
    /// server (#430) each connection runs on its own OS thread, so isolates never
    /// collide across connections; this per-connection lock keeps a connection's
    /// own nav task and command dispatch from interleaving two of its pages'
    /// isolates on that one thread. It is deliberately per-connection, not a
    /// process-wide lock, so connections run in parallel (measured ~2x at
    /// concurrency 2, ~3x at 4) instead of serializing all V8 on one mutex.
    pub v8_lock: Arc<tokio::sync::Mutex<()>>,
}

impl CdpContext {
    pub fn new() -> Self {
        Self::new_with_options(None, false)
    }

    pub fn new_with_proxy(proxy: Option<String>) -> Self {
        Self::new_with_options(proxy, false)
    }

    pub fn new_with_options(proxy: Option<String>, stealth: bool) -> Self {
        Self::new_with_full_options(proxy, stealth, None)
    }

    pub fn new_with_full_options(
        proxy: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
    ) -> Self {
        Self::new_with_security(proxy, stealth, user_agent, false)
    }

    pub fn new_with_storage(
        proxy: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self::_new_inner(proxy, stealth, user_agent, storage_dir, false, false)
    }

    pub fn new_with_security(
        proxy: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        allow_file_access: bool,
    ) -> Self {
        Self::_new_inner(proxy, stealth, user_agent, None, allow_file_access, false)
    }

    /// Build a CDP context around an already-constructed default browser
    /// context. The server passes a fresh isolated context per WebSocket; tests
    /// and embedders may construct their own.
    pub fn new_with_shared_context(default_context: Arc<BrowserContext>) -> Self {
        // Pre-seed with the default-frame execution context ids that
        // `Runtime.enable` (1) and post-navigation re-emission (2) advertise via
        // Runtime.executionContextCreated. Anything else has to be registered
        // explicitly (Page.createIsolatedWorld), otherwise
        // Runtime.{evaluate,callFunctionOn} should reject it per CDP spec.
        let mut valid_context_ids = HashSet::new();
        valid_context_ids.insert(1);
        valid_context_ids.insert(2);
        CdpContext {
            pages: Vec::new(),
            sessions: HashMap::new(),
            current_loader_ids: HashMap::new(),
            emitted_document_lifecycle: HashMap::new(),
            navigation_sessions: HashMap::new(),
            announced_frames: HashMap::new(),
            pending_events: Vec::new(),
            #[cfg(feature = "render")]
            screencasts: HashMap::new(),
            #[cfg(feature = "render")]
            next_screencast_session_id: 0,
            default_context,
            browser_contexts: HashMap::new(),
            page_counter: 0,
            browser_context_counter: 0,
            target_session_counter: 0,
            preload_scripts: Vec::new(),
            binding_sessions: HashMap::new(),
            runtime_enabled_sessions: HashSet::new(),
            preload_counter: 0,
            fetch_intercept: FetchInterceptState::new(),
            intercept_tx: None,
            isolated_worlds: Vec::new(),
            valid_context_ids,
            next_isolated_context_id: 100,
            io_streams: crate::domains::io::IoStreamStore::default(),
            v8_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn _new_inner(
        proxy: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<std::path::PathBuf>,
        allow_file_access: bool,
        allow_private_network: bool,
    ) -> Self {
        let mut ctx = BrowserContext::with_storage_and_network(
            "default".to_string(),
            proxy,
            stealth,
            user_agent,
            storage_dir,
            allow_private_network,
        );
        ctx.allow_file_access = allow_file_access;
        Self::new_with_shared_context(Arc::new(ctx))
    }

    /// Claim the next isolated-world execution context id and register it as
    /// valid for `Runtime.evaluate`/`callFunctionOn`. Issue #192.
    pub fn next_isolated_context(&mut self) -> i64 {
        let id = self.next_isolated_context_id;
        self.next_isolated_context_id += 1;
        self.valid_context_ids.insert(id);
        id
    }

    pub fn create_page(&mut self) -> String {
        self.create_page_in_context(None)
            .expect("default browser context must exist")
    }

    pub fn create_page_in_context(&mut self, context_id: Option<&str>) -> Result<String, String> {
        let context = match context_id {
            Some(id) => self
                .browser_context(id)
                .cloned()
                .ok_or_else(|| format!("Browser context not found: {}", id))?,
            None => self.default_context.clone(),
        };
        self.page_counter += 1;
        let page_id = format!("page-{}", self.page_counter);
        let mut page = Page::new(page_id.clone(), context);
        page.navigate_blank();
        self.pages.push(page);
        self.current_loader_ids
            .insert(page_id.clone(), format!("loader-blank-{page_id}"));
        Ok(page_id)
    }

    pub fn browser_context(&self, id: &str) -> Option<&Arc<BrowserContext>> {
        if id == self.default_context.id {
            Some(&self.default_context)
        } else {
            self.browser_contexts.get(id)
        }
    }

    pub fn create_browser_context(&mut self) -> String {
        self.browser_context_counter += 1;
        let id = format!("context-{}", self.browser_context_counter);
        let context = Arc::new(self.default_context.isolated_copy(id.clone(), false));
        self.browser_contexts.insert(id.clone(), context);
        id
    }

    /// Allocate a distinct CDP session for every explicit target attachment.
    /// A target may have more than one client session at a time (for example,
    /// Playwright's managed page session plus `newCDPSession(page)`). Reusing
    /// the page's auto-attach session id makes the client's session registry
    /// overwrite the original route.
    pub(crate) fn next_target_session(&mut self, target_id: &str) -> String {
        self.target_session_counter = self.target_session_counter.saturating_add(1);
        format!("{target_id}-session-{}", self.target_session_counter)
    }

    pub fn dispose_browser_context(&mut self, id: &str) -> Result<Vec<String>, String> {
        if id == self.default_context.id {
            return Err("The default browser context cannot be disposed".to_string());
        }
        if self.browser_contexts.remove(id).is_none() {
            return Err(format!("Browser context not found: {}", id));
        }
        let page_ids: Vec<String> = self
            .pages
            .iter()
            .filter(|page| page.context.id == id)
            .map(|page| page.id.clone())
            .collect();
        for page_id in &page_ids {
            self.remove_page(page_id);
        }
        Ok(page_ids)
    }

    pub fn get_page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    pub fn get_page_mut(&mut self, id: &str) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    pub(crate) fn refresh_runtime_event_collection(&self, page_id: &str) {
        let enabled = self.runtime_enabled_sessions.iter().any(|session_id| {
            self.sessions
                .get(session_id)
                .is_some_and(|owner| owner == page_id)
        });
        if let Some(page) = self.get_page(page_id) {
            page.set_runtime_events_enabled(enabled);
        }
    }

    pub fn remove_page(&mut self, id: &str) {
        let removed_sessions: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, page_id)| page_id.as_str() == id)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        self.pages.retain(|p| p.id != id);
        self.current_loader_ids.remove(id);
        self.emitted_document_lifecycle.remove(id);
        self.navigation_sessions.remove(id);
        self.announced_frames.remove(id);
        #[cfg(feature = "render")]
        {
            for session_id in &removed_sessions {
                self.screencasts.remove(session_id);
            }
        }
        for session_id in &removed_sessions {
            self.runtime_enabled_sessions.remove(session_id);
        }
        self.sessions.retain(|_, v| v != id);
    }

    /// Destroy a target through the single ownership-cleanup path used by both
    /// ordinary Target commands and navigation cancellation. A task-owned page
    /// is temporarily absent from `pages`, but its sessions still identify it.
    pub(crate) fn destroy_target(&mut self, id: &str) -> bool {
        let removed_sessions: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, page_id)| page_id.as_str() == id)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        let existed = !removed_sessions.is_empty() || self.pages.iter().any(|page| page.id == id);
        if !existed {
            return false;
        }
        for session_id in &removed_sessions {
            self.pending_events.push(CdpEvent::new(
                "Target.detachedFromTarget",
                json!({"sessionId": session_id, "targetId": id}),
            ));
        }
        self.pending_events.push(CdpEvent::new(
            "Target.targetDestroyed",
            json!({"targetId": id}),
        ));
        self.remove_page(id);
        true
    }

    /// Dispose a context, including a page currently owned by an in-flight
    /// navigation task and therefore absent from `pages`.
    pub(crate) fn destroy_browser_context(
        &mut self,
        id: &str,
        task_owned_page: Option<&str>,
    ) -> Result<(), String> {
        let mut page_ids: Vec<String> = self
            .pages
            .iter()
            .filter(|page| page.context.id == id)
            .map(|page| page.id.clone())
            .collect();
        if let Some(page_id) = task_owned_page {
            if self.pages.iter().any(|page| page.id == page_id)
                || self.sessions.values().any(|owner| owner == page_id)
            {
                page_ids.push(page_id.to_string());
            }
        }
        page_ids.sort_unstable();
        page_ids.dedup();
        let removed_sessions: Vec<(String, String)> = self
            .sessions
            .iter()
            .filter(|(_, owner)| page_ids.contains(owner))
            .map(|(session, owner)| (session.clone(), owner.clone()))
            .collect();
        self.dispose_browser_context(id)?;
        if let Some(page_id) = task_owned_page {
            self.remove_page(page_id);
        }
        for (session_id, page_id) in removed_sessions {
            self.pending_events.push(CdpEvent::new(
                "Target.detachedFromTarget",
                json!({"sessionId": session_id, "targetId": page_id}),
            ));
        }
        for page_id in page_ids {
            self.pending_events.push(CdpEvent::new(
                "Target.targetDestroyed",
                json!({"targetId": page_id}),
            ));
        }
        Ok(())
    }

    #[cfg(feature = "render")]
    pub(crate) fn next_screencast_session(&mut self) -> i64 {
        // Never wrap a delayed acknowledgement onto a replacement stream.
        self.next_screencast_session_id = self.next_screencast_session_id.saturating_add(1);
        self.next_screencast_session_id
    }

    pub fn get_session_page(&self, session_id: &Option<String>) -> Option<&Page> {
        let page_id = session_id.as_ref().and_then(|sid| self.sessions.get(sid))?;
        self.get_page(page_id)
    }

    pub fn get_session_page_mut(&mut self, session_id: &Option<String>) -> Option<&mut Page> {
        let page_id = session_id
            .as_ref()
            .and_then(|sid| self.sessions.get(sid))
            .cloned()?;

        let target_has_js = self.pages.iter().any(|p| p.id == page_id && p.has_js());

        if !target_has_js {
            for page in &mut self.pages {
                if page.id != page_id && page.has_js() {
                    page.suspend_js();
                    break;
                }
            }
            if let Some(target) = self.pages.iter_mut().find(|p| p.id == page_id) {
                target.resume_js();
            }
        }

        self.get_page_mut(&page_id)
    }
}

/// Whether a CDP method can be served WITHOUT acquiring the per-connection V8 lock.
///
/// Methods listed here were audited to confirm they do not transitively
/// call into a `JsRuntime`. They either don't touch any `Page` at all, or
/// use only the immutable `get_session_page` accessor and Rust-side field
/// reads. `get_session_page_mut` triggers `suspend_js`/`resume_js` and
/// must stay behind the lock.
fn is_v8_free_method(method: &str) -> bool {
    matches!(
        method,
        "Target.getTargets"
            | "Target.setDiscoverTargets"
            | "Target.attachToTarget"
            | "Target.attachToBrowserTarget"
            | "Target.setAutoAttach"
            | "Target.getBrowserContexts"
            | "Target.createBrowserContext"
            | "Target.disposeBrowserContext"
            | "Target.getTargetInfo"
            | "Target.detachFromTarget"
            | "Target.activateTarget"
            | "Browser.getVersion"
            | "Browser.close"
            | "Browser.getWindowForTarget"
            | "Browser.setDownloadBehavior"
            | "Browser.getWindowBounds"
            | "Browser.setWindowBounds"
            | "Page.enable"
            | "Page.disable"
            | "Page.getFrameTree"
            | "Page.setDownloadBehavior"
            | "Page.setLifecycleEventsEnabled"
            | "Page.addScriptToEvaluateOnNewDocument"
            | "Page.removeScriptToEvaluateOnNewDocument"
            | "Page.setInterceptFileChooserDialog"
            | "Page.getNavigationHistory"
            | "Page.resetNavigationHistory"
            | "Page.captureSnapshot"
            | "Page.stopScreencast"
            | "Page.screencastFrameAck"
            | "Page.createIsolatedWorld"
            | "Runtime.enable"
            | "Runtime.disable"
            | "Runtime.runIfWaitingForDebugger"
            | "Runtime.getExceptionDetails"
            | "Runtime.discardConsoleEntries"
            | "Network.enable"
            | "Network.disable"
            | "Network.setCacheDisabled"
            | "Network.setRequestInterception"
            | "Network.setBlockedURLs"
            | "Network.setExtraHTTPHeaders"
            | "Network.setUserAgentOverride"
            | "Network.getCookies"
            | "Network.getAllCookies"
            | "Network.setCookie"
            | "Network.setCookies"
            | "Network.deleteCookies"
            | "Network.clearBrowserCookies"
            | "Network.getResponseBody"
            | "Fetch.continueRequest"
            | "Fetch.fulfillRequest"
            | "Fetch.failRequest"
            | "Fetch.getResponseBody"
            | "IO.read"
            | "IO.close"
            | "Storage.getCookies"
            | "Storage.setCookies"
            | "Storage.clearCookies"
            | "Storage.deleteCookies"
    )
}

pub async fn dispatch(req: &CdpRequest, ctx: &mut CdpContext) -> CdpResponse {
    // headless_chrome (and older Puppeteer) wrap every CDP call inside
    // Target.sendMessageToTarget. Unwrap and recurse BEFORE acquiring the
    // per-connection V8 lock — the recursive dispatch will acquire it for the
    // inner call, and tokio Mutex is not reentrant.
    if req.method == "Target.sendMessageToTarget" {
        return dispatch_send_message_to_target(req, ctx).await;
    }

    // Issue #430: keep this connection's V8 work serialized on its own thread.
    //
    // Every CDP handler below may call into a per-Page `JsRuntime` (each owning
    // its own V8 Isolate). With the thread-per-connection server each connection
    // runs on its own OS thread, so isolates never collide across connections.
    // Within one connection, though, a nav task spawned by
    // `process_with_interception` runs while this processor keeps pumping other
    // CDP messages, so two of this connection's pages could still interleave V8
    // work on this one thread and trip
    // `heap->isolate() == Isolate::TryGetCurrent()`. The per-connection lock
    // (`ctx.v8_lock`) keeps each handler contiguous: V8 fully exits one Isolate
    // before the next of this connection's pages is allowed in. It is
    // per-connection, not process-wide, so other connections run in parallel.
    //
    // Optimization: methods that demonstrably never touch V8 bypass the lock
    // (Puppeteer's newPage() setup issues ~8 such calls). Each listed method was
    // audited to confirm it never reaches `JsRuntime::execute_script` or DOM
    // mutation that re-enters V8; `get_session_page_mut` (which can trigger
    // `suspend_js`/`resume_js`) is NOT in the list.
    let _v8_guard = if is_v8_free_method(&req.method) {
        None
    } else {
        // Per-connection lock (owned guard, so it does not borrow `ctx`): keeps
        // this connection's own V8 work contiguous on its thread without
        // serializing other connections (#430).
        Some(ctx.v8_lock.clone().lock_owned().await)
    };

    // Per-command V8 watchdog. The lock above keeps each handler contiguous on
    // the thread, but it does not bound how long a handler runs: a hung page (a
    // runaway Runtime.evaluate, a synchronous DOM op) would hold this
    // connection's V8 lock and wedge its other sessions forever. The one-shot
    // CLI uses a process-level hard deadline for this; the long-running server
    // cannot force-exit, so we terminate just the offending isolate instead.
    // OBSCURA_CDP_COMMAND_TIMEOUT_MS tunes the bound (0 disables); the default
    // leaves headroom for the slowest legitimate navigation, which already
    // self-bounds via OBSCURA_NAV_TIMEOUT_MS plus the watchdog-bounded settle.
    let cmd_budget_ms: u64 = std::env::var("OBSCURA_CDP_COMMAND_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60_000);
    let cmd_watchdog = if cmd_budget_ms == 0 || is_v8_free_method(&req.method) {
        None
    } else {
        ctx.get_session_page(&req.session_id)
            .and_then(|p| p.isolate_handle())
            .map(|h| {
                obscura_js::cdp_watchdog::arm(h, std::time::Duration::from_millis(cmd_budget_ms))
            })
    };

    let (domain, method) = match req.method.split_once('.') {
        Some((d, m)) => (d, m),
        None => {
            return CdpResponse::error(
                req.id,
                -32601,
                format!("Invalid method format: {}", req.method),
                req.session_id.clone(),
            );
        }
    };

    let result = match domain {
        "Target" => domains::target::handle(method, &req.params, ctx, &req.session_id).await,
        "Browser" => domains::browser::handle(method, &req.params).await,
        "Page" => domains::page::handle(method, &req.params, ctx, &req.session_id).await,
        "DOM" => domains::dom::handle(method, &req.params, ctx, &req.session_id).await,
        "DOMSnapshot" => {
            domains::domsnapshot::handle(method, &req.params, ctx, &req.session_id).await
        }
        "Runtime" => domains::runtime::handle(method, &req.params, ctx, &req.session_id).await,
        "Network" => domains::network::handle(method, &req.params, ctx, &req.session_id).await,
        "Fetch" => domains::fetch::handle(method, &req.params, ctx, &req.session_id).await,
        "IO" => domains::io::handle(method, &req.params, ctx).await,
        "Input" => domains::input::handle(method, &req.params, ctx, &req.session_id).await,
        "Emulation" => domains::emulation::handle(method, &req.params, ctx, &req.session_id).await,
        "Storage" => domains::storage::handle(method, &req.params, ctx, &req.session_id).await,
        "LP" => domains::lp::handle(method, &req.params, ctx, &req.session_id).await,
        "Accessibility" => {
            domains::accessibility::handle(method, &req.params, ctx, &req.session_id).await
        }
        // Accepted but no-op. Puppeteer's FrameManager.initialize calls
        // Audits.enable on connect — refusing it breaks puppeteer.connect()
        // before any user code runs.
        "Log" | "Performance" | "Security" | "CSS" | "ServiceWorker" | "Inspector" | "Debugger"
        | "Profiler" | "HeapProfiler" | "Overlay" | "Audits" => Ok(json!({})),
        _ => Err(format!("Unknown domain: {}", domain)),
    };

    #[cfg(feature = "render")]
    if result.is_ok() && domains::page::command_can_change_screencast_frame(&req.method) {
        if let Err(error) = domains::page::queue_screencast_frame(ctx, &req.session_id, false) {
            // Frame delivery is an asynchronous side effect in Chromium; it
            // must not rewrite an otherwise successful command response.
            tracing::warn!(method = %req.method,
                "could not produce command-driven screencast frame: {error}");
        }
    }

    // Stop the per-command watchdog. If it fired (the handler held V8 past the
    // budget), V8 is left in a terminating state, so clear that flag before the
    // next command runs on this page.
    if let Some(wd) = cmd_watchdog {
        if obscura_js::cdp_watchdog::disarm(wd) {
            tracing::warn!(
                "CDP command {} held V8 past {}ms; terminated the isolate to free the dispatcher",
                req.method,
                cmd_budget_ms
            );
            if let Some(page) = ctx.get_session_page_mut(&req.session_id) {
                page.cancel_v8_termination();
            }
        }
    }

    drain_runtime_events(ctx);
    drain_binding_calls(ctx);
    drain_frame_events(ctx);

    match result {
        Ok(value) => CdpResponse::success(req.id, value, req.session_id.clone()),
        Err(msg) => {
            tracing::warn!("CDP error for {}: {}", req.method, msg);
            CdpResponse::error(req.id, -32601, msg, req.session_id.clone())
        }
    }
}

pub(crate) fn drain_runtime_events(ctx: &mut CdpContext) {
    let mut page_to_sessions: HashMap<&str, Vec<&str>> = HashMap::new();
    for session_id in &ctx.runtime_enabled_sessions {
        if let Some(page_id) = ctx.sessions.get(session_id) {
            page_to_sessions
                .entry(page_id.as_str())
                .or_default()
                .push(session_id.as_str());
        }
    }
    for sessions in page_to_sessions.values_mut() {
        sessions.sort_unstable();
    }

    let mut events = Vec::new();
    for page in &mut ctx.pages {
        let runtime_events = page.take_pending_runtime_events();
        if runtime_events.is_empty() {
            continue;
        }
        let Some(sessions) = page_to_sessions.get(page.id.as_str()) else {
            continue;
        };
        let execution_context_id = if ctx
            .current_loader_ids
            .get(&page.id)
            .is_some_and(|loader_id| loader_id.starts_with("loader-blank-"))
        {
            1
        } else {
            2
        };
        for runtime_event in runtime_events {
            for session_id in sessions {
                let (method, params) = match &runtime_event {
                    obscura_js::ops::RuntimeEvent::Console(event) => (
                        "Runtime.consoleAPICalled",
                        json!({
                            "type": event.kind,
                            "args": event.args,
                            "executionContextId": execution_context_id,
                            "timestamp": event.timestamp,
                        }),
                    ),
                    obscura_js::ops::RuntimeEvent::Exception(event) => (
                        "Runtime.exceptionThrown",
                        json!({
                            "timestamp": event.timestamp,
                            "exceptionDetails": {
                                "exceptionId": event.exception_id,
                                "text": "Uncaught",
                                "lineNumber": event.line_number,
                                "columnNumber": event.column_number,
                                "scriptId": "",
                                "url": event.url,
                                "stackTrace": { "callFrames": event.stack_trace },
                                "executionContextId": execution_context_id,
                                "exception": {
                                    "type": "object",
                                    "subtype": "error",
                                    "className": event.name,
                                    "description": event.description,
                                },
                            },
                        }),
                    ),
                };
                events.push(CdpEvent {
                    method: method.to_string(),
                    params,
                    session_id: Some((*session_id).to_string()),
                });
            }
        }
    }
    ctx.pending_events.extend(events);
}

// Drain every page's binding-call queue (filled by op_binding_called when
// page JS invokes a `Runtime.addBinding` shim) and turn each entry into a
// Runtime.bindingCalled CDP event that the writer task forwards to the
// connected client. Called after every dispatch — binding calls only land
// in the queue while V8 is running inside a CDP handler, so there is no
// window in which they could pile up without a draining opportunity.
pub(crate) fn drain_binding_calls(ctx: &mut CdpContext) {
    // page_id -> every session on that page. A page commonly has more than one:
    // Target.createTarget opens a session and the Target.attachToTarget that
    // follows opens another, so a client that reaches a page the ordinary way
    // holds two and uses the second.
    let mut page_to_sessions: HashMap<&str, Vec<&str>> = HashMap::new();
    for (session_id, page_id) in &ctx.sessions {
        page_to_sessions
            .entry(page_id.as_str())
            .or_default()
            .push(session_id.as_str());
    }
    // ctx.sessions is a HashMap, so fix an order the events can be asserted in.
    for sessions in page_to_sessions.values_mut() {
        sessions.sort_unstable();
    }

    let mut events: Vec<CdpEvent> = Vec::new();
    for page in &mut ctx.pages {
        let calls = page.take_pending_binding_calls();
        if calls.is_empty() {
            continue;
        }
        let Some(page_sessions) = page_to_sessions.get(page.id.as_str()) else {
            // No session attached — drop the calls; there is no client to
            // deliver them to.
            continue;
        };
        for (name, payload) in calls {
            // The sessions that asked for this binding, narrowed to the page the
            // call came from. Falling back to every session of the page keeps a
            // binding that was installed without a session (a preload, or a
            // direct embedder) deliverable rather than silently dropped.
            let registered = ctx.binding_sessions.get(&name);
            let targets: Vec<&str> = page_sessions
                .iter()
                .copied()
                .filter(|session| {
                    registered.is_none_or(|owners| owners.iter().any(|owner| owner == session))
                })
                .collect();
            let targets = if targets.is_empty() {
                page_sessions.clone()
            } else {
                targets
            };
            for session_id in targets {
                events.push(CdpEvent {
                    method: "Runtime.bindingCalled".into(),
                    // Use executionContextId=2: the default main-frame context
                    // emitted post-navigation (see domains/page.rs phase1).
                    // Puppeteer matches on session_id + binding name and
                    // tolerates any registered context id.
                    params: json!({
                        "name": name,
                        "payload": payload,
                        "executionContextId": 2,
                    }),
                    session_id: Some(session_id.to_string()),
                });
            }
        }
    }
    ctx.pending_events.extend(events);
}

// Announce child frames the client has not been told about yet, and retract
// the ones that are gone.
//
// A frame is built when the page settles, which is not necessarily during the
// navigation that created it: script can add an iframe at any time, and the
// settle that gives it a realm may belong to a later command. Diffing here,
// after every dispatch, reports a frame whenever it actually appears instead
// of only at navigation, and is the same drain point binding calls use.
pub(crate) fn drain_frame_events(ctx: &mut CdpContext) {
    // Every session on the page, not just one: a client that reaches a page the
    // ordinary way holds two of them, because Target.createTarget opens a
    // session and the Target.attachToTarget that follows opens another. A
    // client drops any event whose sessionId is not the one it attached with,
    // so announcing to an arbitrary session is the same as not announcing.
    let mut page_to_sessions: HashMap<String, Vec<String>> = HashMap::new();
    for (session_id, page_id) in &ctx.sessions {
        page_to_sessions
            .entry(page_id.clone())
            .or_default()
            .push(session_id.clone());
    }
    // ctx.sessions is a HashMap, so fix an order the events can be asserted in.
    for sessions in page_to_sessions.values_mut() {
        sessions.sort();
    }

    let mut events: Vec<CdpEvent> = Vec::new();
    let mut announced: HashMap<String, Vec<String>> = HashMap::new();
    for page in &ctx.pages {
        let Some(session_ids) = page_to_sessions.get(&page.id) else {
            continue;
        };
        let live = crate::domains::page::child_frame_values(page);
        let known = ctx.announced_frames.get(&page.id);
        let live_ids: Vec<String> = live
            .iter()
            .map(|frame| frame["id"].as_str().unwrap_or_default().to_string())
            .collect();

        for frame in &live {
            let id = frame["id"].as_str().unwrap_or_default();
            if known.is_some_and(|ids| ids.iter().any(|seen| seen == id)) {
                continue;
            }
            for session_id in session_ids {
                // Attach before navigate: a client builds its frame from the
                // attach event and treats a navigation of a frame it has never
                // seen as a protocol error.
                events.push(CdpEvent {
                    method: "Page.frameAttached".into(),
                    params: json!({
                        "frameId": id,
                        "parentFrameId": frame["parentId"].as_str().unwrap_or_default(),
                    }),
                    session_id: Some(session_id.clone()),
                });
                events.push(CdpEvent {
                    method: "Page.frameNavigated".into(),
                    params: json!({ "frame": frame, "type": "Navigation" }),
                    session_id: Some(session_id.clone()),
                });
                // The frame's document scripts have already run by the time it
                // is in this list, so it is not still loading.
                events.push(CdpEvent {
                    method: "Page.frameStoppedLoading".into(),
                    params: json!({ "frameId": id }),
                    session_id: Some(session_id.clone()),
                });
            }
        }

        if let Some(known) = known {
            for id in known {
                if !live_ids.contains(id) {
                    for session_id in session_ids {
                        events.push(CdpEvent {
                            method: "Page.frameDetached".into(),
                            params: json!({ "frameId": id, "reason": "remove" }),
                            session_id: Some(session_id.clone()),
                        });
                    }
                }
            }
        }
        announced.insert(page.id.clone(), live_ids);
    }
    ctx.announced_frames.extend(announced);
    ctx.pending_events.extend(events);
}

async fn dispatch_send_message_to_target(req: &CdpRequest, ctx: &mut CdpContext) -> CdpResponse {
    let session_id = req
        .params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let message = match req.params.get("message").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return CdpResponse::error(
                req.id,
                -32602,
                "sendMessageToTarget requires a message string".into(),
                req.session_id.clone(),
            );
        }
    };

    let inner: CdpRequest = match serde_json::from_str(message) {
        Ok(r) => r,
        Err(e) => {
            return CdpResponse::error(
                req.id,
                -32700,
                format!("sendMessageToTarget message is not a valid CDP request: {e}"),
                req.session_id.clone(),
            );
        }
    };

    // Override the inner session with the one supplied by the wrapper so
    // the inner dispatch routes against the right page. Boxing the future
    // sidesteps the async-fn recursion limit.
    let inner_with_session = CdpRequest {
        id: inner.id,
        method: inner.method.clone(),
        params: inner.params,
        session_id: session_id.clone().or(inner.session_id),
    };
    let inner_response = Box::pin(dispatch(&inner_with_session, ctx)).await;

    // Re-emit the inner response as the legacy event headless_chrome (and
    // older Puppeteer) listen for instead of correlating responses by id.
    let inner_serialized = serde_json::to_string(&inner_response).unwrap_or_else(|_| "{}".into());
    ctx.pending_events.push(CdpEvent {
        method: "Target.receivedMessageFromTarget".to_string(),
        params: json!({
            "sessionId": session_id.clone().unwrap_or_default(),
            "message": inner_serialized,
            "targetId": session_id.clone().unwrap_or_default(),
        }),
        session_id: req.session_id.clone(),
    });

    CdpResponse::success(req.id, json!({}), req.session_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CdpRequest;

    fn req(method: &str) -> CdpRequest {
        CdpRequest {
            id: 1,
            method: method.into(),
            params: json!({}),
            session_id: None,
        }
    }

    #[tokio::test]
    async fn audits_enable_returns_empty_success() {
        let mut ctx = CdpContext::new();
        let resp = dispatch(&req("Audits.enable"), &mut ctx).await;
        assert!(
            resp.error.is_none(),
            "Audits.enable should not error: {:?}",
            resp.error
        );
        assert_eq!(resp.result, Some(json!({})));
    }

    #[tokio::test]
    async fn unknown_domain_still_errors() {
        let mut ctx = CdpContext::new();
        let resp = dispatch(&req("DefinitelyNotADomain.enable"), &mut ctx).await;
        let err = resp.error.expect("unknown domain must surface as error");
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("Unknown domain"));
    }

    #[tokio::test]
    async fn send_message_to_target_unwraps_inner_call() {
        let mut ctx = CdpContext::new();
        let inner = json!({
            "id": 42,
            "method": "Browser.getVersion",
            "params": {},
        });
        let outer = CdpRequest {
            id: 99,
            method: "Target.sendMessageToTarget".into(),
            params: json!({
                "sessionId": "sess-1",
                "message": inner.to_string(),
            }),
            session_id: None,
        };

        let resp = dispatch(&outer, &mut ctx).await;
        assert!(
            resp.error.is_none(),
            "wrapper must succeed: {:?}",
            resp.error
        );
        assert_eq!(resp.id, 99);
        assert_eq!(resp.result, Some(json!({})));

        // headless_chrome reads the inner response from the
        // receivedMessageFromTarget event, not from the wrapper response.
        let evt = ctx
            .pending_events
            .iter()
            .find(|e| e.method == "Target.receivedMessageFromTarget")
            .expect("receivedMessageFromTarget event must be emitted");
        assert_eq!(evt.params["sessionId"], "sess-1");
        let inner_msg = evt.params["message"].as_str().expect("message is a string");
        let parsed: serde_json::Value = serde_json::from_str(inner_msg).unwrap();
        assert_eq!(parsed["id"], 42);
        // Browser.getVersion returns a populated result object, not an error.
        assert!(
            parsed.get("result").is_some(),
            "inner response carries result"
        );
        assert!(
            parsed.get("error").is_none(),
            "inner response is not an error"
        );
    }

    #[tokio::test]
    async fn send_message_to_target_rejects_invalid_message() {
        let mut ctx = CdpContext::new();
        let outer = CdpRequest {
            id: 5,
            method: "Target.sendMessageToTarget".into(),
            params: json!({
                "sessionId": "sess-1",
                "message": "{not valid json",
            }),
            session_id: None,
        };
        let resp = dispatch(&outer, &mut ctx).await;
        let err = resp.error.expect("malformed inner messages must error");
        assert_eq!(err.code, -32700);
    }
}
