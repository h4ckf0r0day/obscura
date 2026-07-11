use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use obscura_js::ops::InterceptedRequest;
use serde_json::json;

use crate::domains;
use crate::domains::fetch::FetchInterceptState;
use crate::types::{CdpEvent, CdpRequest, CdpResponse};

pub struct CdpContext {
    pub pages: Vec<Page>,
    pub sessions: HashMap<String, String>, // session_id -> page_id
    pub pending_events: Vec<CdpEvent>,
    pub default_context: Arc<BrowserContext>,
    page_counter: u32,
    pub preload_scripts: Vec<(String, String)>, // (identifier, source)
    pub preload_counter: u32,
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

    /// Kitchen-sink constructor that also threads `allow_private_network`
    /// (issue #33). Older `new_with_*` builders stay as-is.
    pub fn new_full(
        proxy: Option<String>,
        stealth: bool,
        user_agent: Option<String>,
        storage_dir: Option<std::path::PathBuf>,
        allow_file_access: bool,
        allow_private_network: bool,
    ) -> Self {
        Self::_new_inner(
            proxy, stealth, user_agent, storage_dir, allow_file_access, allow_private_network,
        )
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
        let default_context = Arc::new(ctx);
        // Pre-seed with the default-frame execution context ids that
        // `Runtime.enable` (1) and post-navigation re-emission (2) advertise
        // via Runtime.executionContextCreated. Anything else has to be
        // registered explicitly (Page.createIsolatedWorld), otherwise
        // Runtime.{evaluate,callFunctionOn} should reject it per CDP spec.
        let mut valid_context_ids = HashSet::new();
        valid_context_ids.insert(1);
        valid_context_ids.insert(2);

        CdpContext {
            pages: Vec::new(),
            sessions: HashMap::new(),
            pending_events: Vec::new(),
            default_context,
            page_counter: 0,
            preload_scripts: Vec::new(),
            preload_counter: 0,
            fetch_intercept: FetchInterceptState::new(),
            intercept_tx: None,
            isolated_worlds: Vec::new(),
            valid_context_ids,
            next_isolated_context_id: 100,
            io_streams: crate::domains::io::IoStreamStore::default(),
        }
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
        self.page_counter += 1;
        let page_id = format!("page-{}", self.page_counter);
        let mut page = Page::new(page_id.clone(), self.default_context.clone());
        page.navigate_blank();
        self.pages.push(page);
        page_id
    }

    pub fn get_page(&self, id: &str) -> Option<&Page> {
        self.pages.iter().find(|p| p.id == id)
    }

    pub fn get_page_mut(&mut self, id: &str) -> Option<&mut Page> {
        self.pages.iter_mut().find(|p| p.id == id)
    }

    pub fn remove_page(&mut self, id: &str) {
        self.pages.retain(|p| p.id != id);
        self.sessions.retain(|_, v| v != id);
    }

    pub fn get_session_page(&self, session_id: &Option<String>) -> Option<&Page> {
        let page_id = session_id
            .as_ref()
            .and_then(|sid| self.sessions.get(sid))?;
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

/// Whether a CDP method can be served WITHOUT acquiring the global V8 lock.
///
/// Methods listed here were audited to confirm they do not transitively
/// call into a `JsRuntime`. They either don't touch any `Page` at all, or
/// use only the immutable `get_session_page` accessor and Rust-side field
/// reads. `get_session_page_mut` triggers `suspend_js`/`resume_js` and
/// must stay behind the lock.
fn is_v8_free_method(method: &str) -> bool {
    matches!(method,
        "Target.getTargets" | "Target.setDiscoverTargets"
        | "Target.attachToTarget" | "Target.attachToBrowserTarget"
        | "Target.setAutoAttach"
        | "Target.getBrowserContexts" | "Target.createBrowserContext"
        | "Target.disposeBrowserContext" | "Target.getTargetInfo"
        | "Target.detachFromTarget" | "Target.activateTarget"
        | "Browser.getVersion" | "Browser.close" | "Browser.getWindowForTarget"
        | "Browser.setDownloadBehavior" | "Browser.getWindowBounds" | "Browser.setWindowBounds"
        | "Page.enable" | "Page.disable" | "Page.getFrameTree"
        | "Page.setDownloadBehavior"
        | "Page.setLifecycleEventsEnabled"
        | "Page.addScriptToEvaluateOnNewDocument" | "Page.removeScriptToEvaluateOnNewDocument"
        | "Page.setInterceptFileChooserDialog" | "Page.getNavigationHistory"
        | "Page.resetNavigationHistory" | "Page.printToPDF"
        | "Page.captureScreenshot" | "Page.captureSnapshot"
        | "Page.createIsolatedWorld"
        | "Runtime.enable" | "Runtime.disable"
        | "Runtime.runIfWaitingForDebugger" | "Runtime.getExceptionDetails"
        | "Runtime.discardConsoleEntries"
        | "Network.enable" | "Network.disable" | "Network.setCacheDisabled"
        | "Network.setRequestInterception" | "Network.setBlockedURLs"
        | "Network.setExtraHTTPHeaders"
        | "Network.setUserAgentOverride"
        | "Network.getCookies" | "Network.getAllCookies"
        | "Network.setCookie" | "Network.setCookies"
        | "Network.deleteCookies" | "Network.clearBrowserCookies"
        | "Network.getResponseBody"
        | "Fetch.continueRequest" | "Fetch.fulfillRequest"
        | "Fetch.failRequest" | "Fetch.getResponseBody"
        | "Storage.getCookies" | "Storage.setCookies" | "Storage.deleteCookies"
    )
}

pub async fn dispatch(req: &CdpRequest, ctx: &mut CdpContext) -> CdpResponse {
    // headless_chrome (and older Puppeteer) wrap every CDP call inside
    // Target.sendMessageToTarget. Unwrap and recurse BEFORE acquiring the
    // V8 lock — the recursive dispatch will acquire it for the inner call,
    // and tokio Mutex is not reentrant.
    if req.method == "Target.sendMessageToTarget" {
        return dispatch_send_message_to_target(req, ctx).await;
    }

    // Issue #19: V8 fatal abort under concurrent CDP work.
    //
    // Every CDP handler below may end up calling into a per-Page `JsRuntime`
    // (each owning its own V8 Isolate). All of them run on a single OS
    // thread (current_thread tokio + LocalSet). When two pages' V8-touching
    // futures interleave across an `.await` (which `process_with_interception`
    // can trigger by handling new CDP messages while a navigation task is in
    // flight on `spawn_local`), V8 trips the
    // `heap->isolate() == Isolate::TryGetCurrent()` invariant and aborts the
    // process via `V8_Fatal` — no Rust panic, just `abort(3)`.
    //
    // Holding the process-wide V8 lock around the entire dispatch keeps each
    // handler contiguous on the thread: V8 fully exits one Isolate before
    // the next page is allowed in. This converts the abort into latency.
    // It overshoots — non-V8 handlers (Browser.*, Storage.*, Emulation.*)
    // also serialize — but those are cheap and the safety win dominates.
    //
    // The properly concurrent fix is to pin each `JsRuntime` to its own OS
    // thread and message-pass; that's tracked as the larger #19 follow-up.
    //
    // Optimization: methods that demonstrably never touch V8 bypass the
    // lock. During Puppeteer's newPage() setup ~8 such calls (Page.enable,
    // Runtime.enable, Page.getFrameTree, Page.addScriptToEvaluateOnNewDocument,
    // Page.createIsolatedWorld, etc.) used to serialize behind any
    // in-flight V8 work on a sibling page. Bypassing keeps the setup chain
    // running while a separate nav executes. Each listed method was audited
    // to confirm it never reaches `JsRuntime::execute_script` or DOM
    // mutation that re-enters V8; `get_session_page_mut` (which can
    // trigger `suspend_js`/`resume_js`) is NOT in the list.
    let _v8_guard = if is_v8_free_method(&req.method) {
        None
    } else {
        Some(obscura_js::v8_lock::global().lock().await)
    };

    // Per-command V8 watchdog. The lock above keeps each handler contiguous on
    // the thread, but it does not bound how long a handler runs: a hung page (a
    // runaway Runtime.evaluate, a synchronous DOM op) would hold the
    // process-wide V8 lock and wedge every other session forever. The one-shot
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
            .map(|h| obscura_js::cdp_watchdog::arm(h, std::time::Duration::from_millis(cmd_budget_ms)))
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
        "Target" => domains::target::handle(method, &req.params, ctx).await,
        "Browser" => domains::browser::handle(method, &req.params).await,
        "Page" => domains::page::handle(method, &req.params, ctx, &req.session_id).await,
        "DOM" => domains::dom::handle(method, &req.params, ctx, &req.session_id).await,
        "DOMSnapshot" => domains::domsnapshot::handle(method, &req.params, ctx, &req.session_id).await,
        "Runtime" => domains::runtime::handle(method, &req.params, ctx, &req.session_id).await,
        "Network" => domains::network::handle(method, &req.params, ctx, &req.session_id).await,
        "Fetch" => domains::fetch::handle(method, &req.params, ctx, &req.session_id).await,
        "IO" => domains::io::handle(method, &req.params, ctx).await,
        "Input" => domains::input::handle(method, &req.params, ctx, &req.session_id).await,
        "Storage" => domains::storage::handle(method, &req.params, ctx, &req.session_id).await,
        "LP" => domains::lp::handle(method, &req.params, ctx, &req.session_id).await,
        "Accessibility" => domains::accessibility::handle(method, &req.params, ctx, &req.session_id).await,
        // Accepted but no-op. Puppeteer's FrameManager.initialize calls
        // Audits.enable on connect — refusing it breaks puppeteer.connect()
        // before any user code runs.
        "Emulation" | "Log" | "Performance" | "Security" | "CSS"
        | "ServiceWorker" | "Inspector"
        | "Debugger" | "Profiler" | "HeapProfiler" | "Overlay"
        | "Audits" => {
            Ok(json!({}))
        }
        _ => Err(format!("Unknown domain: {}", domain)),
    };

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

    drain_binding_calls(ctx);

    match result {
        Ok(value) => CdpResponse::success(req.id, value, req.session_id.clone()),
        Err(msg) => {
            tracing::warn!("CDP error for {}: {}", req.method, msg);
            CdpResponse::error(req.id, -32601, msg, req.session_id.clone())
        }
    }
}

// Drain every page's binding-call queue (filled by op_binding_called when
// page JS invokes a `Runtime.addBinding` shim) and turn each entry into a
// Runtime.bindingCalled CDP event that the writer task forwards to the
// connected client. Called after every dispatch — binding calls only land
// in the queue while V8 is running inside a CDP handler, so there is no
// window in which they could pile up without a draining opportunity.
fn drain_binding_calls(ctx: &mut CdpContext) {
    // page_id -> session_id (any one session that holds this page).
    let page_to_session: HashMap<String, String> = ctx
        .sessions
        .iter()
        .map(|(sid, pid)| (pid.clone(), sid.clone()))
        .collect();

    let mut events: Vec<CdpEvent> = Vec::new();
    for page in &mut ctx.pages {
        let calls = page.take_pending_binding_calls();
        if calls.is_empty() {
            continue;
        }
        let Some(session_id) = page_to_session.get(&page.id).cloned() else {
            // No session attached — drop the calls; there is no client to
            // deliver them to.
            continue;
        };
        for (name, payload) in calls {
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
                session_id: Some(session_id.clone()),
            });
        }
    }
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
        assert!(resp.error.is_none(), "Audits.enable should not error: {:?}", resp.error);
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
        assert!(resp.error.is_none(), "wrapper must succeed: {:?}", resp.error);
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
        assert!(parsed.get("result").is_some(), "inner response carries result");
        assert!(parsed.get("error").is_none(), "inner response is not an error");
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
