use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::{JsRuntime, RuntimeOptions};
use obscura_dom::{DomTree, NodeId};

/// Re-exported so other crates (obscura-browser, obscura-cdp) can name the V8
/// isolate handle without taking a direct dependency on deno_core.
pub use deno_core::v8::IsolateHandle;

use crate::import_map::ImportMap;
use crate::module_loader::{ModuleLoadActivity, ObscuraModuleLoader};
#[cfg(all(test, feature = "render"))]
use crate::ops::ensure_prepared_render;
use crate::ops::{build_extension, node_is_script, ObscuraState, StoredNetworkResponseBody};
#[cfg(feature = "render")]
use crate::ops::{
    begin_animation_task, clamp_scroll_offset, document_base_url, ensure_resolved_scroll,
};

#[cfg(feature = "render")]
struct RuntimeCanvasSurfaceSource<'a>(
    &'a HashMap<NodeId, crate::ops::CanvasBackingSurface>,
);

#[cfg(feature = "render")]
impl obscura_render::CanvasSurfaceSource for RuntimeCanvasSurfaceSource<'_> {
    fn surface(&self, node: NodeId) -> Option<obscura_render::CanvasSurface<'_>> {
        let surface = self.0.get(&node)?;
        obscura_render::CanvasSurface::from_rgba8(
            surface.width,
            surface.height,
            surface.pixels.as_ref(),
        )
    }
}

static SNAPSHOT: &[u8] = include_bytes!(env!("OBSCURA_SNAPSHOT_PATH"));

/// Serializes V8 isolate construction across OS threads. The thread-per-
/// connection server (issue #430) builds isolates on many threads. The main
/// thread already warms up V8 once before any connection thread starts (see the
/// `ObscuraJsRuntime::new` warmup in `obscura-cdp` server startup), which is
/// what actually prevents the `InitializeBuiltinJSDispatchTable` segfault of a
/// first isolate built off the main thread. This lock is defense-in-depth: it
/// keeps two connections from running V8's isolate setup concurrently in case
/// any residual first-time process init races. Construction is rare and fast, so
/// serializing it costs nothing measurable; isolate *execution* stays fully
/// parallel, each isolate on its own thread with no shared lock.
static ISOLATE_CREATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const DEFAULT_CDP_AWAIT_TIMEOUT_MS: u64 = 30_000;
const HEAP_LIMIT_RECOVERY_HEADROOM_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct HeapLimitState {
    tripped: std::sync::atomic::AtomicBool,
    restore_limit: std::sync::atomic::AtomicUsize,
}

fn install_heap_limit_guard(
    runtime: &mut JsRuntime,
    isolate_handle: IsolateHandle,
    state: std::sync::Arc<HeapLimitState>,
) {
    runtime.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
        let _ = state.restore_limit.compare_exchange(
            0,
            current_limit,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        );
        state
            .tripped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        isolate_handle.terminate_execution();
        current_limit.saturating_add(HEAP_LIMIT_RECOVERY_HEADROOM_BYTES)
    });
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("unknown panic")
}

#[cfg(feature = "render")]
fn with_sync_render_loading_disabled<R>(
    state: &mut ObscuraState,
    capture: impl FnOnce(&mut ObscuraState) -> R,
) -> R {
    let previous = state
        .render_resources
        .set_sync_loading_enabled(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| capture(state)));
    state
        .render_resources
        .set_sync_loading_enabled(previous);
    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[derive(Debug, Clone)]
pub struct RemoteObjectInfo {
    pub js_type: String,
    pub subtype: Option<String>,
    pub class_name: String,
    pub description: String,
    pub object_id: Option<String>,
    pub value: Option<serde_json::Value>,
}

pub struct ObscuraJsRuntime {
    runtime: JsRuntime,
    state: Rc<RefCell<ObscuraState>>,
    object_store: HashMap<String, String>,
    object_counter: u64,
    import_map: Rc<RefCell<ImportMap>>,
    /// Loader-owned signal for pending dynamic-import graph fetches. This is
    /// intentionally separate from page fetch/XHR activity so analytics does
    /// not hold screenshot readiness open.
    module_load_activity: std::sync::Arc<ModuleLoadActivity>,
    /// Thread-safe handle to this runtime's V8 isolate, captured at
    /// construction. Lets a watchdog be armed from `&self` (the CDP dispatcher
    /// only holds `&Page` on the hot path) and is stable for the isolate's life.
    isolate_handle: IsolateHandle,
    /// Signals that V8 approached its configured heap limit. The callback
    /// terminates the current script and temporarily raises the limit just
    /// enough for V8 to unwind instead of aborting the worker process.
    heap_limit_state: std::sync::Arc<HeapLimitState>,
    /// Browser module-map evaluation is idempotent. deno_core 0.350 asserts if
    /// the same ModuleId is evaluated twice, so retain the first outcome for
    /// duplicate script tags and roots already seen by Obscura.
    module_evaluations: HashMap<deno_core::ModuleId, Result<(), String>>,
    /// Append-only record owned by the module loader. A cursor around each
    /// graph load identifies the dependency specifiers that become evaluated
    /// with its root.
    loaded_module_specifiers: Rc<RefCell<Vec<String>>>,
    /// Successful graph evaluation also evaluates every dependency. Remember
    /// those URLs so a dependency later encountered as a top-level script is a
    /// browser-style no-op instead of a second deno_core `mod_evaluate` call.
    evaluated_module_specifiers: HashMap<String, Result<(), String>>,
    /// The bound op table, taken from bootstrap at construction and removed from
    /// the global in the same step. Child frame realms are handed this object so
    /// their shims can call ops; nothing else can reach it, including page
    /// script.
    ops_handoff: Option<deno_core::v8::Global<deno_core::v8::Value>>,
}

/// Renders a caught V8 exception as a message for realm evaluation errors.
fn exception_text(
    scope: &mut deno_core::v8::TryCatch<'_, deno_core::v8::HandleScope<'_>>,
) -> String {
    match scope.exception() {
        Some(exception) => exception.to_rust_string_lossy(scope),
        None => "unknown error".to_string(),
    }
}

/// A fetched and instantiated module graph whose evaluation is intentionally
/// delayed until the HTML script scheduler reaches its post-parse turn.
pub struct PreparedModule {
    module_id: deno_core::ModuleId,
    description: String,
    entry_specifier: Option<String>,
    graph_specifiers: Vec<String>,
}

fn remaining_deadline_ms(deadline: tokio::time::Instant) -> Option<u64> {
    let remaining = deadline.checked_duration_since(tokio::time::Instant::now())?;
    if remaining.is_zero() {
        return None;
    }
    // Round up so a positive sub-millisecond remainder still gets one bounded
    // event-loop turn. The watchdog supplies the hard wall-clock boundary.
    let millis = remaining
        .as_millis()
        .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0));
    Some(millis.min(u128::from(u64::MAX)) as u64)
}

/// Handle to an armed V8 execution watchdog (see [`ObscuraJsRuntime::arm_watchdog`]).
/// Holds the cancel channel and the watchdog thread; pass it back to
/// `disarm_watchdog` to stop the watchdog and learn whether it fired.
pub struct WatchdogToken {
    pair: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    join: Option<std::thread::JoinHandle<()>>,
    fired: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Arm a V8 termination watchdog directly from an isolate handle, with no
/// runtime borrow. The CDP dispatcher uses this to bound every command so a
/// hung page cannot hold this connection's V8 lock forever. Pair with
/// [`WatchdogToken::stop`]; if `stop` returns true, clear the termination flag
/// via [`ObscuraJsRuntime::cancel_termination`] before reusing the isolate.
pub fn spawn_watchdog(handle: IsolateHandle, budget: std::time::Duration) -> WatchdogToken {
    let pair = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pair_c = pair.clone();
    let fired_c = fired.clone();
    let join = std::thread::spawn(move || {
        let (lock, cvar) = &*pair_c;
        let mut cancelled = lock.lock().unwrap();
        let deadline = std::time::Instant::now() + budget;
        loop {
            // Check first: stop() may have set this (and notified into the void)
            // before this thread even started, which happens constantly for fast
            // CDP commands where stop() is called right after spawn. Without this
            // top check the lost notify means we wait the full budget before
            // noticing, and stop()'s join() blocks for that whole time.
            if *cancelled {
                return;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                fired_c.store(true, std::sync::atomic::Ordering::SeqCst);
                handle.terminate_execution();
                return;
            }
            let (guard, _) = cvar.wait_timeout(cancelled, remaining).unwrap();
            cancelled = guard;
            if *cancelled {
                return;
            }
        }
    });
    WatchdogToken {
        pair,
        join: Some(join),
        fired,
    }
}

impl WatchdogToken {
    /// Stop the watchdog. Returns true if it had already fired (terminated the
    /// isolate). The caller must then clear the termination flag via
    /// [`ObscuraJsRuntime::cancel_termination`] before the next eval.
    pub fn stop(mut self) -> bool {
        {
            let (lock, cvar) = &*self.pair;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.fired.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// Observation deadlines are checked between browser tasks. A task which has
// already started receives this bounded completion allowance, matching the
// fixed-wait path while retaining an absolute backstop for infinite script.
const SYNCHRONOUS_TASK_FLOOR_MS: u64 = 5_000;
const WATCHDOG_SCHEDULING_MARGIN_MS: u64 = 500;

impl ObscuraJsRuntime {
    /// Freeze the document timeline for one JavaScript task. Browser timelines
    /// update at task/rendering boundaries, not on each forced style or layout
    /// read. Keeping one sample across the task also lets repeated CSSOM reads
    /// share the retained layout on pages with running animations.
    fn begin_javascript_task(&mut self) {
        // Some internal callers intentionally ignore script errors. Recover a
        // heap-limit termination before any later task enters V8 even when the
        // caller that triggered it did not need the error value.
        self.recover_heap_limit();
        #[cfg(feature = "render")]
        begin_animation_task(&mut self.state.borrow_mut());
    }
    pub fn new() -> Self {
        Self::with_base_url("about:blank")
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self::with_base_url_and_proxy(base_url, None)
    }

    /// Construct a runtime whose ES-module loader routes dynamic imports
    /// through `proxy_url` (#139). `None` is equivalent to `with_base_url`
    /// (direct connection).
    pub fn with_base_url_and_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        let state = Rc::new(RefCell::new(ObscuraState::new()));
        let state_clone = state.clone();
        let import_map = state.borrow().import_map.clone();

        let module_loader = ObscuraModuleLoader::with_page_state(
            base_url,
            proxy_url,
            &state,
            import_map.clone(),
        );
        let module_load_activity = module_loader.activity();
        let loaded_module_specifiers = module_loader.loaded_specifiers();
        let module_loader = Rc::new(module_loader);

        // Build the isolate under the process-wide creation lock so two
        // connection threads never construct isolates concurrently (#430).
        let (runtime, isolate_handle, heap_limit_state) = {
            let _create_guard = ISOLATE_CREATE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            let mut runtime = JsRuntime::new(RuntimeOptions {
                extensions: vec![build_extension()],
                module_loader: Some(module_loader),
                startup_snapshot: Some(SNAPSHOT),
                ..Default::default()
            });

            {
                let op_state = runtime.op_state();
                let mut op_state = op_state.borrow_mut();
                op_state.put(state_clone);
                // Empty until a frame realm exists, which is what keeps the
                // lookup free for pages that have no frames.
                op_state.put(Rc::new(RefCell::new(crate::ops::RealmStates::default())));
            }

            let isolate_handle = runtime.v8_isolate().thread_safe_handle();
            let heap_limit_state = std::sync::Arc::new(HeapLimitState::default());
            install_heap_limit_guard(
                &mut runtime,
                isolate_handle.clone(),
                heap_limit_state.clone(),
            );

            runtime
                .execute_script(
                    "<obscura:init>",
                    "globalThis.__obscura_objects = {}; globalThis.__obscura_oid = 0;".to_string(),
                )
                .expect("init should not fail");

            (runtime, isolate_handle, heap_limit_state)
        };

        let mut instance = ObscuraJsRuntime {
            runtime,
            state,
            object_store: HashMap::new(),
            object_counter: 0,
            import_map,
            module_load_activity,
            isolate_handle,
            heap_limit_state,
            module_evaluations: HashMap::new(),
            loaded_module_specifiers,
            evaluated_module_specifiers: HashMap::new(),
            ops_handoff: None,
        };
        // Take the op table before any page script can run, and drop the global
        // that exposed it in the same step.
        instance.ops_handoff = instance.take_ops_handoff();
        instance
    }

    /// Creates an additional realm in this isolate: a second `v8::Context`.
    ///
    /// The startup snapshot already contains the whole bootstrap (see
    /// `build.rs`), so a context restored from it arrives with every DOM class
    /// and shim installed. Building a realm is therefore a context restore, not
    /// a re-parse of the whole bootstrap.
    ///
    /// The new context has no ops: deno_core binds those into the main context
    /// only. Use [`Self::share_ops_with_realm`] to give it the same `Deno.core`
    /// object, which is legal because native function objects are shareable
    /// between contexts of one isolate.
    pub(crate) fn create_realm_context(
        &mut self,
    ) -> Option<deno_core::v8::Global<deno_core::v8::Context>> {
        let context = {
            let isolate = self.runtime.v8_isolate();
            let scope = &mut deno_core::v8::HandleScope::new(isolate);
            let context = deno_core::v8::Context::from_snapshot(
                scope,
                1,
                deno_core::v8::ContextOptions::default(),
            )
            .or_else(|| {
                deno_core::v8::Context::from_snapshot(
                    scope,
                    0,
                    deno_core::v8::ContextOptions::default(),
                )
            })?;
            deno_core::v8::Global::new(scope, context)
        };
        Some(context)
    }

    /// Takes the ops object bootstrap handed out, and removes the handoff from
    /// the global so page script can never reach `Deno.core.ops`.
    ///
    /// deno_core hides `globalThis.Deno` after setup and bootstrap keeps its
    /// reference in a private const, so this handoff is the only way for the
    /// host to reach the bound op functions and pass them to a child realm.
    fn take_ops_handoff(&mut self) -> Option<deno_core::v8::Global<deno_core::v8::Value>> {
        use deno_core::v8;

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Local::new(scope, main);
        let scope = &mut v8::ContextScope::new(scope, context);

        let handoff_key = v8::String::new(scope, "__obscura_core_handoff")?;
        let ops_key = v8::String::new(scope, "ops")?;
        let global = context.global(scope);

        let core = global.get(scope, handoff_key.into())?;
        let core = core.to_object(scope)?;
        let ops = core.get(scope, ops_key.into())?;
        if !ops.is_object() {
            return None;
        }
        let ops = v8::Global::new(scope, ops);
        global.delete(scope, handoff_key.into());
        Some(ops)
    }

    /// Points a child realm's `Deno.core.ops` at the main realm's ops object.
    ///
    /// A realm restored from the snapshot has its own `Deno.core` with an empty
    /// ops table, and its bootstrap captured that exact object, so filling the
    /// `ops` table on it is enough to give every shim in that realm a working
    /// op surface. The functions are shared, not copied: same isolate.
    pub(crate) fn share_ops_with_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
    ) -> bool {
        use deno_core::v8;

        let Some(ops) = self.ops_handoff.clone() else {
            return false;
        };
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Local::new(scope, realm);
        let scope = &mut v8::ContextScope::new(scope, context);

        let Some(handoff_key) = v8::String::new(scope, "__obscura_core_handoff") else {
            return false;
        };
        let Some(ops_key) = v8::String::new(scope, "ops") else {
            return false;
        };
        let global = context.global(scope);
        let Some(core) = global.get(scope, handoff_key.into()) else {
            return false;
        };
        let Some(core) = core.to_object(scope) else {
            return false;
        };
        // `Deno.core.ops` is non-writable and non-configurable, so the table
        // cannot be swapped wholesale: V8 reports success and changes nothing.
        // Copy the bound op functions into the realm's existing table instead.
        let Some(target) = core
            .get(scope, ops_key.into())
            .and_then(|value| value.to_object(scope))
        else {
            return false;
        };
        let source = v8::Local::new(scope, ops);
        let Some(source) = source.to_object(scope) else {
            return false;
        };
        let Some(names) = source.get_own_property_names(scope, Default::default()) else {
            return false;
        };
        let mut copied = 0;
        for index in 0..names.length() {
            let Some(key) = names.get_index(scope, index) else {
                continue;
            };
            let Some(value) = source.get(scope, key) else {
                continue;
            };
            if target.set(scope, key, value).unwrap_or(false) {
                copied += 1;
            }
        }
        // The child realm must not expose the handoff to frame script either.
        global.delete(scope, handoff_key.into());
        copied > 0
    }

    /// Runs `source` inside `realm` and returns its value as a string. Errors
    /// come back as `Err(message)`.
    pub(crate) fn eval_in_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
        source: &str,
    ) -> Result<String, String> {
        use deno_core::v8;

        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let context = v8::Local::new(scope, realm);
        let scope = &mut v8::ContextScope::new(scope, context);
        let scope = &mut v8::TryCatch::new(scope);

        let code = v8::String::new(scope, source).ok_or("source too large")?;
        let script = match v8::Script::compile(scope, code, None) {
            Some(script) => script,
            None => return Err(exception_text(scope)),
        };
        match script.run(scope) {
            Some(value) => Ok(value.to_rust_string_lossy(scope)),
            None => Err(exception_text(scope)),
        }
    }

    /// Copies the browser-identity globals from the main realm into `realm`.
    ///
    /// A frame must present the same identity as its parent: anti-bot code
    /// fingerprints inside the frame and compares it with the top document.
    /// Copying the values the parent already has makes that true by
    /// construction, instead of relying on a caller to reapply the same
    /// settings to both.
    pub(crate) fn copy_identity_to_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
    ) {
        use deno_core::v8;

        const IDENTITY_GLOBALS: [&str; 7] = [
            "__obscura_ua",
            "__obscura_platform",
            "__obscura_ua_platform",
            "__obscura_ua_platform_version",
            "__obscura_stealth",
            "__obscura_geo_lat",
            "__obscura_geo_lon",
        ];

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);

        let main_context = v8::Local::new(scope, main);
        let mut carried = Vec::new();
        {
            let scope = &mut v8::ContextScope::new(scope, main_context);
            let global = main_context.global(scope);
            for name in IDENTITY_GLOBALS {
                let Some(key) = v8::String::new(scope, name) else {
                    continue;
                };
                match global.get(scope, key.into()) {
                    Some(value) if !value.is_undefined() => {
                        carried.push((name, v8::Global::new(scope, value)));
                    }
                    _ => {}
                }
            }
        }

        let realm_context = v8::Local::new(scope, realm);
        let scope = &mut v8::ContextScope::new(scope, realm_context);
        let global = realm_context.global(scope);
        for (name, value) in carried {
            let Some(key) = v8::String::new(scope, name) else {
                continue;
            };
            let value = v8::Local::new(scope, value);
            global.set(scope, key.into(), value);
        }
    }

    /// Gives a frame's state the resources the page owns: cookie jar, HTTP
    /// client, callbacks and the stealth transport. A frame shares these with
    /// its page, exactly as it shares them in a browser.
    pub(crate) fn share_resources_with(&self, frame: &mut ObscuraState) {
        let parent = self.state.borrow();
        frame.cookie_jar = parent.cookie_jar.clone();
        frame.http_client = parent.http_client.clone();
        frame.callbacks = parent.callbacks.clone();
        frame.encoding = parent.encoding.clone();
        frame.blocked_urls = parent.blocked_urls.clone();
        frame.intercept_enabled = parent.intercept_enabled;
        frame.page_in_flight = parent.page_in_flight.clone();
        #[cfg(feature = "stealth")]
        {
            frame.stealth_client = parent.stealth_client.clone();
        }
    }

    /// The origin of the document this runtime is running, or `"null"` for a
    /// scheme that has no tuple origin.
    pub(crate) fn page_origin(&self) -> String {
        let url = self.state.borrow().url.clone();
        match url::Url::parse(&url) {
            Ok(parsed) if parsed.origin().is_tuple() => parsed.origin().ascii_serialization(),
            _ => "null".to_string(),
        }
    }

    /// Gives a same-origin frame realm the page's security token.
    ///
    /// V8 access-checks property reads across contexts and answers `undefined`
    /// unless the two carry the same token, which is how a browser keeps one
    /// origin out of another's window. Two contexts of one origin must share a
    /// token, or the page reads its own frame's globals as undefined. Only
    /// ever called after an origin comparison; a cross-origin frame keeps its
    /// own token and stays opaque.
    pub(crate) fn share_security_token_with_realm(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
    ) {
        use deno_core::v8;

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);
        let main = v8::Local::new(scope, main);
        let realm = v8::Local::new(scope, realm);
        let token = main.get_security_token(scope);
        realm.set_security_token(token);
    }

    /// Publishes a frame realm's own `window` and `document` objects into the
    /// page realm, under `__obscura_frameObjects[frameId]`.
    ///
    /// This is what the single isolate buys. Objects cannot cross isolates, so
    /// a parent could only ever be handed a copy or a shim; within one isolate
    /// it can hold the frame's real globals, which is what a browser gives it
    /// for a same-origin frame. `contentWindow.someGlobal` is then a plain
    /// property read of the frame's own object, and `contentDocument` is the
    /// document the frame's scripts actually mutated.
    pub(crate) fn publish_realm_objects(
        &mut self,
        realm: &deno_core::v8::Global<deno_core::v8::Context>,
        frame_id: u32,
    ) -> bool {
        use deno_core::v8;

        let main = self.runtime.main_context();
        let isolate = self.runtime.v8_isolate();
        let scope = &mut v8::HandleScope::new(isolate);

        // Read the frame's globals first, then install them in the page realm.
        // Both contexts belong to this isolate, so the handles stay valid
        // across the switch.
        let realm_context = v8::Local::new(scope, realm);
        let (frame_window, frame_document) = {
            let scope = &mut v8::ContextScope::new(scope, realm_context);
            let global = realm_context.global(scope);
            let Some(key) = v8::String::new(scope, "document") else {
                return false;
            };
            let document = global.get(scope, key.into());
            (
                v8::Global::new(scope, global),
                document.map(|value| v8::Global::new(scope, value)),
            )
        };

        let main_context = v8::Local::new(scope, main);
        let scope = &mut v8::ContextScope::new(scope, main_context);
        let global = main_context.global(scope);
        let Some(registry_key) = v8::String::new(scope, "__obscura_frameObjects") else {
            return false;
        };
        let registry = match global
            .get(scope, registry_key.into())
            .and_then(|value| value.to_object(scope))
        {
            Some(registry) if !registry.is_null_or_undefined() => registry,
            _ => {
                let fresh = v8::Object::new(scope);
                global.set(scope, registry_key.into(), fresh.into());
                fresh
            }
        };

        let entry = v8::Object::new(scope);
        let window = v8::Local::new(scope, frame_window);
        if let Some(key) = v8::String::new(scope, "window") {
            entry.set(scope, key.into(), window.into());
        }
        if let (Some(key), Some(document)) = (
            v8::String::new(scope, "document"),
            frame_document.map(|document| v8::Local::new(scope, document)),
        ) {
            entry.set(scope, key.into(), document);
        }
        let index = v8::Integer::new_from_unsigned(scope, frame_id);
        registry.set(scope, index.into(), entry.into()).unwrap_or(false)
    }

    /// The table ops consult to find the calling realm's document.
    pub(crate) fn realm_states(&self) -> Rc<RefCell<crate::ops::RealmStates>> {
        self.runtime
            .op_state()
            .borrow()
            .borrow::<Rc<RefCell<crate::ops::RealmStates>>>()
            .clone()
    }

    /// Frame documents fetched by any realm that still need one of their own.
    /// The op queues onto the page's state whichever frame asked, so a frame
    /// nested inside a frame is drained here too.
    pub fn take_pending_frames(&self) -> Vec<crate::ops::PendingFrame> {
        let mut state = self.state.borrow_mut();
        state.pending_frame_bytes = 0;
        std::mem::take(&mut state.pending_frames)
    }

    /// postMessage traffic waiting to be delivered to another realm.
    pub fn take_pending_frame_messages(&self) -> Vec<crate::ops::PendingFrameMessage> {
        let mut state = self.state.borrow_mut();
        state.pending_frame_message_bytes = 0;
        std::mem::take(&mut state.pending_frame_messages)
    }

    /// Restore the configured V8 heap limit after the emergency headroom has
    /// allowed a terminated allocation to unwind. The callback is then armed
    /// again so a second hostile script cannot grow the isolate without bound.
    fn recover_heap_limit(&mut self) -> bool {
        if !self
            .heap_limit_state
            .tripped
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return false;
        }

        self.runtime.v8_isolate().cancel_terminate_execution();
        let restore_limit = self
            .heap_limit_state
            .restore_limit
            .swap(0, std::sync::atomic::Ordering::SeqCst);
        self.runtime
            .remove_near_heap_limit_callback(restore_limit);
        install_heap_limit_guard(
            &mut self.runtime,
            self.isolate_handle.clone(),
            self.heap_limit_state.clone(),
        );
        tracing::warn!("V8 heap limit reached: terminated the current JavaScript task");
        true
    }

    fn finish_heap_checked<T>(&mut self, result: Result<T, String>) -> Result<T, String> {
        if self.recover_heap_limit() {
            Err("JavaScript heap limit exceeded; execution terminated".to_string())
        } else {
            result
        }
    }

    fn execute_runtime_script(
        &mut self,
        name: &'static str,
        source: String,
    ) -> Result<deno_core::v8::Global<deno_core::v8::Value>, String> {
        let result = self
            .runtime
            .execute_script(name, source)
            .map_err(|error| error.to_string());
        self.finish_heap_checked(result)
    }

    /// Parse and merge an inline document import map. Rules which would alter
    /// already-observed module resolutions are discarded while unrelated new
    /// rules remain available, matching Chromium's multiple-map model.
    pub fn add_import_map(&self, source: &str, base_url: &str) -> Result<(), String> {
        let map = ImportMap::parse(source, base_url)?;
        self.import_map
            .try_borrow_mut()
            .map_err(|_| "Import map is already borrowed".to_string())?
            .merge(map);
        Ok(())
    }

    pub fn set_cookie_jar(&self, jar: std::sync::Arc<obscura_net::CookieJar>) {
        self.state.borrow_mut().cookie_jar = Some(jar);
    }

    pub fn set_http_client(&self, client: std::sync::Arc<obscura_net::ObscuraHttpClient>) {
        self.state.borrow_mut().http_client = Some(client);
    }

    /// Install the owning page's passive on_request/on_response callback
    /// registry so scripted fetch()/XHR observation is page-scoped (issue #408).
    pub fn set_callbacks(&self, callbacks: std::sync::Arc<obscura_net::CallbackRegistry>) {
        self.state.borrow_mut().callbacks = Some(callbacks);
    }

    /// Install the stealth (wreq) HTTP client so scripted fetch()/XHR is routed
    /// through it in stealth mode (see op_fetch_url / stealth_fetch_all).
    #[cfg(feature = "stealth")]
    pub fn set_stealth_client(&self, client: std::sync::Arc<obscura_net::StealthHttpClient>) {
        self.state.borrow_mut().stealth_client = Some(client);
    }

    pub fn set_dom(&self, dom: DomTree) {
        let mut gs = self.state.borrow_mut();
        gs.dom = Some(dom);
        gs.document_generation = gs.document_generation.wrapping_add(1);
        gs.activity_generation = 0;
        gs.page_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        gs.already_started_scripts.borrow_mut().clear();
        // A new document owns a fresh retained scene and resource cache.
        #[cfg(feature = "render")]
        {
            gs.prepared_render = None;
            gs.animation_sample = obscura_render::AnimationSample::default();
            gs.animation_timeline = obscura_render::AnimationTimelineState::default();
            gs.animation_timeline_origin = std::time::Instant::now();
            gs.animation_task_generation = 0;
            gs.animation_sampled_task_generation = 0;
            gs.pending_style_mutations.clear();
            gs.render_resources = obscura_render::RenderResourceCache::default();
            gs.render_image_in_flight.clear();
            gs.stylesheet_cache = obscura_render::StylesheetCache::default();
            gs.dynamic_fonts.clear();
            gs.canvas_surfaces.clear();
            gs.scroll_offset = (0.0, 0.0);
            gs.element_scroll_offsets.clear();
            gs.scroll_generation = 0;
            gs.resolved_scroll = None;
        }
    }

    pub fn set_url(&self, url: &str) {
        let mut state = self.state.borrow_mut();
        if state.url != url {
            state.url = url.to_string();
            #[cfg(feature = "render")]
            {
                // Relative resources use the document URL when no <base> is
                // present. Keep already-fetched absolute bytes, but rebuild
                // candidate selection/layout against the new base.
                state.prepared_render = None;
                state.pending_style_mutations.clear();
                state.resolved_scroll = None;
            }
        }
    }

    /// Set the document's character encoding (WHATWG canonical name). Backs
    /// `document.characterSet` and the `<a>`/`<area>` URL query encoding
    /// override for legacy-charset documents.
    pub fn set_encoding(&self, encoding: &str) {
        self.state.borrow_mut().encoding = encoding.to_string();
    }

    pub fn set_title(&self, title: &str) {
        self.state.borrow_mut().title = title.to_string();
    }

    /// Set the source document URL exposed as `document.referrer`. Navigation
    /// owns this value; it is not derived from the current URL because direct
    /// navigations and document-initiated navigations have different
    /// referrer semantics.
    pub fn set_referrer(&self, referrer: &str) {
        self.state.borrow_mut().referrer = referrer.to_string();
    }

    pub fn set_blocked_urls(&self, patterns: Vec<String>) {
        self.state.borrow_mut().blocked_urls = patterns;
    }

    pub fn take_pending_navigation(&self) -> Option<(String, String, String)> {
        self.state.borrow_mut().pending_navigation.take()
    }

    pub fn take_pending_binding_calls(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.state.borrow_mut().pending_binding_calls)
    }

    pub fn get_network_response_body(&self, request_id: &str) -> Option<StoredNetworkResponseBody> {
        self.state
            .borrow()
            .network_response_bodies
            .get(request_id)
            .cloned()
    }

    pub fn clear_network_response_bodies(&self) {
        let mut state = self.state.borrow_mut();
        state.network_response_bodies.clear();
        state.network_response_body_order.clear();
    }

    /// Wire up the interception channel without enabling interception.
    /// Use set_intercept_enabled separately. The two were entangled before
    /// and every navigation auto-enabled interception, which made
    /// `fetch()` from page JS hang forever waiting for a CDP client to
    /// answer Fetch.requestPaused events that the client never asked for.
    pub fn set_intercept_tx(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::ops::InterceptedRequest>,
    ) {
        let mut state = self.state.borrow_mut();
        state.intercept_tx = Some(tx);
    }

    pub fn set_intercept_enabled(&self, enabled: bool) {
        let mut state = self.state.borrow_mut();
        state.intercept_enabled = enabled;
    }

    pub fn set_user_agent(&mut self, ua: &str) {
        let escaped = ua.replace('\\', "\\\\").replace('\'', "\\'");
        let _ = self.execute_runtime_script(
            "<set-ua>",
            format!("globalThis.__obscura_ua = '{}';", escaped),
        );
    }

    pub fn set_platform(&mut self, platform: &str, ua_platform: &str, ua_platform_version: &str) {
        let p = platform.replace('\'', "\\'");
        let uap = ua_platform.replace('\'', "\\'");
        let uapv = ua_platform_version.replace('\'', "\\'");
        let _ = self.execute_runtime_script(
            "<set-platform>",
            format!(
                "globalThis.__obscura_platform='{}';globalThis.__obscura_ua_platform='{}';globalThis.__obscura_ua_platform_version='{}';",
                p, uap, uapv
            ),
        );
    }

    pub fn set_stealth(&mut self, enabled: bool) {
        let _ = self.execute_runtime_script(
            "<set-stealth>",
            format!("globalThis.__obscura_stealth = {};", enabled),
        );
    }

    /// Set the CSS viewport exposed to page JavaScript. This must run before
    /// `run_page_init` for navigation-time responsive code; it may also be
    /// called later by CDP emulation to update the live window surfaces.
    pub fn set_viewport(&mut self, width: f64, height: f64) {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return;
        }
        #[cfg(feature = "render")]
        {
            let mut state = self.state.borrow_mut();
            let viewport = (width as f32, height as f32);
            if state.viewport != viewport {
                state.viewport = viewport;
                state.prepared_render = None;
                state.pending_style_mutations.clear();
                state.resolved_scroll = None;
            }
        }
        let _ = self.execute_runtime_script(
            "<set-viewport>",
            format!(
                "globalThis.__obscura_viewport_w={width};\
                 globalThis.__obscura_viewport_h={height};\
                 globalThis.innerWidth={width};globalThis.innerHeight={height};\
                 if(globalThis.visualViewport){{\
                   globalThis.visualViewport.width={width};\
                   globalThis.visualViewport.height={height};\
                 }}\
                 if(typeof globalThis.__obscura_recompute_intersections==='function'){{\
                   globalThis.__obscura_recompute_intersections();\
                 }}\
                 if(typeof globalThis.__obscura_recompute_resizes==='function'){{\
                   globalThis.__obscura_recompute_resizes();\
                 }}",
            ),
        );
    }

    /// Override the physical screen metrics exposed to page JavaScript.
    /// Unlike the CSS viewport, CDP only changes these when both optional
    /// screen dimensions are supplied. Passing `None` restores the native
    /// screen surface while keeping the viewport override intact.
    pub fn set_screen_size_override(&mut self, size: Option<(f64, f64)>, emulated: bool) {
        let script = match size {
            Some((width, height))
                if width.is_finite()
                    && height.is_finite()
                    && width > 0.0
                    && height > 0.0 =>
            {
                format!(
                    "globalThis.__obscura_set_screen_override({width},{height},{emulated});"
                )
            }
            _ => format!(
                "globalThis.__obscura_set_screen_override(null,null,{emulated});"
            ),
        };
        let _ = self.execute_runtime_script("<set-screen-size>", script);
    }

    /// Current clamped root scroll offset shared by CSSOM geometry and paint.
    #[cfg(feature = "render")]
    pub fn scroll_offset(&self) -> (f32, f32) {
        let mut state = self.state.borrow_mut();
        let requested = state.scroll_offset;
        clamp_scroll_offset(&mut state, requested)
    }

    /// Select the document-timeline instant used by the next render flush.
    /// Returns false for invalid times and preserves the current sample.
    #[cfg(feature = "render")]
    pub fn set_animation_sample_time(
        &self,
        sample: obscura_render::AnimationSampleTime,
    ) -> bool {
        self.set_animation_sample(obscura_render::AnimationSample::document(
            sample.milliseconds,
        ))
    }

    #[cfg(feature = "render")]
    pub fn set_animation_sample(&self, sample: obscura_render::AnimationSample) -> bool {
        if !sample.time.milliseconds.is_finite() || sample.time.milliseconds < 0.0 {
            return false;
        }
        let mut state = self.state.borrow_mut();
        if state.animation_sample != sample {
            let forward_document_sample =
                sample.mode == obscura_render::AnimationSampleMode::DocumentTime
                && state.animation_sample.mode == obscura_render::AnimationSampleMode::DocumentTime
                && sample.time.milliseconds > state.animation_sample.time.milliseconds;
            if forward_document_sample
                && state.pending_style_mutations.is_empty()
                && state.prepared_render.as_mut().is_some_and(|prepared| {
                    prepared.advance_inactive_animation_sample_time(sample.time)
                })
            {
                state.animation_sample = sample;
                return true;
            }
            state.animation_sample = sample;
            if !forward_document_sample {
                state.prepared_render = None;
                state.pending_style_mutations.clear();
            }
            state.resolved_scroll = None;
        }
        true
    }

    /// Select the CSS media type for the next synchronous render flush.
    /// Changing media invalidates geometry and the compiled stylesheet key but
    /// leaves the live DOM, scroll offsets, and resource bytes untouched.
    #[cfg(feature = "render")]
    pub fn set_render_media(
        &self,
        media: obscura_render::CssMediaType,
    ) -> obscura_render::CssMediaType {
        let mut state = self.state.borrow_mut();
        let previous = state.render_media;
        if previous != media {
            state.render_media = media;
            state.prepared_render = None;
            state.resolved_scroll = None;
        }
        previous
    }

    #[cfg(feature = "render")]
    pub fn animation_sample_time(&self) -> obscura_render::AnimationSampleTime {
        self.state.borrow().animation_sample.time
    }

    #[cfg(feature = "render")]
    pub fn live_animation_sample(&self) -> obscura_render::AnimationSample {
        let state = self.state.borrow();
        obscura_render::AnimationSample::document(
            (state.animation_timeline_origin.elapsed().as_secs_f64() * 1_000.0)
                .min(f64::from(f32::MAX)) as f32,
        )
    }

    #[cfg(feature = "render")]
    pub fn reset_animation_timeline(&self) {
        let mut state = self.state.borrow_mut();
        state.animation_timeline_origin = std::time::Instant::now();
        state.animation_timeline = obscura_render::AnimationTimelineState::default();
        state.animation_sample = obscura_render::AnimationSample::default();
        state.prepared_render = None;
        state.pending_style_mutations.clear();
        state.resolved_scroll = None;
    }

    /// Read animation damage from the last prepared frame without causing a
    /// style/layout flush. Screencast scheduling uses this to avoid rasterizing
    /// static pages on every compositor tick.
    #[cfg(feature = "render")]
    pub fn prepared_has_active_css_animations(&self) -> bool {
        self.state
            .borrow()
            .prepared_render
            .as_ref()
            .is_some_and(|prepared| prepared.has_active_css_animations())
    }

    /// Capture the live render viewport from the same prepared layout used by
    /// CSSOM geometry. A mismatched ad-hoc viewport/base returns `None` so the
    /// browser layer can retain its compatibility one-shot path.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared(
        &self,
        viewport: (f32, f32),
        base_url: Option<&str>,
    ) -> Option<Vec<u8>> {
        self.screenshot_prepared_with_surface_color(
            viewport,
            base_url,
            [255, 255, 255, 255],
        )
    }

    #[cfg(feature = "render")]
    pub fn screenshot_prepared_with_surface_color(
        &self,
        viewport: (f32, f32),
        base_url: Option<&str>,
        surface_color: [u8; 4],
    ) -> Option<Vec<u8>> {
        let mut state = self.state.borrow_mut();
        let effective_base = document_base_url(&state);
        if viewport != state.viewport || base_url != effective_base.as_deref() {
            return None;
        }
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = state;
            let (_, scroll) = resolved_scroll.as_ref()?;
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
                dom.as_ref()?,
                prepared_render.as_mut()?,
                render_resources,
                scroll,
                surface_color,
                &canvas_surfaces,
            )
        })
    }

    /// Capture a document-space rectangle without changing the live viewport,
    /// root scroll, element scroll offsets, or retained layout. The current
    /// resolved scroll snapshot supplies fixed/sticky and nested-scroll state.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region(
        &self,
        region: obscura_render::CaptureRegion,
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        self.screenshot_prepared_region_with_surface_color(region, [255, 255, 255, 255])
    }

    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region_with_surface_color(
        &self,
        region: obscura_render::CaptureRegion,
        surface_color: [u8; 4],
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state).ok_or(obscura_render::CaptureError::PaintFailed)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = state;
            let (_, scroll) = resolved_scroll
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?;
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces(
                dom.as_ref()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                prepared_render
                    .as_mut()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                render_resources,
                scroll,
                region,
                surface_color,
                &canvas_surfaces,
            )
        })
    }

    /// Capture a document-space rectangle with the PDF print-background
    /// policy without mutating the page DOM or retained geometry.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region_with_backgrounds(
        &self,
        region: obscura_render::CaptureRegion,
        paint_backgrounds: bool,
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state).ok_or(obscura_render::CaptureError::PaintFailed)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = state;
            let (_, scroll) = resolved_scroll
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?;
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces(
                dom.as_ref()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                prepared_render
                    .as_mut()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                render_resources,
                scroll,
                region,
                paint_backgrounds,
                &canvas_surfaces,
            )
        })
    }

    /// Capture one immutable document slice as if its origin were the root
    /// scroll position of a virtual viewport. This leaves the live page scroll
    /// untouched while giving fixed and sticky descendants page-local paint
    /// geometry, which paginated raster PDF export requires.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region_at_scroll_with_backgrounds(
        &self,
        region: obscura_render::CaptureRegion,
        root_scroll: (f32, f32),
        paint_backgrounds: bool,
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state).ok_or(obscura_render::CaptureError::PaintFailed)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                element_scroll_offsets,
                canvas_surfaces,
                ..
            } = state;
            let dom = dom
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?;
            let scroll = prepared_render
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?
                .resolve_scroll_state_for_viewport(
                    dom,
                    root_scroll,
                    element_scroll_offsets,
                    (region.width, region.height),
                );
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces(
                dom,
                prepared_render
                    .as_mut()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                render_resources,
                &scroll,
                region,
                paint_backgrounds,
                &canvas_surfaces,
            )
        })
    }

    /// Return the retained layout's scrollable document size without changing
    /// the live viewport or scroll position. PDF/full-document consumers use
    /// this to paginate document-space captures from the same geometry.
    #[cfg(feature = "render")]
    pub fn prepared_content_size(&self) -> Option<(f32, f32)> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state)?;
            state
                .prepared_render
                .as_ref()
                .map(|render| render.content_size())
        })
    }

    /// Return the exact responsive candidates selected for live `<img>`
    /// elements and `<video poster>` resources without loading them. The
    /// browser layer can then fetch them concurrently through the page-owned
    /// transport before synchronous layout or paint observes the cache.
    #[cfg(feature = "render")]
    pub fn pending_render_image_urls(&self) -> Vec<(String, crate::ops::ImageRequestProfile)> {
        let state = self.state.borrow();
        let base_url = document_base_url(&state);
        let Some(dom) = state.dom.as_ref() else {
            return Vec::new();
        };
        let mut urls = Vec::new();
        for id in dom.descendants(dom.document()) {
            let Some(node) = dom.get_node(id) else {
                continue;
            };
            let Some(element) = node.as_element() else {
                continue;
            };
            let candidate = match element.local.as_ref() {
                "img" => state
                    .render_resources
                    .cached_image_element_metadata(dom, id, state.viewport, base_url.as_deref())
                    .map(|(url, _, known, _)| {
                        let profile = match node
                            .get_attribute("crossorigin")
                            .map(|value| value.trim().to_ascii_lowercase())
                            .as_deref()
                        {
                            Some("use-credentials") => {
                                crate::ops::ImageRequestProfile::CorsInclude
                            }
                            Some(_) => crate::ops::ImageRequestProfile::CorsSameOrigin,
                            None => crate::ops::ImageRequestProfile::NoCorsInclude,
                        };
                        (url, profile, known)
                    }),
                "video" => state
                    .render_resources
                    .cached_video_poster_metadata(dom, id, base_url.as_deref())
                    .map(|(url, profile, known, _)| (url, profile, known)),
                _ => None,
            };
            let Some((url, profile, known)) = candidate else {
                continue;
            };
            if !known && !url.starts_with("data:") {
                urls.push((url, profile));
            }
        }
        urls.sort();
        urls.dedup();
        urls
    }

    /// Insert one page-transport resource outcome into the retained renderer
    /// cache. Successful image/font bytes queue one resource-dependent layout
    /// refresh while preserving computed styles and any DOM damage already
    /// queued. A negative outcome cannot change geometry and preserves the
    /// retained layout/scroll.
    #[cfg(feature = "render")]
    pub fn seed_render_resource(&mut self, url: String, bytes: Option<Vec<u8>>) {
        let mut state = self.state.borrow_mut();
        match bytes {
            Some(bytes) => {
                state.render_resources.seed(url, bytes);
                crate::ops::invalidate_render_resource_geometry(&mut state);
            }
            None => state.render_resources.seed_missing(url),
        }
    }

    #[cfg(feature = "render")]
    pub fn seed_render_image_resource(
        &mut self,
        url: String,
        profile: crate::ops::ImageRequestProfile,
        bytes: Option<Vec<u8>>,
    ) {
        let mut state = self.state.borrow_mut();
        match bytes {
            Some(bytes) if obscura_render::image_intrinsic_dimensions(&bytes).is_some() => {
                let needs_geometry = match (&state.prepared_render, &state.dom) {
                    (Some(prepared), Some(dom)) => {
                        prepared.image_resource_needs_geometry(dom, &url, profile)
                    }
                    _ => true,
                };
                state.render_resources.seed_image(url, profile, bytes);
                state.activity_generation = state.activity_generation.wrapping_add(1);
                if needs_geometry {
                    crate::ops::invalidate_render_resource_geometry(&mut state);
                }
            }
            _ => state.render_resources.seed_image_missing(url, profile),
        }
    }

    #[cfg(feature = "render")]
    pub fn render_resource_is_known(&self, url: &str) -> bool {
        self.state.borrow().render_resources.has_live_outcome(url)
    }

    #[cfg(feature = "render")]
    pub fn render_image_resource_is_known(
        &self,
        url: &str,
        profile: crate::ops::ImageRequestProfile,
    ) -> bool {
        self.state
            .borrow()
            .render_resources
            .has_live_image_outcome(url, profile)
    }

    /// Run __obscura_init() after all per-page properties (UA, platform, stealth, etc.)
    /// have been set. Must be called once per page setup, after all set_* methods.
    pub fn run_page_init(&mut self) {
        let _ = self.execute_runtime_script(
            "<obscura:page-init>",
            "globalThis.__obscura_init();".to_string(),
        );
    }

    /// Override the coordinates the navigator.geolocation shim reports. The
    /// values are injected as numeric globals the bootstrap reads; when unset it
    /// keeps the built-in default. Callers validate the range before calling.
    pub fn set_geolocation(&mut self, latitude: f64, longitude: f64) {
        let _ = self.execute_runtime_script(
            "<set-geo>",
            format!(
                "globalThis.__obscura_geo_lat={};globalThis.__obscura_geo_lon={};",
                latitude, longitude
            ),
        );
    }

    pub fn evaluate(&mut self, expression: &str) -> Result<serde_json::Value, String> {
        self.begin_javascript_task();
        let wrapped = Self::wrap_expression(expression);
        let result = self
            .execute_runtime_script("<eval>", wrapped)
            .map_err(|e| format!("JS error: {}", e))?;
        self.v8_to_json(result)
    }

    pub async fn evaluate_for_cdp(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.evaluate_for_cdp_with_timeout(
            expression,
            return_by_value,
            await_promise,
            DEFAULT_CDP_AWAIT_TIMEOUT_MS,
        )
        .await
    }

    pub async fn evaluate_for_cdp_with_timeout(
        &mut self,
        expression: &str,
        return_by_value: bool,
        await_promise: bool,
        await_timeout_ms: u64,
    ) -> Result<RemoteObjectInfo, String> {
        if !await_promise && return_by_value {
            let val = self.evaluate(expression)?;
            return Ok(Self::info_from_json(&val));
        }
        self.begin_javascript_task();

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        // Same trailing-semicolon trim as wrap_expression — Playwright's
        // utility-script eval ends with `})();`, and `({expr})` would
        // otherwise become `(...;)` which is a parse-time SyntaxError.
        let cleaned_expr = expression
            .trim()
            .trim_end_matches(|c: char| c == ';' || c.is_whitespace());

        // Puppeteer / Playwright bundles end with a `//# sourceURL=...`
        // line comment. If we put `{expr})` on a single line the comment
        // swallows the closing paren and our wrapper breaks. A newline
        // before the `)` terminates any trailing line comment so the
        // parens close on their own line.
        let done_counter = self.object_counter;
        let meta_code = if await_promise {
            format!(
                "(async function() {{\n\
                    try {{\n\
                        var __result = await (\n{expr}\n);\n\
                        globalThis.__obscura_objects['{oid}'] = __result;\n\
                        globalThis.__obscura_await_meta = {meta_fn};\n\
                        globalThis.__obscura_await_rejected = false;\n\
                    }} catch(e) {{\n\
                        globalThis.__obscura_objects['{oid}'] = e;\n\
                        globalThis.__obscura_await_meta = {err_meta_fn};\n\
                        globalThis.__obscura_await_rejected = true;\n\
                    }}\n\
                    globalThis.__obscura_done_{done_counter} = true;\n\
                }})()",
                expr = cleaned_expr,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
                err_meta_fn = Self::meta_extract_js("e"),
                done_counter = done_counter,
            )
        } else {
            format!(
                "(function() {{\n\
                    var __result;\n\
                    try {{ __result = (\n{expr}\n); }} catch(e) {{ __result = undefined; }}\n\
                    globalThis.__obscura_objects['{oid}'] = __result;\n\
                    return {meta_fn};\n\
                }})()",
                expr = cleaned_expr,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
            )
        };

        let result = self
            .execute_runtime_script("<eval-remote>", meta_code)
            .map_err(|e| format!("JS error: {}", e))?;

        let meta_str = if await_promise {
            let __t0 = std::time::Instant::now();
            let sentinel = format!("globalThis.__obscura_done_{done_counter} === true");
            let settled = self
                .resolve_promises_until(
                    |rt| {
                        rt.execute_runtime_script("<done?>", sentinel.clone())
                            .ok()
                            .and_then(|v| rt.v8_to_json(v).ok())
                            .and_then(|j| j.as_bool())
                            .unwrap_or(false)
                    },
                    await_timeout_ms,
                )
                .await;
            if !settled {
                return Err(format!(
                    "Runtime.evaluate promise did not settle within {await_timeout_ms}ms"
                ));
            }
            let __dt = __t0.elapsed();
            if __dt > std::time::Duration::from_secs(1) {
                let preview: String = expression
                    .chars()
                    .take(200)
                    .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                    .collect();
                tracing::debug!(
                    "Runtime.evaluate awaitPromise took {}ms; expr={}",
                    __dt.as_millis(),
                    preview,
                );
            }
            let rejected = self
                .execute_runtime_script(
                    "<readRejected>",
                    "globalThis.__obscura_await_rejected".to_string(),
                )
                .map_err(|e| format!("JS error: {}", e))?;
            if self.v8_to_json(rejected)?.as_bool().unwrap_or(false) {
                let err = self.execute_runtime_script("<readError>", format!("String(globalThis.__obscura_objects['{0}'] && (globalThis.__obscura_objects['{0}'].message || globalThis.__obscura_objects['{0}']))", oid))
                    .map_err(|e| format!("JS error: {}", e))?;
                return Err(format!(
                    "Promise rejected: {}",
                    self.v8_to_json(err)?.as_str().unwrap_or("")
                ));
            }
            self.execute_runtime_script("<readMeta>", "globalThis.__obscura_await_meta".to_string())
                .map_err(|e| format!("JS error: {}", e))?
        } else {
            result
        };
        let meta_str = self.v8_to_json(meta_str)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str)
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );

        if await_promise && return_by_value {
            let read = self
                .execute_runtime_script(
                    "<readResult>",
                    format!("globalThis.__obscura_objects['{}']", oid),
                )
                .map_err(|e| format!("JS error: {}", e))?;
            let json_val = self.v8_to_json(read)?;
            return Ok(Self::info_from_json(&json_val));
        }

        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    pub async fn call_function_on_for_cdp(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.call_function_on_for_cdp_with_timeout(
            function_declaration,
            object_id,
            arguments,
            return_by_value,
            await_promise,
            DEFAULT_CDP_AWAIT_TIMEOUT_MS,
        )
        .await
    }

    pub async fn call_function_on_for_cdp_with_timeout(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
        await_promise: bool,
        await_timeout_ms: u64,
    ) -> Result<RemoteObjectInfo, String> {
        self.begin_javascript_task();
        let this_expr = self.resolve_this(object_id);
        let (setup, args_list) = self.build_args(arguments);

        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);

        if await_promise {
            let done_counter = self.object_counter;
            let err_meta_fn = Self::meta_extract_js("__result");
            let code = format!(
                "(async function() {{\n\
                    {setup}\n\
                    var __fn = ({fn_decl});\n\
                    var __this = ({this_expr});\n\
                    var __result;\n\
                    try {{\n\
                        __result = await __fn.call(__this, {args});\n\
                        globalThis.__obscura_objects['{oid}'] = __result;\n\
                        globalThis.__obscura_await_meta = {meta_fn};\n\
                    }} catch(e) {{\n\
                        __result = e;\n\
                        globalThis.__obscura_objects['{oid}'] = e;\n\
                        globalThis.__obscura_await_meta = {err_meta_fn};\n\
                    }} finally {{\n\
                        globalThis.__obscura_done_{done_counter} = true;\n\
                    }}\n\
                }})()",
                setup = setup,
                fn_decl = function_declaration,
                this_expr = this_expr,
                args = args_list,
                oid = oid,
                meta_fn = Self::meta_extract_js("__result"),
                err_meta_fn = err_meta_fn,
                done_counter = done_counter,
            );

            self.execute_runtime_script("<callFnAsync>", code)
                .map_err(|e| format!("JS error: {}", e))?;

            let __t0 = std::time::Instant::now();
            let sentinel = format!("globalThis.__obscura_done_{done_counter} === true");
            let settled = self
                .resolve_promises_until(
                    |rt| {
                        rt.execute_runtime_script("<done?>", sentinel.clone())
                            .ok()
                            .and_then(|v| rt.v8_to_json(v).ok())
                            .and_then(|j| j.as_bool())
                            .unwrap_or(false)
                    },
                    await_timeout_ms,
                )
                .await;
            if !settled {
                return Err(format!(
                    "Runtime.callFunctionOn promise did not settle within {await_timeout_ms}ms"
                ));
            }
            let __dt = __t0.elapsed();
            if __dt > std::time::Duration::from_secs(1) {
                let preview: String = function_declaration
                    .chars()
                    .take(300)
                    .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
                    .collect();
                tracing::debug!(
                    "Runtime.callFunctionOn awaitPromise took {}ms; fn={}",
                    __dt.as_millis(),
                    preview,
                );
            }

            if return_by_value {
                let read = self
                    .execute_runtime_script(
                        "<readResult>",
                        format!("globalThis.__obscura_objects['{}']", oid),
                    )
                    .map_err(|e| format!("JS error: {}", e))?;
                let json_val = self.v8_to_json(read)?;
                return Ok(Self::info_from_json(&json_val));
            }

            let meta_result = self
                .execute_runtime_script("<readMeta>", "globalThis.__obscura_await_meta".to_string())
                .map_err(|e| format!("JS error: {}", e))?;
            let meta_str = self.v8_to_json(meta_result)?;
            let meta_json = if let serde_json::Value::String(s) = &meta_str {
                serde_json::from_str(s).unwrap_or(meta_str.clone())
            } else {
                meta_str
            };
            self.object_store.insert(
                oid.clone(),
                format!("globalThis.__obscura_objects['{}']", oid),
            );
            return Ok(Self::info_from_meta(&meta_json, Some(oid)));
        }

        if return_by_value {
            let code = format!(
                "(function() {{\n\
                    {setup}\n\
                    var __fn = ({fn_decl});\n\
                    var __this = ({this_expr});\n\
                    return __fn.call(__this, {args});\n\
                }})()",
                setup = setup,
                fn_decl = function_declaration,
                this_expr = this_expr,
                args = args_list,
            );
            let result = self
                .execute_runtime_script("<callFnByValue>", code)
                .map_err(|e| format!("JS error: {}", e))?;
            let json_val = self.v8_to_json(result)?;
            return Ok(Self::info_from_json(&json_val));
        }

        let code = format!(
            "(function() {{\n\
                {setup}\n\
                var __fn = ({fn_decl});\n\
                var __this = ({this_expr});\n\
                var __result = __fn.call(__this, {args});\n\
                globalThis.__obscura_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            setup = setup,
            fn_decl = function_declaration,
            this_expr = this_expr,
            args = args_list,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self
            .execute_runtime_script("<callFnRemote>", code)
            .map_err(|e| format!("JS error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }
    pub async fn call_function_on(
        &mut self,
        function_declaration: &str,
        object_id: Option<&str>,
        arguments: &[serde_json::Value],
        return_by_value: bool,
    ) -> Result<RemoteObjectInfo, String> {
        self.call_function_on_for_cdp(
            function_declaration,
            object_id,
            arguments,
            return_by_value,
            false,
        )
        .await
    }
    pub fn store_object(&mut self, js_expression: &str) -> Result<String, String> {
        self.begin_javascript_task();
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!(
            "globalThis.__obscura_objects['{}'] = ({});",
            oid, js_expression,
        );
        self.execute_runtime_script("<store>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(oid)
    }

    pub fn store_object_with_meta(
        &mut self,
        js_expression: &str,
    ) -> Result<RemoteObjectInfo, String> {
        self.begin_javascript_task();
        self.object_counter += 1;
        let oid = self.make_oid(self.object_counter);
        let code = format!(
            "(function() {{\n\
                var __result = (\n{expr}\n);\n\
                globalThis.__obscura_objects['{oid}'] = __result;\n\
                return {meta_fn};\n\
            }})()",
            expr = js_expression,
            oid = oid,
            meta_fn = Self::meta_extract_js("__result"),
        );
        let result = self
            .execute_runtime_script("<store-meta>", code)
            .map_err(|e| format!("Store error: {}", e))?;
        let meta_str = self.v8_to_json(result)?;
        let meta_json = if let serde_json::Value::String(s) = &meta_str {
            serde_json::from_str(s).unwrap_or(meta_str.clone())
        } else {
            meta_str
        };
        self.object_store.insert(
            oid.clone(),
            format!("globalThis.__obscura_objects['{}']", oid),
        );
        Ok(Self::info_from_meta(&meta_json, Some(oid)))
    }

    pub fn release_object(&mut self, object_id: &str) {
        if self.object_store.remove(object_id).is_some() {
            let code = format!("delete globalThis.__obscura_objects['{}'];", object_id,);
            let _ = self.execute_runtime_script("<release>", code);
        }
    }

    pub fn release_object_group(&mut self) {
        let _ = self.execute_runtime_script(
            "<releaseGroup>",
            "globalThis.__obscura_objects = {};".to_string(),
        );
        self.object_store.clear();
    }
    pub async fn load_module(&mut self, url: &str, budget_ms: u64) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(budget_ms);
        let prepared = self.prepare_module(url, budget_ms).await?;
        let remaining_ms = remaining_deadline_ms(deadline).ok_or_else(|| {
            format!(
                "Module {} exhausted its {}ms load+evaluation budget",
                url, budget_ms
            )
        })?;
        self.evaluate_prepared_module(prepared, remaining_ms).await
    }

    pub async fn prepare_module(
        &mut self,
        url: &str,
        budget_ms: u64,
    ) -> Result<PreparedModule, String> {
        let budget = tokio::time::Duration::from_millis(budget_ms);
        let specifier = deno_core::ModuleSpecifier::parse(url)
            .map_err(|e| format!("Invalid module URL {}: {}", url, e))?;
        let loaded_start = self.loaded_module_specifiers.borrow().len();

        // Bound the recursive import-graph fetch. deno_core fetches the graph
        // concurrently through the one page-scoped module loader. Loading the
        // entry from that loader too is important: cookies, configured request
        // headers, redirects, interception, and callbacks must not change at
        // the first import edge.
        // The caller sizes the budget: short for enhancement modules on an
        // already-rendered page, full for an unmounted SPA shell (#205).
        let module_id = match tokio::time::timeout(
            budget,
            self.runtime.load_side_es_module(&specifier),
        )
        .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => return Err(format!("Module load error: {}", e)),
            Err(_) => {
                return Err(format!(
                    "Module graph load timed out after {}ms: {}",
                    budget_ms, url
                ));
            }
        };

        // Return as soon as the module finishes evaluating rather than waiting
        // for the loop to go fully idle: a page timer (setInterval) keeps the
        // loop busy forever and would otherwise burn the whole budget (#374).
        let mut graph_specifiers = self.loaded_module_specifiers.borrow()[loaded_start..].to_vec();
        graph_specifiers.push(specifier.to_string());
        graph_specifiers.sort_unstable();
        graph_specifiers.dedup();

        Ok(PreparedModule {
            module_id,
            description: format!("Module {}", url),
            entry_specifier: Some(specifier.to_string()),
            graph_specifiers,
        })
    }

    /// Drive a just-started module evaluation to completion, or up to
    /// `budget_ms`. Returns as soon as the module finishes rather than waiting
    /// for the event loop to go idle: a page timer (setInterval) keeps the loop
    /// busy forever and would otherwise burn the whole budget, abandoning a
    /// module that had already evaluated (issue #374).
    ///
    /// A module eval error or timeout is returned to the page lifecycle. The
    /// caller may continue rendering, but must not report a failed module as
    /// successfully loaded. An event-loop error is propagated out of the
    /// select and handled the same way.
    async fn drive_module_eval(
        &mut self,
        module_id: deno_core::ModuleId,
        budget_ms: u64,
        what: &str,
    ) -> Result<(), String> {
        if let Some(outcome) = self.module_evaluations.get(&module_id) {
            return outcome.clone();
        }

        self.begin_javascript_task();
        let budget = tokio::time::Duration::from_millis(budget_ms);
        // deno_core 0.350 asserts instead of treating a second evaluation as
        // the module-map no-op required by browsers. The local outcome cache
        // covers duplicate roots prepared by Obscura. A root can also have
        // been evaluated earlier as another graph's dependency, which is only
        // observable when mod_evaluate checks V8's private module status, so
        // contain that dependency assertion at this boundary as well.
        let evaluation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.runtime.mod_evaluate(module_id)
        }));
        let result = match evaluation {
            Ok(result) => result,
            Err(payload) => {
                let message = panic_payload_message(payload.as_ref());
                let outcome = if message.contains("Module already evaluated") {
                    Ok(())
                } else {
                    Err(format!("{} evaluation panicked: {}", what, message))
                };
                self.module_evaluations.insert(module_id, outcome.clone());
                return outcome;
            }
        };
        tokio::pin!(result);

        let outcome = tokio::time::timeout(budget, async {
            let event_loop = self
                .runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default());
            tokio::pin!(event_loop);
            tokio::select! {
                biased;
                e = &mut event_loop => { e?; (&mut result).await }
                r = &mut result => r,
            }
        })
        .await;

        let outcome = match outcome {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("{} eval error: {}", what, e)),
            Err(_) => Err(format!(
                "{} evaluation timed out after {}ms",
                what, budget_ms
            )),
        };
        let outcome = self.finish_heap_checked(outcome);
        self.module_evaluations.insert(module_id, outcome.clone());
        outcome
    }

    pub async fn load_inline_module(
        &mut self,
        code: &str,
        base_url: &str,
        budget_ms: u64,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(budget_ms);
        let prepared = self
            .prepare_inline_module(code, base_url, budget_ms)
            .await?;
        let remaining_ms = remaining_deadline_ms(deadline).ok_or_else(|| {
            format!(
                "Inline module exhausted its {}ms load+evaluation budget",
                budget_ms
            )
        })?;
        self.evaluate_prepared_module(prepared, remaining_ms).await
    }

    pub async fn prepare_inline_module(
        &mut self,
        code: &str,
        base_url: &str,
        budget_ms: u64,
    ) -> Result<PreparedModule, String> {
        let budget = tokio::time::Duration::from_millis(budget_ms);
        // Inline modules use the document base URL as their module URL. This is
        // observable through import.meta.url and is also the referrer used for
        // relative imports and import-map scope matching. deno_core permits
        // multiple side modules with this name; the returned ModuleId keeps
        // each prepared module distinct until its scheduled evaluation.
        let specifier = deno_core::ModuleSpecifier::parse(base_url)
            .unwrap_or_else(|_| deno_core::ModuleSpecifier::parse("about:blank").unwrap());
        let loaded_start = self.loaded_module_specifiers.borrow().len();

        let module_id = match tokio::time::timeout(
            budget,
            self.runtime.load_side_es_module_from_code(
                &specifier,
                deno_core::ModuleCodeString::from(code.to_string()),
            ),
        )
        .await
        {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => return Err(format!("Inline module load error: {}", e)),
            Err(_) => {
                return Err(format!(
                    "Inline module graph load timed out after {}ms",
                    budget_ms
                ));
            }
        };

        // Return as soon as the module finishes evaluating rather than waiting
        // for idle: Vite's HMR / React-Refresh client installs a setInterval that
        // keeps the loop busy forever, and waiting for idle burned the whole
        // budget on this preamble module and starved the module that mounts the
        // app, leaving #root empty (issue #374).
        let mut graph_specifiers = self.loaded_module_specifiers.borrow()[loaded_start..].to_vec();
        graph_specifiers.sort_unstable();
        graph_specifiers.dedup();

        Ok(PreparedModule {
            module_id,
            description: "Inline module".to_string(),
            // Multiple inline modules intentionally share the document URL,
            // but each has its own source and ModuleId.
            entry_specifier: None,
            graph_specifiers,
        })
    }

    pub async fn evaluate_prepared_module(
        &mut self,
        prepared: PreparedModule,
        budget_ms: u64,
    ) -> Result<(), String> {
        let PreparedModule {
            module_id,
            description,
            entry_specifier,
            graph_specifiers,
        } = prepared;
        if let Some(outcome) = entry_specifier
            .as_ref()
            .and_then(|specifier| self.evaluated_module_specifiers.get(specifier))
        {
            return outcome.clone();
        }
        // Tokio timeouts cannot run while synchronous top-level module work
        // pins the runtime thread in V8. Pair the async timeout with a hard V8
        // watchdog so this budget is a real wall-clock ceiling for both forms
        // of evaluation.
        let watchdog = self.arm_watchdog(std::time::Duration::from_millis(budget_ms));
        let result = self
            .drive_module_eval(module_id, budget_ms, &description)
            .await;
        let watchdog_fired = self.disarm_watchdog(watchdog);
        let result = if watchdog_fired {
            Err(format!(
                "{} evaluation timed out after {}ms",
                description, budget_ms
            ))
        } else {
            result
        };

        if let Some(entry_specifier) = entry_specifier {
            self.evaluated_module_specifiers
                .insert(entry_specifier, result.clone());
        }
        if result.is_ok() {
            for specifier in graph_specifiers {
                self.evaluated_module_specifiers.insert(specifier, Ok(()));
            }
        }
        result
    }

    fn execute_classic_script(&mut self, name: &str, source: &str) -> Result<(), String> {
        self.begin_javascript_task();
        // JsRuntime::execute_script in deno_core 0.350 restricts `name` to a
        // &'static str. Browser script URLs are runtime data, and V8 uses this
        // origin as import()'s referrer, so compile in the runtime's main
        // context directly instead of substituting the fixed "<script>" name.
        let result = (|| {
            let scope = &mut self.runtime.handle_scope();
            let source = deno_core::v8::String::new(scope, source)
                .ok_or_else(|| "JS error: source allocation failed".to_string())?;
            let name = deno_core::v8::String::new(scope, name)
                .ok_or_else(|| "JS error: script URL allocation failed".to_string())?;
            let origin = deno_core::v8::ScriptOrigin::new(
                scope,
                name.into(),
                0,
                0,
                false,
                0,
                None,
                false,
                false,
                false,
                None,
            );
            let scope = &mut deno_core::v8::TryCatch::new(scope);
            let script = deno_core::v8::Script::compile(scope, source, Some(&origin));
            let Some(script) = script else {
                if scope.is_execution_terminating() {
                    scope.cancel_terminate_execution();
                    return Err("JS error: Uncaught Error: execution terminated".to_string());
                }
                return match scope.exception() {
                    Some(exception) => {
                        let error = deno_core::error::JsError::from_v8_exception(scope, exception);
                        Err(format!("JS error: {error}"))
                    }
                    None => {
                        Err("JS error: script compilation failed without an exception".to_string())
                    }
                };
            };
            if script.run(scope).is_none() {
                if scope.is_execution_terminating() {
                    scope.cancel_terminate_execution();
                    return Err("JS error: Uncaught Error: execution terminated".to_string());
                }
                return match scope.exception() {
                    Some(exception) => {
                        let error = deno_core::error::JsError::from_v8_exception(scope, exception);
                        Err(format!("JS error: {error}"))
                    }
                    None => {
                        Err("JS error: script execution failed without an exception".to_string())
                    }
                };
            }
            Ok(())
        })();
        self.finish_heap_checked(result)
    }

    pub fn execute_script(&mut self, name: &str, source: &str) -> Result<(), String> {
        self.execute_classic_script(name, source)
    }

    pub fn execute_script_guarded(&mut self, name: &str, source: &str) -> Result<(), String> {
        if source.len() < 10_000 {
            self.execute_script(name, source)
        } else {
            self.execute_script_with_timeout(name, source, std::time::Duration::from_secs(5))
        }
    }

    pub fn execute_script_with_timeout(
        &mut self,
        name: &str,
        source: &str,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        if timeout.is_zero() {
            return self.execute_classic_script(name, source);
        }

        let isolate_handle = self.runtime.v8_isolate().thread_safe_handle();

        let pair = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let pair_clone = pair.clone();

        let watchdog = std::thread::spawn(move || {
            let (lock, cvar) = &*pair_clone;
            let mut cancelled = lock.lock().unwrap();
            let deadline = std::time::Instant::now() + timeout;

            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    isolate_handle.terminate_execution();
                    return;
                }

                let result = cvar.wait_timeout(cancelled, remaining).unwrap();
                cancelled = result.0;
                if *cancelled {
                    return;
                }
            }
        });

        let result = self.execute_classic_script(name, source);

        {
            let (lock, cvar) = &*pair;
            let mut cancelled = lock.lock().unwrap();
            *cancelled = true;
            cvar.notify_one();
        }
        let _ = watchdog.join();

        match result {
            Ok(()) => Ok(()),
            Err(msg) => {
                if msg.contains("Uncaught Error: execution terminated") {
                    tracing::warn!("Script killed after {}s timeout", timeout.as_secs());
                    Ok(())
                } else {
                    Err(msg)
                }
            }
        }
    }

    pub async fn run_event_loop(&mut self) -> Result<(), String> {
        self.begin_javascript_task();
        // A browser performs a microtask checkpoint at the end of each task.
        // deno_core's event loop may return immediately when no async op is
        // pending, leaving an already-resolved Promise continuation stranded
        // (document.fonts.load(...).then(...), framework post-render hooks,
        // and hydration follow-ups all rely on this boundary).
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        let result = self
            .runtime
            .run_event_loop(deno_core::PollEventLoopOptions::default())
            .await
            .map_err(|e| format!("Event loop error: {}", e));
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        self.finish_heap_checked(result)
    }

    /// Whether the serialized dynamic-script queue is still fetching or
    /// evaluating a script. The queue stays private to the bootstrap closure;
    /// Rust reads it through a hidden status function so page declarations
    /// cannot collide with or overwrite the queue itself.
    pub fn has_pending_dynamic_scripts(&mut self) -> bool {
        let pending_dom_script = self
            .evaluate("globalThis.__obscura_hasPendingDynamicScripts?.() === true")
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        // A short tail bridges the parser/evaluator hand-off between one
        // fetched module and the next dependency. It is below the lifecycle's
        // existing 500ms fast-settle floor, so static entry graphs pay no new
        // latency while lazy import graphs remain observable. deno_core's
        // dynamic-module evaluation/TLA counters are private; the event-loop
        // pump itself remains responsible for that non-fetch portion.
        pending_dom_script
            || self
                .module_load_activity
                .is_pending_or_recent(std::time::Duration::from_millis(100))
    }

    /// Whether a connected dynamic script prepared before the document load
    /// event still has fetch/evaluation/load-or-error work outstanding.
    ///
    /// This intentionally excludes `import()` and scripts created by a load
    /// handler. Those are ordinary post-load enhancement work and should only
    /// be driven when an automation caller explicitly asks the page to settle.
    pub fn has_pending_load_delaying_scripts(&mut self) -> bool {
        self.evaluate("globalThis.__obscura_hasPendingLoadDelayingScripts?.() === true")
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }

    /// Generation of observable connected-document mutations. This excludes
    /// detached-tree construction and no-op writes, which cannot affect a
    /// screenshot or DOM dump.
    pub fn activity_generation(&self) -> u64 {
        self.state.borrow().activity_generation
    }

    fn has_pending_network_requests(&self) -> bool {
        let state = self.state.borrow();
        state
            .page_in_flight
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    }

    fn next_pending_timeout_delay_ms(&mut self) -> Option<f64> {
        self.evaluate("globalThis.__obscura_nextPendingTimeoutDelay?.() ?? -1")
        .ok()
        .and_then(|value| value.as_f64())
        .filter(|delay| *delay >= 0.0)
    }

    /// Arm a hard wall-clock backstop on synchronous V8 work. A page stuck in a
    /// synchronous loop or a microtask storm pins the OS thread inside V8, so
    /// `tokio::time::timeout` (which can only cancel at await points) never
    /// fires. This spawns a watchdog thread that terminates the isolate once
    /// `budget` elapses, forcing V8 to throw an uncatchable error and hand
    /// control back. Always balance with [`Self::disarm_watchdog`].
    pub fn arm_watchdog(&mut self, budget: std::time::Duration) -> WatchdogToken {
        spawn_watchdog(self.runtime.v8_isolate().thread_safe_handle(), budget)
    }

    /// Stop a watchdog armed by [`Self::arm_watchdog`]. If it had already fired
    /// (terminated the isolate), clear V8's termination flag so the isolate is
    /// usable again, and return `true`.
    pub fn disarm_watchdog(&mut self, token: WatchdogToken) -> bool {
        let fired = token.stop();
        if fired {
            self.runtime.v8_isolate().cancel_terminate_execution();
            tracing::warn!("V8 watchdog fired: terminated a synchronous overrun");
        }
        fired
    }

    /// This runtime's V8 isolate handle (captured at construction, stable for
    /// the isolate's life). Lets the CDP dispatcher arm a per-command watchdog
    /// from `&self`.
    pub fn isolate_handle(&self) -> IsolateHandle {
        self.isolate_handle.clone()
    }

    /// Clear V8's termination flag after a watchdog armed externally (via the
    /// isolate handle) fired, so the isolate is usable for the next command.
    /// No-op when the isolate is not terminating.
    pub fn cancel_termination(&mut self) {
        self.runtime.v8_isolate().cancel_terminate_execution();
    }

    /// Drive the event loop for at most `budget_ms`, bounded against BOTH async
    /// idle (Tokio deadline) and synchronous hangs (V8 watchdog). The deadline
    /// is observed between browser tasks; a task already running there gets a
    /// five-second completion allowance plus a 500ms scheduling margin before
    /// the watchdog terminates it. A well-behaved page returns as soon as the
    /// loop goes idle.
    pub async fn run_event_loop_bounded(&mut self, budget_ms: u64) -> Result<(), String> {
        if budget_ms == 0 {
            return self.run_event_loop().await;
        }
        let budget = std::time::Duration::from_millis(budget_ms);
        let deadline = tokio::time::Instant::now() + budget;
        // A capture/readiness deadline is observed only between browser tasks.
        // Chromium does not terminate the JavaScript task which happens to be
        // active when a screenshot delay expires; the capture waits for that
        // task boundary. Keep a separate long-task floor so short compositor
        // slices and explicit waits do not kill legitimate framework work,
        // while an actually unyielding task remains bounded.
        // One watchdog for the complete pump avoids spawning a native thread
        // per cooperative task. Adding the floor after the observation budget
        // guarantees that even a task beginning just before `deadline` gets
        // the same bounded completion allowance.
        let synchronous_budget = budget
            .saturating_add(std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS));
        let token =
            self.arm_watchdog(synchronous_budget
                + std::time::Duration::from_millis(WATCHDOG_SCHEDULING_MARGIN_MS));
        let result = loop {
            if tokio::time::Instant::now() >= deadline {
                break Ok(());
            }

            match tokio::time::timeout_at(deadline, self.run_cooperative_event_loop_tick()).await {
                Ok(Ok(true)) => break Ok(()),
                Ok(Ok(false)) => {
                    // End-of-task microtasks belong to this turn, but work
                    // queued from them belongs to a subsequent cooperative
                    // turn. Yield so the wall deadline remains observable even
                    // when every turn immediately schedules another one.
                    self.runtime.v8_isolate().perform_microtask_checkpoint();
                    tokio::task::yield_now().await;
                }
                Ok(Err(error)) => break Err(error),
                Err(_) => break Ok(()),
            }
        };
        let fired = self.disarm_watchdog(token);
        match result {
            Err(error) if error.contains("heap limit exceeded") => Err(error),
            Err(error) if fired || error.contains("execution terminated") => Ok(()),
            other => other,
        }
    }

    /// Drive page tasks for a fixed observation interval without asking
    /// deno_core's run-to-idle future to own that entire interval.
    ///
    /// Modern schedulers commonly keep the event loop continuously ready with
    /// animation frames, zero-delay tasks, or streaming work. A single
    /// `run_event_loop()` poll then never yields to Tokio, so the fixed-delay
    /// deadline can only be enforced by terminating otherwise valid page JS.
    /// Cooperative turns preserve the requested wall interval while returning
    /// to the embedder between task-queue wakes. The watchdog remains solely as
    /// a backstop for one genuinely synchronous, unyielding turn.
    pub async fn run_event_loop_for_duration(&mut self, budget_ms: u64) -> Result<(), String> {
        if budget_ms == 0 {
            return Ok(());
        }
        self.run_event_loop_bounded(budget_ms).await
    }

    /// Drive one deno_core event-loop tick at a time. When the first tick
    /// parks, process one more tick after its registered waker fires, then
    /// yield back to the embedder even if that tick schedules more work.
    ///
    /// `JsRuntime::run_event_loop()` is a run-to-idle future. When a page keeps
    /// it continuously ready (zero-delay schedulers, streaming traffic, or a
    /// framework work queue), Tokio never regains control to observe a timeout
    /// or our readiness policy. This future deliberately turns the wake for a
    /// second tick into a return to the caller. If no work is immediately
    /// ready, it remains parked on deno_core's real I/O/timer waker, so the
    /// adaptive settle loop does not poll at a fixed frequency.
    async fn run_cooperative_event_loop_tick(&mut self) -> Result<bool, String> {
        self.begin_javascript_task();
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        let mut waiting_for_wake = false;
        let result = std::future::poll_fn(|cx| {
            let tick = self
                .runtime
                .poll_event_loop(cx, deno_core::PollEventLoopOptions::default());
            match tick {
                std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(true)),
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(format!(
                    "Event loop error: {error}"
                ))),
                std::task::Poll::Pending if waiting_for_wake => {
                    std::task::Poll::Ready(Ok(false))
                }
                std::task::Poll::Pending => {
                    waiting_for_wake = true;
                    std::task::Poll::Pending
                }
            }
        })
        .await;
        self.finish_heap_checked(result)
    }

    /// Drive one browser task while allowing the future to remain parked on
    /// deno_core's real timer/network waker. This is the long-lived browser
    /// server counterpart to bounded screenshot settling: the owner selects
    /// this future alongside incoming protocol commands, so a page continues
    /// to make progress while the automation client is idle without polling at
    /// a fixed frequency.
    ///
    /// The shared CDP watchdog is armed only around synchronous V8 entry. It is
    /// deliberately disarmed while `Poll::Pending`; a legitimate distant timer
    /// must not look like a hung JavaScript task merely because the runtime is
    /// asleep waiting for it.
    #[doc(hidden)]
    pub async fn run_autonomous_event_loop_turn(&mut self) -> Result<bool, String> {
        const AUTONOMOUS_TASK_WATCHDOG_MS: u64 =
            SYNCHRONOUS_TASK_FLOOR_MS + WATCHDOG_SCHEDULING_MARGIN_MS;

        self.begin_javascript_task();

        let checkpoint_watchdog = crate::cdp_watchdog::arm(
            self.isolate_handle(),
            std::time::Duration::from_millis(AUTONOMOUS_TASK_WATCHDOG_MS),
        );
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        if crate::cdp_watchdog::disarm(checkpoint_watchdog) {
            self.cancel_termination();
            return Err("autonomous microtask checkpoint exceeded its task budget".into());
        }
        if self.recover_heap_limit() {
            return Err("JavaScript heap limit exceeded; execution terminated".into());
        }

        let isolate_handle = self.isolate_handle();
        let mut waiting_for_wake = false;
        let result = std::future::poll_fn(|cx| {
            let watchdog = crate::cdp_watchdog::arm(
                isolate_handle.clone(),
                std::time::Duration::from_millis(AUTONOMOUS_TASK_WATCHDOG_MS),
            );
            let tick = self
                .runtime
                .poll_event_loop(cx, deno_core::PollEventLoopOptions::default());
            let watchdog_fired = crate::cdp_watchdog::disarm(watchdog);
            if watchdog_fired {
                self.runtime.v8_isolate().cancel_terminate_execution();
                return std::task::Poll::Ready(Err(
                    "autonomous browser task exceeded its task budget".into(),
                ));
            }
            match tick {
                std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(true)),
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(format!(
                    "Event loop error: {error}"
                ))),
                std::task::Poll::Pending if waiting_for_wake => {
                    std::task::Poll::Ready(Ok(false))
                }
                std::task::Poll::Pending => {
                    waiting_for_wake = true;
                    std::task::Poll::Pending
                }
            }
        })
        .await;
        self.finish_heap_checked(result)
    }

    /// Drive one cooperative event-loop turn for browser lifecycle code that
    /// must re-check an external readiness predicate after every wake. The
    /// boolean is true only when deno_core reached full idle.
    #[doc(hidden)]
    pub async fn run_load_delaying_event_loop_tick(&mut self) -> Result<bool, String> {
        self.run_cooperative_event_loop_tick().await
    }

    /// Pump deferred work until deno_core reports true idle, or until the page
    /// has had no connected-document mutation, relevant request/dynamic-script
    /// work, or near-term one-shot timeout for `quiet_ms`. Network and script
    /// work gets a bounded post-load grace period: this retains ordinary app
    /// hydration without allowing analytics, telemetry, or a hung endpoint to
    /// consume the caller's complete budget. Long timers and perpetual visual
    /// mutations are bounded separately for the same reason.
    /// `budget_ms` remains an absolute wall-clock bound.
    pub async fn run_event_loop_until_quiescent(
        &mut self,
        budget_ms: u64,
        quiet_ms: u64,
    ) -> Result<(), String> {
        if budget_ms == 0 {
            return Ok(());
        }

        let budget = std::time::Duration::from_millis(budget_ms);
        let quiet = std::time::Duration::from_millis(quiet_ms.max(1).min(budget_ms));
        let started = tokio::time::Instant::now();
        let deadline = started + budget;
        // A one-second grace covers the common load -> fetch -> framework
        // commit path (and matches the CLI's established one-second useful
        // hydration window), but it is intentionally independent of a larger
        // caller budget. Requests which remain pending after this point are no
        // longer readiness evidence by themselves. Their eventual connected
        // DOM mutation is still observed during the bounded activity tail.
        const EXTERNAL_WORK_GRACE_MS: u64 = 1_000;
        const OBSERVABLE_ACTIVITY_TAIL_MS: u64 = 500;
        let external_work_grace =
            std::time::Duration::from_millis(EXTERNAL_WORK_GRACE_MS).min(budget);
        let external_work_deadline = started + external_work_grace;
        let activity_tail = std::time::Duration::from_millis(OBSERVABLE_ACTIVITY_TAIL_MS);
        let mut activity_deadline = deadline.min(started + activity_tail);
        let token = self.arm_watchdog(
            budget
                .saturating_add(std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS))
                + std::time::Duration::from_millis(WATCHDOG_SCHEDULING_MARGIN_MS),
        );
        let mut generation = self.activity_generation();
        let mut quiet_since: Option<tokio::time::Instant> = None;
        let result = loop {
            let now = tokio::time::Instant::now();
            let Some(_remaining) = deadline.checked_duration_since(now) else {
                break Ok(());
            };
            let next_generation = self.activity_generation();
            // One-shot timers up to two quiet windows away are commonly app
            // hydration/debounce work (`setTimeout(render, 200)`). Intervals
            // are intentionally excluded, and distant one-shots are treated
            // like Chromium after `load`: callers needing an arbitrary fixed
            // delay can request strict settle.
            let near_timeout = self
                .next_pending_timeout_delay_ms()
                .is_some_and(|delay| delay <= quiet.as_secs_f64() * 2_000.0);
            let external_work_pending = now < external_work_deadline
                && (self.has_pending_network_requests() || self.has_pending_dynamic_scripts());
            if external_work_pending {
                activity_deadline = deadline.min(external_work_deadline + activity_tail);
                generation = next_generation;
                quiet_since = None;
            } else if now < activity_deadline && near_timeout {
                generation = next_generation;
                quiet_since = None;
            } else {
                if now < activity_deadline && next_generation != generation {
                    // A mutation starts a fresh quiet interval at its observed
                    // delivery time. There is no need for a fixed-rate poll to
                    // discover that the interval has begun.
                    quiet_since = Some(now);
                }
                generation = next_generation;
                let since = quiet_since.get_or_insert(now);
                if now.duration_since(*since) >= quiet {
                    break Ok(());
                }
            }

            // Park on the runtime's actual waker. The policy deadline is only
            // a fallback for a hung request, a quiet-window expiry, or the
            // caller's absolute budget; it is not a periodic polling quantum.
            let policy_deadline = if external_work_pending {
                external_work_deadline
            } else if now < activity_deadline && near_timeout {
                activity_deadline
            } else {
                quiet_since.map_or(deadline, |since| since + quiet)
            }
            .min(deadline);
            // deno_core's public poll is one event-loop iteration, but an
            // iteration may synchronously drain an arbitrarily long chain of
            // nextTick/macrotask/microtask callbacks before returning. Tokio's
            // deadline cannot preempt that native V8 call. Bound the individual
            // turn beyond the readiness horizon by the same bounded task
            // allowance as fixed waits. The observation window may expire
            // while valid framework/layout work is running; browser capture
            // waits for that task boundary instead of terminating it midway.
            let tick_watchdog = self.arm_watchdog(
                policy_deadline.saturating_duration_since(now)
                    + std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS)
                    + std::time::Duration::from_millis(WATCHDOG_SCHEDULING_MARGIN_MS),
            );
            let tick = tokio::time::timeout_at(
                policy_deadline,
                self.run_cooperative_event_loop_tick(),
            )
            .await;
            let tick_fired = self.disarm_watchdog(tick_watchdog);
            if tick_fired {
                break Ok(());
            }
            self.runtime.v8_isolate().perform_microtask_checkpoint();
            match tick {
                Ok(Ok(true)) => break Ok(()),
                Ok(Ok(false)) | Err(_) => {}
                Ok(Err(error)) => break Err(error),
            }
        };
        let fired = self.disarm_watchdog(token);
        match result {
            Err(error) if error.contains("heap limit exceeded") => Err(error),
            Err(error) if fired || error.contains("execution terminated") => Ok(()),
            other => other,
        }
    }

    /// Like [`Self::evaluate`] but bounded by a V8 watchdog, so a `--eval`
    /// expression that loops forever (or awaits a promise that never settles in
    /// synchronous form) cannot hang the process.
    pub fn evaluate_with_timeout(
        &mut self,
        expression: &str,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value, String> {
        if timeout.is_zero() {
            return self.evaluate(expression);
        }
        self.begin_javascript_task();
        let wrapped = Self::wrap_expression(expression);
        let token = self.arm_watchdog(timeout);
        let result = self.runtime.execute_script("<eval>", wrapped);
        let fired = self.disarm_watchdog(token);
        if self.recover_heap_limit() {
            return Err("JavaScript heap limit exceeded; execution terminated".to_string());
        }
        match result {
            Ok(v) if !fired => self.v8_to_json(v),
            Ok(_) => Err("eval timed out".to_string()),
            Err(e) => {
                let msg = e.to_string();
                if fired || msg.contains("execution terminated") {
                    Err("eval timed out".to_string())
                } else {
                    Err(format!("JS error: {}", msg))
                }
            }
        }
    }

    pub async fn resolve_promises(&mut self) {
        self.begin_javascript_task();
        // Default settle: just pump until idle or 5s.
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            self.runtime
                .run_event_loop(deno_core::PollEventLoopOptions::default()),
        )
        .await;
        self.recover_heap_limit();
    }

    /// Pump the event loop until `done_check` returns true (e.g. an IIFE
    /// has written its result sentinel), or `max_total_ms` elapses. Returns
    /// whether the predicate completed before the deadline.
    ///
    /// Why this exists: `run_event_loop(default)` only returns when there is
    /// no pending work. Page JS routinely schedules long setTimeouts
    /// (IntersectionObserver re-fires at 7s, requestIdleCallback, etc.) that
    /// the caller does not care about. With the plain timeout we waited 5s
    /// even when the IIFE we cared about resolved in <1ms — the click flow
    /// added ~7s per click because Puppeteer's `isIntersectingViewport`
    /// disconnects its observer in the callback, but our scheduled
    /// re-fires keep the event loop "busy" until they all fire.
    pub async fn resolve_promises_until<F>(
        &mut self,
        mut done_check: F,
        max_total_ms: u64,
    ) -> bool
    where
        F: FnMut(&mut Self) -> bool,
    {
        let deadline =
            tokio::time::Instant::now() + tokio::time::Duration::from_millis(max_total_ms);
        let mut tick_ms: u64 = 1;
        loop {
            self.begin_javascript_task();
            if done_check(self) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            // Pump for a short slice. If the loop returns idle in <tick_ms,
            // run_event_loop returns Ok and we check the predicate again.
            let _ = tokio::time::timeout(
                tokio::time::Duration::from_millis(tick_ms),
                self.runtime
                    .run_event_loop(deno_core::PollEventLoopOptions::default()),
            )
            .await;
            if self.recover_heap_limit() {
                return false;
            }
            // Backoff so a hung promise doesn't burn CPU. Caps at 50ms;
            // worst case we miss the result by <50ms.
            if tick_ms < 50 {
                tick_ms = (tick_ms * 2).min(50);
            }
        }
    }
    pub fn take_dom(&self) -> Option<DomTree> {
        let mut state = self.state.borrow_mut();
        #[cfg(feature = "render")]
        {
            state.prepared_render = None;
            state.pending_style_mutations.clear();
            state.render_resources = obscura_render::RenderResourceCache::default();
            state.stylesheet_cache = obscura_render::StylesheetCache::default();
            state.dynamic_fonts.clear();
            state.element_scroll_offsets.clear();
            state.resolved_scroll = None;
        }
        state.dom.take()
    }

    /// Export document-owned script preparation state before the runtime realm
    /// is temporarily destroyed.  Page suspension keeps the DOM alive, so the
    /// HTML "already started" flags must travel with it rather than resetting
    /// like window-global JavaScript state.
    pub fn started_script_ids(&self) -> Vec<u32> {
        let state = self.state.borrow();
        let mut ids = state
            .already_started_scripts
            .borrow()
            .iter()
            .map(|node_id| node_id.raw())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    /// Restore script preparation state only onto script nodes in the current
    /// DOM.  Callers use this exclusively for the same DomTree surviving a
    /// suspend/resume cycle; normal set_dom navigation starts from an empty set.
    pub fn restore_started_script_ids(&self, ids: &[u32]) {
        let state = self.state.borrow();
        let Some(dom) = state.dom.as_ref() else {
            return;
        };
        let valid = ids
            .iter()
            .copied()
            .map(NodeId::new)
            .filter(|node_id| node_is_script(dom, *node_id))
            .collect::<Vec<_>>();
        state.already_started_scripts.borrow_mut().extend(valid);
    }

    pub fn with_dom<R>(&self, f: impl FnOnce(&DomTree) -> R) -> Option<R> {
        let state = self.state.borrow();
        state.dom.as_ref().map(f)
    }

    /// Absolute URLs the page requested via fetch()/XHR, in request order
    /// (issue #301). Backs `--dump assets`.
    pub fn fetched_urls(&self) -> Vec<String> {
        self.state.borrow().fetched_urls.clone()
    }

    /// Drain the network events recorded for script-initiated requests
    /// (fetch/XHR/dynamic resource). The Page moves these into its own
    /// network_events so the CDP layer emits Network events for them (#406).
    pub fn take_js_network_events(&self) -> Vec<crate::ops::JsNetworkEvent> {
        std::mem::take(&mut self.state.borrow_mut().js_network_events)
    }

    pub fn dom_ref(&self) -> Option<std::cell::Ref<'_, Option<DomTree>>> {
        let r = self.state.borrow();
        if r.dom.is_some() {
            Some(std::cell::Ref::map(r, |s| &s.dom))
        } else {
            None
        }
    }
    fn make_oid(&self, counter: u64) -> String {
        format!("{{\"injectedScriptId\":1,\"id\":{}}}", counter)
    }

    fn wrap_expression(expression: &str) -> String {
        let trimmed = expression.trim();

        let is_multi_statement = trimmed.starts_with("var ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("if ")
            || trimmed.starts_with("for ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("return ");

        if is_multi_statement {
            format!(
                "(function() {{ try {{\n{}\n}} catch(e) {{ return null; }} }})()",
                expression
            )
        } else {
            // Strip trailing semicolons + whitespace before wrapping in
            // `return (...);`. Playwright's utility-script expression is
            // an IIFE that ends with `})();` — leaving the `;` in place
            // produces `return (...;);`, a SyntaxError. The script fails
            // to parse, the catch never fires (parse errors are not
            // catchable), and the function silently returns `undefined`.
            // Stripping makes the wrapped expression syntactically valid.
            //
            // The newline before the trailing `)` also terminates any
            // `//# sourceURL=...` line comment the caller may have appended
            // (Puppeteer's evaluated bundles do).
            let cleaned = trimmed.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
            format!(
                "(function() {{ try {{ return (\n{}\n); }} catch(e) {{ return null; }} }})()",
                cleaned
            )
        }
    }

    fn meta_extract_js(var_name: &str) -> String {
        format!(
            r#"(function(v) {{
                var t = typeof v;
                var st = null, cn = '', desc = '';
                if (v === null) {{ t = 'object'; st = 'null'; }}
                else if (v === undefined) {{ t = 'undefined'; }}
                else if (Array.isArray(v)) {{
                    st = 'array'; cn = 'Array';
                    desc = 'Array(' + v.length + ')';
                }}
                else if (t === 'object' && typeof v._nid === 'number') {{
                    st = 'node';
                    cn = v.constructor ? v.constructor.name : 'Node';
                    if (v.nodeType === 9) cn = 'HTMLDocument';
                    else if (v.nodeType === 1) cn = 'HTML' + (v.tagName || 'Element').charAt(0) + (v.tagName || 'Element').slice(1).toLowerCase() + 'Element';
                    desc = v.tagName ? v.tagName.toLowerCase() : (v.nodeName || 'node');
                }}
                else if (t === 'function') {{
                    cn = 'Function';
                    desc = v.name ? 'function ' + v.name + '()' : 'function()';
                }}
                else if (t === 'object') {{
                    cn = (v.constructor && v.constructor.name) || 'Object';
                    desc = cn;
                }}
                else {{ desc = String(v); }}
                return JSON.stringify({{type:t,subtype:st,className:cn,description:desc}});
            }})({var_name})"#,
            var_name = var_name,
        )
    }

    fn resolve_this(&self, object_id: Option<&str>) -> String {
        match object_id {
            Some(oid) => {
                if let Some(retrieval) = self.object_store.get(oid) {
                    retrieval.clone()
                } else if oid.starts_with("node-") {
                    let nid = oid.strip_prefix("node-").unwrap_or("0");
                    format!(
                        "(function() {{ \
                            var nid = {}; \
                            var cache = globalThis._cache || new Map(); \
                            if (cache.has(nid)) return cache.get(nid); \
                            return null; \
                        }})()",
                        nid
                    )
                } else {
                    "globalThis".to_string()
                }
            }
            None => "globalThis".to_string(),
        }
    }

    fn build_args(&self, arguments: &[serde_json::Value]) -> (String, String) {
        let mut setup_lines = Vec::new();
        let mut arg_names = Vec::new();

        for (i, arg) in arguments.iter().enumerate() {
            let arg_name = format!("__arg{}", i);
            if let Some(value) = arg.get("value") {
                let json_str =
                    serde_json::to_string(value).unwrap_or_else(|_| "undefined".to_string());
                setup_lines.push(format!("var {} = {};", arg_name, json_str));
            } else if let Some(oid) = arg.get("objectId").and_then(|v| v.as_str()) {
                if let Some(retrieval) = self.object_store.get(oid) {
                    setup_lines.push(format!("var {} = {};", arg_name, retrieval));
                } else {
                    setup_lines.push(format!("var {} = undefined;", arg_name));
                }
            } else if let Some(unser) = arg.get("unserializableValue").and_then(|v| v.as_str()) {
                setup_lines.push(format!("var {} = {};", arg_name, unser));
            } else {
                setup_lines.push(format!("var {} = undefined;", arg_name));
            }
            arg_names.push(arg_name);
        }

        (setup_lines.join("\n"), arg_names.join(", "))
    }

    fn v8_to_json(
        &mut self,
        result: deno_core::v8::Global<deno_core::v8::Value>,
    ) -> Result<serde_json::Value, String> {
        let scope = &mut self.runtime.handle_scope();
        let local = deno_core::v8::Local::new(scope, result);

        if local.is_undefined() || local.is_null() {
            return Ok(serde_json::Value::Null);
        }
        if local.is_boolean() {
            return Ok(serde_json::Value::Bool(local.boolean_value(scope)));
        }
        if local.is_number() {
            let n = local.number_value(scope).unwrap_or(0.0);
            return Ok(serde_json::json!(n));
        }
        if local.is_string() {
            let s = local.to_rust_string_lossy(scope);
            return Ok(serde_json::Value::String(s));
        }

        let global = scope.get_current_context().global(scope);
        let json_obj_str = deno_core::v8::String::new(scope, "JSON").unwrap();
        if let Some(json_obj) = global.get(scope, json_obj_str.into()) {
            if let Some(json_obj) = json_obj.to_object(scope) {
                let stringify_str = deno_core::v8::String::new(scope, "stringify").unwrap();
                if let Some(stringify_fn) = json_obj.get(scope, stringify_str.into()) {
                    if let Ok(stringify_fn) =
                        deno_core::v8::Local::<deno_core::v8::Function>::try_from(stringify_fn)
                    {
                        let args = [local];
                        if let Some(result) = stringify_fn.call(scope, json_obj.into(), &args) {
                            let json_str = result.to_rust_string_lossy(scope);
                            if let Ok(val) = serde_json::from_str(&json_str) {
                                return Ok(val);
                            }
                        }
                    }
                }
            }
        }

        let s = local.to_rust_string_lossy(scope);
        Ok(serde_json::Value::String(s))
    }

    fn info_from_json(value: &serde_json::Value) -> RemoteObjectInfo {
        match value {
            serde_json::Value::Null => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("null".into()),
                class_name: String::new(),
                description: "null".into(),
                object_id: None,
                value: Some(serde_json::Value::Null),
            },
            serde_json::Value::Bool(b) => RemoteObjectInfo {
                js_type: "boolean".into(),
                subtype: None,
                class_name: String::new(),
                description: b.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Number(n) => RemoteObjectInfo {
                js_type: "number".into(),
                subtype: None,
                class_name: String::new(),
                description: n.to_string(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::String(s) => RemoteObjectInfo {
                js_type: "string".into(),
                subtype: None,
                class_name: String::new(),
                description: s.clone(),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Array(arr) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: Some("array".into()),
                class_name: "Array".into(),
                description: format!("Array({})", arr.len()),
                object_id: None,
                value: Some(value.clone()),
            },
            serde_json::Value::Object(_) => RemoteObjectInfo {
                js_type: "object".into(),
                subtype: None,
                class_name: "Object".into(),
                description: "Object".into(),
                object_id: None,
                value: Some(value.clone()),
            },
        }
    }

    fn info_from_meta(meta: &serde_json::Value, object_id: Option<String>) -> RemoteObjectInfo {
        let js_type = meta
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("undefined")
            .to_string();
        let subtype = meta
            .get("subtype")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let class_name = meta
            .get("className")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let description = meta
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let value = if js_type != "object" && js_type != "function" {
            meta.get("description")
                .and_then(|v| v.as_str())
                .map(|s| serde_json::Value::String(s.to_string()))
        } else {
            None
        };

        RemoteObjectInfo {
            js_type,
            subtype,
            class_name,
            description,
            object_id,
            value,
        }
    }
}

impl Default for ObscuraJsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_dom::parse_html;

    fn setup_runtime(html: &str) -> ObscuraJsRuntime {
        let dom = parse_html(html);
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.com/test");
        rt.set_title("Test Page");
        rt.run_page_init();
        rt
    }

    #[test]
    fn function_to_string_has_native_function_shape() {
        let mut rt = setup_runtime("<html><body></body></html>");

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const fn = Function.prototype.toString;
                    let constructible = true;
                    try {
                        Reflect.construct(function () {}, [], fn);
                    } catch (error) {
                        constructible = false;
                    }
                    return {
                        source: fn.toString(),
                        name: fn.name,
                        length: fn.length,
                        hasOwnPrototype: Object.prototype.hasOwnProperty.call(fn, "prototype"),
                        constructible,
                    };
                })()"#,
            )
            .unwrap(),
            serde_json::json!({
                "source": "function toString() { [native code] }",
                "name": "toString",
                "length": 0,
                "hasOwnPrototype": false,
                "constructible": false,
            })
        );
    }

    #[test]
    fn iframe_content_window_exposes_realm_globals() {
        let mut rt = setup_runtime("<html><body></body></html>");

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const iframe = document.createElement("iframe");
                    document.body.appendChild(iframe);
                    const child = iframe.contentWindow;
                    const names = [
                        "Object", "Function", "Error", "Promise", "Proxy",
                        "XMLHttpRequest", "Worker", "Blob", "FormData",
                        "WebSocket", "MutationObserver",
                    ];
                    return {
                        types: names.map(name => typeof child[name]),
                        separate: [
                            child.Object !== Object,
                            child.Promise !== Promise,
                            child.XMLHttpRequest !== XMLHttpRequest,
                            child.Math !== Math,
                        ],
                        constructible: [
                            new child.Object() instanceof child.Object,
                            new child.Promise(resolve => resolve()) instanceof child.Promise,
                            new child.XMLHttpRequest() instanceof child.XMLHttpRequest,
                            new child.Blob([]) instanceof child.Blob,
                            new child.FormData() instanceof child.FormData,
                            new child.MutationObserver(() => {}) instanceof child.MutationObserver,
                        ],
                        utilities: [
                            child.Object.keys({ first: 1 })[0] === "first",
                            child.Array.isArray([]),
                            child.Promise.resolve(1) instanceof child.Promise,
                            child.Function("return 7")() === 7,
                            Object.getOwnPropertyNames(child).includes("XMLHttpRequest"),
                            child.globalThis === child,
                        ],
                    };
                })()"#,
            )
            .unwrap(),
            serde_json::json!({
                "types": vec!["function"; 11],
                "separate": vec![true; 4],
                "constructible": vec![true; 6],
                "utilities": vec![true; 6],
            })
        );
    }

    #[test]
    fn document_domain_getter_and_valid_relaxation_match_effective_host() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("https://deep.assets.example.co.uk:8443/page");
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const initial = document.domain;
                    document.domain = "ASSETS.EXAMPLE.CO.UK";
                    const first = document.domain;
                    document.domain = "example.co.uk";
                    return [initial, first, document.domain, location.hostname,
                            (new Document()).domain,
                            new DOMParser().parseFromString("", "text/html").domain];
                })()"#,
            )
            .unwrap(),
            serde_json::json!([
                "deep.assets.example.co.uk",
                "assets.example.co.uk",
                "example.co.uk",
                "deep.assets.example.co.uk",
                "example.co.uk",
                "example.co.uk"
            ])
        );
    }

    #[test]
    fn document_domain_rejects_unrelated_child_and_public_suffix_hosts() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("https://app.user.github.io/page");
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const attempts = ["", ".github.io", "github.io", "evilgithub.io",
                                      "other.github.io", "child.app.user.github.io"];
                    const rejected = attempts.map(value => {
                        try { document.domain = value; return "accepted"; }
                        catch (error) { return error.name; }
                    });
                    document.domain = "user.github.io";
                    return rejected.concat(document.domain);
                })()"#,
            )
            .unwrap(),
            serde_json::json!([
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "user.github.io"
            ])
        );
    }

    #[test]
    fn document_domain_detached_and_hostless_setters_throw_security_error() {
        let mut rt = setup_runtime("<html><body></body></html>");
        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const detached = [new Document(),
                        document.implementation.createHTMLDocument("x"),
                        document.implementation.createDocument(null, "root")];
                    const errors = detached.map(doc => {
                        try { doc.domain = "example.com"; return "accepted"; }
                        catch (error) { return error.name; }
                    });
                    return [typeof document.domain, document.domain].concat(errors);
                })()"#,
            )
            .unwrap(),
            serde_json::json!([
                "string",
                "example.com",
                "SecurityError",
                "SecurityError",
                "SecurityError"
            ])
        );

        let dom = parse_html("<html><body></body></html>");
        let mut hostless = ObscuraJsRuntime::new();
        hostless.set_dom(dom);
        hostless.set_url("about:blank");
        hostless.run_page_init();
        assert_eq!(
            hostless
                .evaluate(
                    r#"(() => {
                        let error = "";
                        try { document.domain = "example.com"; }
                        catch (caught) { error = caught.name; }
                        return [document.domain, error];
                    })()"#,
                )
                .unwrap(),
            serde_json::json!(["", "SecurityError"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn string_timeout_handler_executes_in_global_scope() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("var __timerValue='pending'; setTimeout('__timerValue=\"done\"', 0)")
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__timerValue").unwrap(),
            serde_json::json!("done")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn string_timeout_declarations_reach_global_scope() {
        // A string timer handler runs as a classic script in global scope, so a
        // top-level var/function declaration in it becomes a global. new Function()
        // kept those declarations local to the compiled function, so they never
        // reached globalThis.
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate(
            "setTimeout('var __leaked = 42; function __leakedFn(){ return 7; }', 0)",
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        let v = rt
            .evaluate(
                "String(globalThis.__leaked) + '|' + (typeof globalThis.__leakedFn === 'function' ? globalThis.__leakedFn() : 'missing')",
            )
            .unwrap();
        assert_eq!(v, serde_json::json!("42|7"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn string_interval_handler_repeats_and_can_clear_itself() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("globalThis.__ticks=0").unwrap();
        rt.evaluate(
            "globalThis.__timerId=setInterval('__ticks++;if(__ticks===2)clearInterval(__timerId)',1)",
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__ticks").unwrap(),
            serde_json::json!(2.0)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn zero_delay_timer_runs_as_a_task_after_microtasks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "zero-delay-task-order",
            r#"
                globalThis.__taskOrder = ["sync"];
                setTimeout(() => __taskOrder.push("timer"), 0);
                Promise.resolve().then(() => __taskOrder.push("microtask"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__taskOrder").unwrap(),
            serde_json::json!(["sync", "microtask", "timer"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scheduler_post_task_observes_priority_fifo_and_task_boundaries() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "scheduler-priority-order",
            r#"
                globalThis.__schedulerOrder = ["sync"];
                const schedule = (name, priority) => scheduler.postTask(() => {
                    __schedulerOrder.push(name);
                    Promise.resolve().then(() => __schedulerOrder.push(name + "-microtask"));
                    return name + "-result";
                }, { priority });
                globalThis.__schedulerResults = Promise.all([
                    schedule("background-1", "background"),
                    schedule("background-2", "background"),
                    schedule("visible", "user-visible"),
                    schedule("blocking-1", "user-blocking"),
                    schedule("blocking-2", "user-blocking"),
                ]).then(values => { globalThis.__schedulerValues = values; });
                Promise.resolve().then(() => __schedulerOrder.push("initial-microtask"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__schedulerOrder").unwrap(),
            serde_json::json!([
                "sync",
                "initial-microtask",
                "blocking-1",
                "blocking-1-microtask",
                "blocking-2",
                "blocking-2-microtask",
                "visible",
                "visible-microtask",
                "background-1",
                "background-1-microtask",
                "background-2",
                "background-2-microtask",
            ])
        );
        assert_eq!(
            rt.evaluate("__schedulerValues").unwrap(),
            serde_json::json!([
                "background-1-result",
                "background-2-result",
                "visible-result",
                "blocking-1-result",
                "blocking-2-result",
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scheduler_abort_delay_and_yield_follow_task_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "scheduler-abort-delay-yield",
            r#"
                globalThis.__schedulerState = {
                    order: [],
                    canceledCallbackRan: false,
                    exactAbortReason: false,
                    selfAbortCallbackRan: false,
                    exactSelfAbortReason: false,
                };
                const abortReason = { reason: "stop" };
                const canceled = new AbortController();
                scheduler.postTask(() => {
                    __schedulerState.canceledCallbackRan = true;
                }, { signal: canceled.signal, delay: 20 }).catch(error => {
                    __schedulerState.exactAbortReason = error === abortReason;
                });
                canceled.abort(abortReason);

                const selfAbortReason = { reason: "inside callback" };
                const selfCanceled = new AbortController();
                scheduler.postTask(() => {
                    __schedulerState.selfAbortCallbackRan = true;
                    selfCanceled.abort(selfAbortReason);
                    return "ignored result";
                }, { signal: selfCanceled.signal }).catch(error => {
                    __schedulerState.exactSelfAbortReason = error === selfAbortReason;
                });

                scheduler.postTask(async () => {
                    __schedulerState.order.push("blocking-start");
                    await scheduler.yield();
                    __schedulerState.order.push("blocking-continuation");
                }, { priority: "user-blocking" });
                scheduler.postTask(() => {
                    __schedulerState.order.push("background");
                }, { priority: "background" });
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                r#"[
                    __schedulerState.order,
                    __schedulerState.canceledCallbackRan,
                    __schedulerState.exactAbortReason,
                    __schedulerState.selfAbortCallbackRan,
                    __schedulerState.exactSelfAbortReason,
                    scheduler instanceof Scheduler,
                    Object.prototype.toString.call(scheduler),
                    Scheduler.prototype.postTask.length,
                    Scheduler.prototype.yield.length,
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                ["blocking-start", "blocking-continuation", "background"],
                false,
                true,
                true,
                true,
                true,
                "[object Scheduler]",
                1,
                0,
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn self_requeueing_message_channel_yields_to_timers() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-channel-task-yield",
            r#"
                globalThis.__messageCount = 0;
                globalThis.__timerObserved = false;
                const channel = new MessageChannel();
                channel.port2.onmessage = () => {
                    __messageCount++;
                    if (!__timerObserved) channel.port1.postMessage(null);
                };
                channel.port1.postMessage(null);
                setTimeout(() => { __timerObserved = true; }, 1);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        let result = rt.evaluate("[__messageCount, __timerObserved]").unwrap();
        let values = result.as_array().unwrap();
        assert!(
            values[0]
                .as_u64()
                .is_some_and(|count| count > 0 && count < 10_000),
            "message task did not yield: {result}"
        );
        assert_eq!(values[1], serde_json::json!(true));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_port_queues_until_start_and_clones_at_post_time() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-start-and-clone",
            r#"
                const channel = new MessageChannel();
                const payload = { nested: { value: 7 } };
                globalThis.__messagePortResult = {
                    portInstance: channel.port1 instanceof MessagePort,
                    channelInstance: channel instanceof MessageChannel,
                    deliveredBeforeStart: false,
                    delivered: false,
                };
                channel.port2.addEventListener("message", function(event) {
                    __messagePortResult.delivered = true;
                    __messagePortResult.value = event.data.nested.value;
                    __messagePortResult.targetIsPort = event.target === channel.port2;
                    __messagePortResult.thisIsPort = this === channel.port2;
                    __messagePortResult.origin = event.origin;
                    __messagePortResult.portCount = event.ports.length;
                });
                channel.port1.postMessage(payload);
                payload.nested.value = 99;
                setTimeout(() => {
                    __messagePortResult.deliveredBeforeStart = __messagePortResult.delivered;
                    channel.port2.start();
                }, 0);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__messagePortResult").unwrap(),
            serde_json::json!({
                "portInstance": true,
                "channelInstance": true,
                "deliveredBeforeStart": false,
                "delivered": true,
                "value": 7,
                "targetIsPort": true,
                "thisIsPort": true,
                "origin": "",
                "portCount": 0,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_port_onmessage_starts_and_yields_between_messages() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-task-boundaries",
            r#"
                globalThis.__messagePortOrder = [];
                const channel = new MessageChannel();
                channel.port1.postMessage(1);
                channel.port1.postMessage(2);
                channel.port2.onmessage = (event) => {
                    __messagePortOrder.push("message-" + event.data);
                    if (event.currentTarget !== channel.port2) __messagePortOrder.push("bad-current-target");
                    Promise.resolve().then(() => __messagePortOrder.push("microtask-" + event.data));
                };
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__messagePortOrder").unwrap(),
            serde_json::json!(["message-1", "microtask-1", "message-2", "microtask-2",])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_port_close_discards_delivery_already_queued_for_a_task() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-close-cancels-queued-delivery",
            r#"
                globalThis.__closedPortDeliveries = 0;
                const channel = new MessageChannel();
                channel.port2.onmessage = () => { __closedPortDeliveries++; };
                channel.port1.postMessage("queued");
                channel.port2.close();
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__closedPortDeliveries").unwrap(),
            serde_json::json!(0.0)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_port_handler_and_listener_follow_registration_order() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-mixed-registration-order",
            r#"
                globalThis.__messagePortRegistrationOrder = [];

                const handlerFirst = new MessageChannel();
                handlerFirst.port2.onmessage = () => __messagePortRegistrationOrder.push("handler-first:handler");
                handlerFirst.port2.addEventListener("message", () => __messagePortRegistrationOrder.push("handler-first:listener"));
                handlerFirst.port1.postMessage(null);

                const listenerFirst = new MessageChannel();
                listenerFirst.port2.addEventListener("message", () => __messagePortRegistrationOrder.push("listener-first:listener"));
                listenerFirst.port2.onmessage = () => __messagePortRegistrationOrder.push("listener-first:handler");
                listenerFirst.port1.postMessage(null);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__messagePortRegistrationOrder").unwrap(),
            serde_json::json!([
                "handler-first:handler",
                "handler-first:listener",
                "listener-first:listener",
                "listener-first:handler",
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_port_internal_state_is_hidden_and_ignores_own_property_tampering() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-hidden-state",
            r#"
                const channel = new MessageChannel();
                globalThis.__messagePortOwnKeys = Object.keys(channel.port2);
                globalThis.__messagePortOwnNames = Object.getOwnPropertyNames(channel.port2);
                globalThis.__messagePortTamperResult = [];
                channel.port2.onmessage = (event) => __messagePortTamperResult.push(event.data);

                // These names used to be the actual implementation state. An
                // expando with any of them must not alter delivery now.
                channel.port1._closed = true;
                channel.port1._entangled = null;
                channel.port2._closed = true;
                channel.port2._messageQueue = [];
                channel.port2._messageQueueEnabled = false;
                channel.port2._messageDeliveryPending = true;
                channel.port2._onmessage = null;
                channel.port2._scheduleMessageDelivery = () => {};
                channel.port2.dispatchEvent = () => { throw new Error("tampered dispatchEvent called"); };
                channel.port1.postMessage("delivered");
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[__messagePortOwnKeys, __messagePortOwnNames, __messagePortTamperResult]")
                .unwrap(),
            serde_json::json!([[], [], ["delivered"]])
        );
    }

    #[test]
    fn message_port_has_browser_shaped_construction_and_clone_errors() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    let constructorError = "";
                    let cloneError = "";
                    try { new MessagePort(); } catch (error) { constructorError = error.name; }
                    try { new MessageChannel().port1.postMessage(() => {}); }
                    catch (error) { cloneError = error.name; }
                    return [constructorError, cloneError, Object.prototype.toString.call(new MessageChannel().port1)];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["TypeError", "DataCloneError", "[object MessagePort]"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_channel_delivers_independent_post_time_clones_to_matching_peers() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "broadcast-channel-clone-delivery",
            r#"
                globalThis.__broadcastResults = { sender: 0, otherName: 0, peers: [] };
                const sender = new BroadcastChannel("session-sync");
                const first = new BroadcastChannel("session-sync");
                const second = new BroadcastChannel("session-sync");
                const other = new BroadcastChannel("other-name");
                sender.onmessage = () => { __broadcastResults.sender++; };
                other.onmessage = () => { __broadcastResults.otherName++; };
                first.onmessage = (event) => {
                    __broadcastResults.peers.push({
                        peer: "first",
                        value: event.data.nested.value,
                        bytes: Array.from(event.data.bytes),
                        source: event.source,
                        ports: event.ports.length,
                    });
                    event.data.nested.value = 500;
                    event.data.bytes[0] = 99;
                };
                second.onmessage = (event) => {
                    __broadcastResults.peers.push({
                        peer: "second",
                        value: event.data.nested.value,
                        bytes: Array.from(event.data.bytes),
                        source: event.source,
                        ports: event.ports.length,
                    });
                };
                const payload = { nested: { value: 7 }, bytes: new Uint8Array([1, 2, 3]) };
                sender.postMessage(payload);
                payload.nested.value = 42;
                payload.bytes[0] = 88;
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__broadcastResults").unwrap(),
            serde_json::json!({
                "sender": 0,
                "otherName": 0,
                "peers": [
                    { "peer": "first", "value": 7, "bytes": [1, 2, 3], "source": null, "ports": 0 },
                    { "peer": "second", "value": 7, "bytes": [1, 2, 3], "source": null, "ports": 0 },
                ],
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_channel_handlers_follow_registration_order_and_task_timing() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "broadcast-channel-registration-order",
            r#"
                globalThis.__broadcastOrder = ["sync"];
                const sender = new BroadcastChannel("ordering");
                const handlerFirst = new BroadcastChannel("ordering");
                const listenerFirst = new BroadcastChannel("ordering");
                handlerFirst.onmessage = () => __broadcastOrder.push("handler-first:handler");
                handlerFirst.addEventListener("message", () => __broadcastOrder.push("handler-first:listener"));
                listenerFirst.addEventListener("message", () => __broadcastOrder.push("listener-first:listener"));
                listenerFirst.onmessage = () => __broadcastOrder.push("listener-first:handler");
                sender.postMessage(null);
                Promise.resolve().then(() => __broadcastOrder.push("microtask"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__broadcastOrder").unwrap(),
            serde_json::json!([
                "sync",
                "microtask",
                "handler-first:handler",
                "handler-first:listener",
                "listener-first:listener",
                "listener-first:handler",
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_channel_close_cancels_delivery_and_closed_post_throws() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "broadcast-channel-close",
            r#"
                globalThis.__broadcastCloseResult = { deliveries: 0 };
                const sender = new BroadcastChannel("close-test");
                const recipient = new BroadcastChannel("close-test");
                recipient.onmessage = () => { __broadcastCloseResult.deliveries++; };
                sender.postMessage("queued");
                recipient.close();
                sender.close();
                try { sender.postMessage("closed"); }
                catch (error) { __broadcastCloseResult.closedError = error.name; }
                try { new BroadcastChannel(); }
                catch (error) { __broadcastCloseResult.constructorError = error.name; }
                try { new BroadcastChannel("no-peers").postMessage(() => {}); }
                catch (error) { __broadcastCloseResult.cloneError = error.name; }
                __broadcastCloseResult.ownKeys = Object.keys(new BroadcastChannel("shape"));
                __broadcastCloseResult.tag = Object.prototype.toString.call(new BroadcastChannel("shape"));
                __broadcastCloseResult.eventTarget = new BroadcastChannel("shape") instanceof EventTarget;
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__broadcastCloseResult").unwrap(),
            serde_json::json!({
                "deliveries": 0,
                "closedError": "InvalidStateError",
                "constructorError": "TypeError",
                "cloneError": "DataCloneError",
                "ownKeys": [],
                "tag": "[object BroadcastChannel]",
                "eventTarget": true,
            })
        );
    }

    #[test]
    fn performance_now_is_monotonic_under_bursty_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        // Hammer performance.now() so many calls land in the same millisecond and
        // the wall clock rolls over repeatedly; the value must never go backwards.
        let violations = rt
            .evaluate(
                "(function(){var prev=-Infinity, bad=0; for(var i=0;i<500000;i++){var t=performance.now(); if(t<prev) bad++; prev=t;} return bad;})()",
            )
            .unwrap();
        assert_eq!(
            violations.as_f64(),
            Some(0.0),
            "performance.now() went backwards"
        );
    }

    #[test]
    fn performance_now_does_not_outrun_elapsed_time() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let lead = rt
            .evaluate(
                "(function(){for(var i=0;i<500000;i++)performance.now(); return performance.now()-(Date.now()-performance.timeOrigin);})()",
            )
            .unwrap();
        assert!(
            lead.as_f64().unwrap() <= 1.0,
            "performance.now() advanced ahead of elapsed time: {lead}"
        );
    }

    #[test]
    fn time_origin_never_lands_in_the_future() {
        // __obscura_init deletes itself, so a realm yields one draw of the
        // origin jitter. Build a fresh runtime per draw, do not hoist this out.
        for _ in 0..40 {
            let mut rt = setup_runtime("<html><body></body></html>");
            let skew = rt
                .evaluate("performance.timeOrigin - Date.now()")
                .unwrap()
                .as_f64()
                .unwrap();
            assert!(
                skew <= 0.0,
                "performance.timeOrigin is {skew} ms ahead of Date.now()"
            );
        }
    }

    #[test]
    fn childnode_helpers_coerce_non_string_primitives_to_text() {
        let mut rt =
            setup_runtime(r#"<html><body><div id="p"><span id="t">x</span></div></body></html>"#);
        let before = rt
            .evaluate("(function(){var t=document.getElementById('t'); t.before(5); return t.previousSibling ? t.previousSibling.textContent : 'NULL';})()")
            .unwrap();
        assert_eq!(before, serde_json::json!("5"));
        let after = rt
            .evaluate("(function(){var t=document.getElementById('t'); t.after(true); return t.nextSibling ? t.nextSibling.textContent : 'NULL';})()")
            .unwrap();
        assert_eq!(after, serde_json::json!("true"));
        let replaced = rt
            .evaluate("(function(){var t=document.getElementById('t'); t.replaceWith(42); return document.getElementById('p').textContent;})()")
            .unwrap();
        assert!(
            replaced.as_str().unwrap().contains("42"),
            "replaceWith(42) should leave text '42': {replaced}"
        );
    }

    #[test]
    fn replace_state_without_url_preserves_current_location() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let path = rt
            .evaluate(
                "(function(){history.pushState({}, '', '/dashboard'); history.replaceState({scroll:1}); return location.pathname;})()",
            )
            .unwrap();
        assert_eq!(path, serde_json::json!("/dashboard"));
    }

    #[test]
    fn push_state_without_url_preserves_current_location() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let path = rt
            .evaluate(
                "(function(){history.pushState({}, '', '/a'); history.pushState({b:1}); return location.pathname;})()",
            )
            .unwrap();
        assert_eq!(path, serde_json::json!("/a"));
    }

    #[test]
    fn history_exposes_the_web_platform_constructor_and_prototype() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(function(){
                    const original = history.replaceState;
                    History.prototype.replaceState.call(history, {ok:true}, "", "/prototype");
                    let illegal = false;
                    try { new History(); } catch (error) { illegal = error instanceof TypeError; }
                    return {
                        instance: history instanceof History,
                        prototype: Object.getPrototypeOf(history) === History.prototype,
                        method: original === History.prototype.replaceState,
                        tag: Object.prototype.toString.call(history),
                        path: location.pathname,
                        illegal,
                    };
                })()"#,
            )
            .unwrap();
        assert_eq!(result["instance"], serde_json::json!(true));
        assert_eq!(result["prototype"], serde_json::json!(true));
        assert_eq!(result["method"], serde_json::json!(true));
        assert_eq!(result["tag"], serde_json::json!("[object History]"));
        assert_eq!(result["path"], serde_json::json!("/prototype"));
        assert_eq!(result["illegal"], serde_json::json!(true));
    }

    #[test]
    fn style_attribute_parses_into_style_object() {
        // Inline styles present in the parsed HTML must be visible via el.style.*
        let mut rt = setup_runtime(
            r#"<html><body><div id="d" style="color: red; display: none">hi</div></body></html>"#,
        );
        assert_eq!(
            rt.evaluate("document.getElementById('d').style.color")
                .unwrap(),
            serde_json::json!("red")
        );
        assert_eq!(
            rt.evaluate("document.getElementById('d').style.display")
                .unwrap(),
            serde_json::json!("none")
        );
    }

    #[test]
    fn set_style_attribute_updates_style_object() {
        let mut rt = setup_runtime(r#"<html><body><div id="d">hi</div></body></html>"#);
        let margin = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.setAttribute('style','margin: 5px'); return e.style.margin;})()",
            )
            .unwrap();
        assert_eq!(margin, serde_json::json!("5px"));
    }

    #[test]
    fn null_namespace_style_attribute_stays_in_sync() {
        let mut rt = setup_runtime(r#"<html><body><div id="d">hi</div></body></html>"#);
        let result = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.setAttributeNS(null,'style','color: green'); var before=e.style.color; e.removeAttributeNS(null,'style'); return before+'|'+e.style.color+'|'+String(e.getAttribute('style'));})()",
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("green||null"));
    }

    #[test]
    fn setting_style_property_updates_the_attribute_and_serialization() {
        let mut rt = setup_runtime(r#"<html><body><div id="d">hi</div></body></html>"#);
        let attr = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.style.color='blue'; return e.getAttribute('style');})()",
            )
            .unwrap();
        assert_eq!(attr, serde_json::json!("color: blue;"));
        let html = rt
            .evaluate("document.getElementById('d').outerHTML")
            .unwrap();
        assert!(
            html.as_str().unwrap().contains("color: blue"),
            "outerHTML should carry the style set via el.style: {html}"
        );
    }

    #[test]
    fn style_object_reflects_external_attribute_change() {
        // A later setAttribute('style', …) must supersede an earlier value read
        // through el.style (the declaration re-syncs from the attribute).
        let mut rt =
            setup_runtime(r#"<html><body><div id="d" style="color: red">hi</div></body></html>"#);
        let color = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.style.color; e.setAttribute('style','color: green'); return e.style.color;})()",
            )
            .unwrap();
        assert_eq!(color, serde_json::json!("green"));
    }

    #[test]
    fn clone_node_deep_preserves_context_sensitive_elements() {
        // A <tr> is not a valid child of <div>, so cloning through a throwaway
        // <div>.innerHTML dropped it and returned null. A structural clone keeps it.
        let mut rt = setup_runtime("<html><body></body></html>");
        let tag = rt
            .evaluate("(document.createElement('tr').cloneNode(true) || {}).tagName || 'NULL'")
            .unwrap();
        assert_eq!(tag, serde_json::json!("TR"));
        let td = rt
            .evaluate("(document.createElement('td').cloneNode(true) || {}).tagName || 'NULL'")
            .unwrap();
        assert_eq!(td, serde_json::json!("TD"));
    }

    #[test]
    fn clone_node_deep_copies_children_and_attributes() {
        let mut rt = setup_runtime(r#"<html><body><ul id="l"><li class="a">one</li><li class="b">two</li></ul></body></html>"#);
        let out = rt
            .evaluate(
                "(function(){var c=document.getElementById('l').cloneNode(true); return c.children.length + '|' + c.children[0].className + '|' + c.children[1].textContent;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("2|a|two"));
    }

    #[test]
    fn clone_node_deep_preserves_table_rows() {
        let mut rt = setup_runtime(
            r#"<html><body><table id="t"><tbody><tr><td>1</td><td>2</td></tr></tbody></table></body></html>"#,
        );
        // Navigate the detached clone directly (querySelector does not traverse
        // detached subtrees). tbody > tr > (td, td).
        let out = rt
            .evaluate(
                "(function(){var tb=document.querySelector('#t tbody').cloneNode(true); var tr=tb.children[0]; return tr.tagName + '|' + tr.children.length + '|' + tr.children[1].textContent;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("TR|2|2"));
    }

    #[test]
    fn clone_node_shallow_copies_attributes_without_children() {
        let mut rt = setup_runtime(r#"<html><body><div id="d" data-x="7"><span>kid</span></div></body></html>"#);
        let out = rt
            .evaluate(
                "(function(){var c=document.getElementById('d').cloneNode(false); return c.getAttribute('data-x') + '|' + c.childNodes.length;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("7|0"));
    }

    #[test]
    fn clone_node_copies_js_assigned_inline_styles() {
        let mut rt = setup_runtime("<html><body><div id='d'></div></body></html>");
        let out = rt
            .evaluate(
                "(function(){var d=document.getElementById('d');d.style.color='red';d.style.fontSize='12px';var c=d.cloneNode(false);return c.style.color+'|'+c.style.fontSize+'|'+c.style.cssText;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("red|12px|color: red; font-size: 12px;"));
    }

    #[test]
    fn clone_node_deep_copies_template_content() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let out = rt
            .evaluate(
                "(function(){var t=document.createElement('template');t.content.appendChild(document.createElement('option')).textContent='choice';var c=t.cloneNode(true);return c.content.childNodes.length+'|'+c.content.firstChild.tagName+'|'+c.content.firstChild.textContent;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("1|OPTION|choice"));
    }

    #[test]
    fn insert_adjacent_html_parses_table_fragments() {
        let mut rt = setup_runtime(
            r#"<html><body><table id="t"><tbody id="tb"></tbody></table></body></html>"#,
        );
        let out = rt
            .evaluate("(function(){var tb=document.getElementById('tb'); tb.insertAdjacentHTML('beforeend','<tr><td>1</td><td>2</td></tr>'); var tr=tb.firstElementChild; return tr ? (tr.tagName+':'+tr.children.length) : 'NULL';})()")
            .unwrap();
        assert_eq!(out, serde_json::json!("TR:2"));
    }

    #[test]
    fn insert_adjacent_html_position_is_case_insensitive() {
        let mut rt = setup_runtime(r#"<html><body><div id="host"><span>base</span></div></body></html>"#);
        let out = rt
            .evaluate("(function(){var h=document.getElementById('host'); h.insertAdjacentHTML('BeforeEnd','<b>x</b>'); return h.lastElementChild ? h.lastElementChild.tagName : 'NULL';})()")
            .unwrap();
        assert_eq!(out, serde_json::json!("B"));
    }

    #[test]
    fn insert_adjacent_html_rejects_invalid_position() {
        let mut rt = setup_runtime(r#"<html><body><div id="host"></div></body></html>"#);
        let out = rt
            .evaluate("(function(){var h=document.getElementById('host'); try { h.insertAdjacentHTML('nope','<b>x</b>'); return 'no-throw'; } catch(e){ return e.name; }})()")
            .unwrap();
        assert_eq!(out, serde_json::json!("SyntaxError"));
    }

    #[test]
    fn insert_adjacent_html_keeps_leading_comments_in_table_contexts() {
        let mut rt = setup_runtime(
            r#"<html><body><table><tbody id="tb"><tr id="row"></tr></tbody></table></body></html>"#,
        );
        let out = rt
            .evaluate(
                "(function(){var tb=document.getElementById('tb');tb.insertAdjacentHTML('beforeend','<!--m--><tr><td>v</td></tr>');var row=document.getElementById('row');row.insertAdjacentHTML('beforeend','<!--n--><td>x</td>');return Array.from(tb.childNodes).map(function(n){return n.nodeName}).join('|')+';'+Array.from(row.childNodes).map(function(n){return n.nodeName}).join('|');})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("TR|#comment|TR;#comment|TD"));
    }

    #[test]
    fn insert_adjacent_html_uses_the_insertion_element_as_context() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="d"></div><table id="table"><tbody id="tb"></tbody></table></body></html>"#,
        );
        let out = rt
            .evaluate(
                "(function(){var d=document.getElementById('d');d.insertAdjacentHTML('beforeend','<tr><td>v</td></tr>');var table=document.getElementById('table');table.insertAdjacentHTML('beforeend','<tr><td>x</td></tr>');var tb=document.getElementById('tb');tb.insertAdjacentHTML('beforeend','<tr><td>y</td></tr>tail');return d.firstChild.nodeName+':'+d.textContent+';'+table.lastElementChild.tagName+';'+Array.from(tb.childNodes).map(function(n){return n.nodeName+(n.data?':'+n.data:'')}).join('|');})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("#text:v;TBODY;TR|#text:tail"));
    }

    #[test]
    fn set_attribute_ns_is_retrievable_by_namespace_and_local_name() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var s=document.createElementNS('http://www.w3.org/2000/svg','svg'); s.setAttributeNS('http://www.w3.org/1999/xlink','xlink:href','#g'); return s.getAttributeNS('http://www.w3.org/1999/xlink','href');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#g"));
    }

    #[test]
    fn remove_attribute_ns_removes_by_namespace() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var s=document.createElementNS('http://www.w3.org/2000/svg','svg'); s.setAttributeNS('http://www.w3.org/1999/xlink','xlink:href','#g'); s.removeAttributeNS('http://www.w3.org/1999/xlink','href'); return s.getAttributeNS('http://www.w3.org/1999/xlink','href');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!(null));
    }

    #[test]
    fn get_attribute_ns_reads_plain_attributes_with_null_namespace() {
        // Backward-compat: getAttributeNS(null, name) still reads a plain attr.
        let mut rt = setup_runtime(r#"<html><body><div id="d" title="hi"></div></body></html>"#);
        let v = rt
            .evaluate("document.getElementById('d').getAttributeNS(null,'title')")
            .unwrap();
        assert_eq!(v, serde_json::json!("hi"));
    }

    #[test]
    fn namespaced_attribute_keeps_its_qualified_name() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var s=document.createElementNS('http://www.w3.org/2000/svg','svg');s.setAttributeNS('http://www.w3.org/1999/xlink','xlink:href','#g');return s.getAttribute('xlink:href')+'|'+s.getAttributeNames()[0]+'|'+s.outerHTML;})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#g|xlink:href|<svg xlink:href=\"#g\"></svg>"));
    }

    #[test]
    fn parsed_xlink_attribute_is_available_through_both_apis() {
        let mut rt = setup_runtime(
            r##"<html><body><svg><use id="u" xlink:href="#icon"></use></svg></body></html>"##,
        );
        let v = rt
            .evaluate("(function(){var u=document.getElementById('u');return u.getAttribute('xlink:href')+'|'+u.getAttributeNS('http://www.w3.org/1999/xlink','href')+'|'+u.getAttributeNames().join(',');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#icon|#icon|id,xlink:href"));
    }

    #[test]
    fn set_attribute_updates_a_parsed_namespaced_attribute_in_place() {
        // setAttribute matched the stored attribute by local name only, so a
        // parsed `xlink:href` (prefix=xlink, local=href) was never found by the
        // qualified name "xlink:href": the update was pushed as a *second*
        // attribute, getAttribute kept returning the stale original, and the
        // element serialized `xlink:href` twice.
        let mut rt = setup_runtime(
            r##"<html><body><svg><use id="u" xlink:href="#a"></use></svg></body></html>"##,
        );
        let v = rt
            .evaluate("(function(){var u=document.getElementById('u');u.setAttribute('xlink:href','#b');var dup=(u.outerHTML.match(/xlink:href/g)||[]).length;return u.getAttribute('xlink:href')+'|'+u.getAttributeNS('http://www.w3.org/1999/xlink','href')+'|'+u.getAttributeNames().join(',')+'|'+dup;})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#b|#b|id,xlink:href|1"));
    }

    #[test]
    fn set_attribute_ns_validates_namespace_constraints() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var e=document.createElement('div'),out=[];for(const args of [[null,'x:y'],['urn:test','a:b:c'],['urn:test','xml:lang'],['urn:test','xmlns:x']]){try{e.setAttributeNS(args[0],args[1],'v');out.push('none')}catch(err){out.push(err.name)}}return out.join('|');})()")
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!(
                "NamespaceError|InvalidCharacterError|NamespaceError|NamespaceError"
            )
        );
    }

    #[test]
    fn dom_parser_flags_malformed_xml_with_parsererror() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let has_err = rt
            .evaluate("(function(){var d=new DOMParser().parseFromString('<a><b></a>','application/xml'); return d.querySelector('parsererror') ? true : false;})()")
            .unwrap();
        assert_eq!(has_err, serde_json::json!(true));
    }

    #[test]
    fn dom_parser_accepts_well_formed_xml() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let ok = rt
            .evaluate("(function(){var d=new DOMParser().parseFromString('<root><child>x</child></root>','application/xml'); return d.querySelector('parsererror') ? 'ERR' : 'OK';})()")
            .unwrap();
        assert_eq!(ok, serde_json::json!("OK"));
    }

    #[test]
    fn dom_parser_html_never_gets_parsererror() {
        // HTML parsing is tolerant and must never synthesize a parsererror.
        let mut rt = setup_runtime("<html><body></body></html>");
        let ok = rt
            .evaluate("(function(){var d=new DOMParser().parseFromString('<div><p>hi</a>','text/html'); return d.querySelector('parsererror') ? 'ERR' : 'OK';})()")
            .unwrap();
        assert_eq!(ok, serde_json::json!("OK"));
    }

    #[test]
    fn custom_element_upgrade_runs_class_constructor_on_existing_element() {
        let mut rt = setup_runtime(
            r#"<html><body><svelte-like id="component"></svelte-like></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const before = document.getElementById("component");
                class SvelteLike extends HTMLElement {
                    constructor() {
                        super();
                        this.$$s = [];
                        this.attachShadow({ mode: "open" });
                    }
                    connectedCallback() {
                        for (const subscription of this.$$s) subscription();
                        this.$$s.push(() => {});
                        this.shadowRoot.textContent = "ready";
                    }
                }
                customElements.define("svelte-like", SvelteLike);
                return [
                    document.getElementById("component") === before,
                    before instanceof SvelteLike,
                    before.constructor === SvelteLike,
                    before.$$s.length,
                    before.shadowRoot && before.shadowRoot.textContent
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true, true, 1, "ready"]));
    }

    #[test]
    fn shadow_root_children_expose_parent_siblings_and_composed_root() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const host = document.createElement("lit-host");
                document.body.appendChild(host);
                const root = host.attachShadow({ mode: "open" });
                const start = document.createComment("start");
                const end = document.createComment("end");
                root.appendChild(start);
                root.appendChild(end);

                const text = document.createTextNode("rendered");
                start.parentNode.insertBefore(text, end);
                const inserted = [
                    start.parentNode === root,
                    start.nextSibling === text,
                    text.previousSibling === start,
                    text.nextSibling === end,
                    end.previousSibling === text,
                    root.contains(text),
                    text.getRootNode() === root,
                    text.getRootNode({ composed: true }) === document,
                    root.getRootNode({ composed: true }) === document,
                    root.isConnected,
                    text.isConnected,
                    root.textContent
                ];

                root.removeChild(text);
                const removed = [
                    text.parentNode === null,
                    start.nextSibling === end,
                    end.previousSibling === start
                ];

                document.body.appendChild(start);
                const moved = [
                    start.parentNode === document.body,
                    start.getRootNode() === document,
                    root.firstChild === end
                ];

                root.innerHTML = "<span id='inside'>inside</span>";
                const inside = root.firstChild;
                const parsed = [
                    inside.parentNode === root,
                    inside.getRootNode() === root,
                    root.textContent,
                    inside.matches("span#inside"),
                    root.querySelector("span#inside") === inside,
                    root.querySelectorAll("span#inside").length === 1
                ];

                const a = document.createElement("a");
                const b = document.createElement("b");
                const c = document.createElement("i");
                root.replaceChildren(a, b, c);
                root.insertBefore(a, c);
                const movedWithin = Array.from(root.children, el => el.localName);

                const fragment = document.createDocumentFragment();
                const x = document.createElement("x-one");
                const y = document.createElement("x-two");
                fragment.append(x, y);
                root.insertBefore(fragment, c);
                const flattened = [
                    Array.from(root.children, el => el.localName),
                    fragment.childNodes.length,
                    x.parentNode === root,
                    y.parentNode === root
                ];

                root.replaceChild(b, c);
                const replaced = [
                    Array.from(root.children, el => el.localName),
                    c.parentNode === null,
                    b.parentNode === root
                ];

                const detached = document.createElement("detached-node");
                const errors = [];
                for (const operation of [
                    () => root.insertBefore(detached, c),
                    () => root.removeChild(c),
                    () => root.replaceChild(detached, c),
                    () => root.appendChild(root),
                    () => root.appendChild(host)
                ]) {
                    try {
                        operation();
                        errors.push("none");
                    } catch (error) {
                        errors.push(error.name);
                    }
                }
                return [inserted, removed, moved, parsed, movedWithin, flattened, replaced, errors];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, true, true, true, true, true, true, true, true, true, true, "rendered"],
                [true, true, true],
                [true, true, true],
                [true, true, "inside", true, true, true],
                ["b", "a", "i"],
                [["b", "a", "x-one", "x-two", "i"], 0, true, true],
                [["a", "x-one", "x-two", "b"], true, true],
                [
                    "NotFoundError",
                    "NotFoundError",
                    "NotFoundError",
                    "HierarchyRequestError",
                    "HierarchyRequestError"
                ]
            ])
        );
    }

    #[test]
    fn shadow_root_identity_and_children_are_native_tree_backed() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r##"
                const host = document.createElement("native-shadow-host");
                document.body.appendChild(host);
                const root = host.attachShadow({ mode: "open", delegatesFocus: true });
                root.innerHTML = "<section id='inside'><span>native</span></section>";
                const inside = root.querySelector("#inside");
                const records = [];
                const observer = new MutationObserver(batch => records.push(...batch));
                observer.observe(root, { childList: true, subtree: true });
                const added = document.createElement("strong");
                inside.appendChild(added);
                records.push(...observer.takeRecords());

                host._shadowRoot = { mode: "closed" };
                let duplicateError = "none";
                try { host.attachShadow({ mode: "open" }); }
                catch (error) { duplicateError = error.name; }

                class ClosedShadowHost extends HTMLElement {
                    constructor() {
                        super();
                        this.closedRoot = this.attachShadow({ mode: "closed" });
                        this.internals = this.attachInternals();
                    }
                }
                customElements.define("closed-shadow-host", ClosedShadowHost);
                const closedHost = document.createElement("closed-shadow-host");

                return [
                    root instanceof ShadowRoot,
                    root.nodeType,
                    root.nodeName,
                    root.host === host,
                    root.mode,
                    root.delegatesFocus,
                    host.shadowRoot === root,
                    duplicateError,
                    inside.parentNode === root,
                    inside.getRootNode() === root,
                    inside.getRootNode({ composed: true }) === document,
                    root.isConnected,
                    inside.isConnected,
                    host.contains(inside),
                    document.querySelector("#inside") === null,
                    root.querySelector("#inside") === inside,
                    records.length,
                    records[0] && records[0].target === inside,
                    records[0] && records[0].addedNodes[0] === added,
                    closedHost.shadowRoot,
                    closedHost.internals.shadowRoot === closedHost.closedRoot
                ];
                "##,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                11,
                "#document-fragment",
                true,
                "open",
                true,
                true,
                "NotSupportedError",
                true,
                true,
                true,
                true,
                true,
                false,
                true,
                true,
                1,
                true,
                true,
                null,
                true
            ])
        );
    }

    #[test]
    fn create_element_synchronously_constructs_an_existing_definition() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const testStart = true;
                class CreatedLater extends HTMLElement {
                    constructor() {
                        super();
                        this.constructorState = ["initialized"];
                        this.attachShadow({ mode: "open" });
                        this.shadowRoot.textContent = "constructed";
                    }
                    connectedCallback() {
                        this.constructorState.push("connected");
                    }
                }
                customElements.define("created-later", CreatedLater);
                const element = document.createElement("created-later");
                const foreign = document.createElementNS(
                    "http://www.w3.org/2000/svg", "created-later"
                );
                return [
                    element instanceof CreatedLater,
                    element.constructor === CreatedLater,
                    element.localName,
                    element.constructorState,
                    element.shadowRoot && element.shadowRoot.textContent,
                    element.isConnected,
                    foreign instanceof CreatedLater
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                true,
                "created-later",
                ["initialized"],
                "constructed",
                false,
                false
            ])
        );
    }

    #[test]
    fn created_foreign_element_keeps_native_qualified_name_through_clone() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){const ns='http://www.w3.org/2000/svg';const el=document.createElementNS(ns,'linearGradient');const clone=el.cloneNode(true);return [el.namespaceURI,el.localName,el.tagName,el.nodeName,clone.namespaceURI,clone.localName,clone.outerHTML].join('|');})()",
            )
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!(
                "http://www.w3.org/2000/svg|linearGradient|linearGradient|linearGradient|http://www.w3.org/2000/svg|linearGradient|<linearGradient></linearGradient>"
            )
        );
    }

    #[test]
    fn svg_path_uses_the_standard_interface_chain() {
        let mut rt = setup_runtime(
            r#"<html><body><svg><path id="shape" d="M0 0L1 1"></path></svg></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const parsed = document.getElementById("shape");
                SVGPathElement.prototype.polyfillProbe = () => "path";
                const created = document.createElementNS(
                    "http://www.w3.org/2000/svg", "path"
                );
                const div = document.createElement("div");
                return [
                    parsed.constructor.name,
                    parsed instanceof SVGPathElement,
                    parsed instanceof SVGGeometryElement,
                    parsed instanceof SVGGraphicsElement,
                    parsed instanceof SVGElement,
                    parsed instanceof Element,
                    created instanceof SVGPathElement,
                    Object.getPrototypeOf(SVGPathElement.prototype) === SVGGeometryElement.prototype,
                    Object.getPrototypeOf(SVGGeometryElement.prototype) === SVGGraphicsElement.prototype,
                    Object.getPrototypeOf(SVGGraphicsElement.prototype) === SVGElement.prototype,
                    Object.getPrototypeOf(SVGElement.prototype) === Element.prototype,
                    parsed.polyfillProbe(),
                    typeof div.polyfillProbe
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "SVGPathElement",
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                "path",
                "undefined"
            ])
        );
    }

    #[test]
    fn foreign_inner_html_and_contextual_fragments_keep_svg_namespace() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){const ns='http://www.w3.org/2000/svg';const svg=document.createElementNS(ns,'svg');svg.innerHTML='<linearGradient id=paint></linearGradient>';const range=document.createRange();range.selectNodeContents(svg);const fragment=range.createContextualFragment('<circle></circle>');const circle=fragment.firstElementChild;return [svg.firstElementChild.namespaceURI,svg.firstElementChild.localName,circle.namespaceURI,circle.localName].join('|');})()",
            )
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!(
                "http://www.w3.org/2000/svg|linearGradient|http://www.w3.org/2000/svg|circle"
            )
        );
    }

    #[test]
    fn throwing_custom_element_constructor_marks_upgrade_failed_without_connecting() {
        let mut rt = setup_runtime(
            r#"<html><body><throws-during-upgrade id="target"></throws-during-upgrade></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                let constructorCalls = 0;
                let connectedCalls = 0;
                class ThrowsDuringUpgrade extends HTMLElement {
                    constructor() {
                        super();
                        constructorCalls++;
                        throw new Error("expected constructor failure");
                    }
                    connectedCallback() {
                        connectedCalls++;
                    }
                }
                customElements.define("throws-during-upgrade", ThrowsDuringUpgrade);
                const element = document.getElementById("target");
                customElements.upgrade(document);
                return [
                    constructorCalls,
                    connectedCalls,
                    element.__customUpgradeFailed === true
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([1, 0, true]));
    }

    #[test]
    fn test_document_title() {
        let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
        let title = rt.evaluate("document.title").unwrap();
        assert_eq!(title, serde_json::json!("Test"));

        let result = rt
            .evaluate(
                r#"
                (function() {
                  document.title = "A <new> title";
                  return [
                    document.title,
                    document.querySelector("head > title").textContent,
                    document.querySelectorAll("title").length
                  ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["A <new> title", "A <new> title", 1])
        );

        let normalized = rt
            .evaluate(
                r#"
                (function() {
                  document.querySelector("title").textContent = "  live\n\tDOM   title  ";
                  return document.title;
                })()
                "#,
            )
            .unwrap();
        assert_eq!(normalized, serde_json::json!("live DOM title"));
    }

    #[test]
    fn document_title_setter_creates_missing_title_element() {
        let mut rt = setup_runtime("<html><body><main>content</main></body></html>");
        let result = rt
            .evaluate(
                r#"
                (function() {
                  document.title = "Created";
                  return [
                    document.title,
                    document.head.tagName,
                    document.head.firstElementChild.tagName,
                    document.head.firstElementChild.textContent,
                    document.documentElement.firstElementChild === document.head
                  ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["Created", "HEAD", "TITLE", "Created", true])
        );

        let detached = rt
            .evaluate(
                r#"
                (function() {
                  const doc = document.implementation.createHTMLDocument();
                  doc.title = "  Detached   title  ";
                  return [doc.title, doc.querySelector("title").textContent, doc.referrer];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            detached,
            serde_json::json!(["Detached title", "  Detached   title  ", ""])
        );
    }

    #[test]
    fn document_referrer_has_explicit_navigation_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        assert_eq!(
            rt.evaluate("document.referrer").unwrap(),
            serde_json::json!("")
        );

        rt.set_referrer("https://source.example/path?q=1");
        assert_eq!(
            rt.evaluate("document.referrer").unwrap(),
            serde_json::json!("https://source.example/path?q=1")
        );
    }

    #[test]
    fn global_window_has_browser_constructor_identity() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                "return [window === self, self.constructor === Window,\
                         window instanceof Window, self.document === document,\
                         self.location === location, self.history === history,\
                         self.navigator === navigator];",
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, true, true, true, true, true, true])
        );
    }

    #[test]
    fn window_named_access_exposes_ids_and_eligible_names() {
        let mut rt = setup_runtime(
            r#"<html><body>
                <script id="payload" type="application/json">{"ready":true}</script>
                <div id="duplicate"></div><span id="duplicate"></span>
                <form name="login"></form><img name="hero">
                <div name="not-exposed"></div>
            </body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                return [
                    window.payload === document.getElementById("payload"),
                    window.payload.text,
                    window.duplicate instanceof HTMLCollection,
                    window.duplicate.length,
                    window.login === document.querySelector("form"),
                    window.hero === document.querySelector("img"),
                    typeof window["not-exposed"]
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                "{\"ready\":true}",
                true,
                2,
                true,
                true,
                "undefined"
            ])
        );
    }

    #[test]
    fn window_named_access_tracks_dynamic_ids_and_fragment_parsing() {
        let mut rt = setup_runtime("<html><body><div id='host'></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                const made = document.createElement("section");
                made.id = "dynamicName";
                const detachedIdAbsent = !("dynamicName" in window);
                document.body.appendChild(made);
                const first = window.dynamicName === made;
                made.id = "renamedDynamic";
                const renamed = !("dynamicName" in window)
                    && window.renamedDynamic === made;
                document.body.removeChild(made);
                const removed = !("renamedDynamic" in window);
                document.body.appendChild(made);
                const reattached = window.renamedDynamic === made;
                document.getElementById("host").innerHTML =
                    "<script id='parsedName'>payload</script>";
                const parsed = window.parsedName === document.getElementById("parsedName")
                    && window.parsedName.text === "payload";
                document.getElementById("host").innerHTML = "";
                const subtree = document.createElement("div");
                subtree.innerHTML = "<svg><path id='nestedSvg' name='svgName'></path></svg>";
                const detachedNestedAbsent = !("nestedSvg" in window);
                document.body.appendChild(subtree);
                const nested = window.nestedSvg === subtree.querySelector("path")
                    && typeof window.svgName === "undefined";
                document.body.removeChild(subtree);
                const nestedRemoved = !("nestedSvg" in window);
                document.body.appendChild(subtree);
                const shadowHost = document.createElement("div");
                const shadowRoot = shadowHost.attachShadow({ mode: "open" });
                const shadowChild = document.createElement("span");
                shadowChild.id = "shadowOnly";
                shadowRoot.appendChild(shadowChild);
                document.body.appendChild(shadowHost);
                const originalFetch = window.fetch;
                const collision = document.createElement("div");
                collision.id = "fetch";
                document.body.appendChild(collision);
                document.body.removeChild(collision);
                return [
                    detachedIdAbsent,
                    first,
                    renamed,
                    removed,
                    reattached,
                    parsed,
                    !("parsedName" in window),
                    detachedNestedAbsent,
                    nested,
                    nestedRemoved,
                    window.nestedSvg === subtree.querySelector("path"),
                    !("shadowOnly" in window),
                    window.fetch === originalFetch
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true, true, true, true, true, true, true, true, true, true, true, true,
                true
            ])
        );
    }

    #[test]
    fn explicit_viewport_is_distinct_from_fingerprinted_screen() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1024.0, 768.0);
        rt.run_page_init();
        let result = rt
            .evaluate(
                "return [innerWidth, innerHeight, visualViewport.width,\
                         visualViewport.height, screen.width > 0, screen.height > 0];",
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([1024, 768, 1024, 768, true, true])
        );
    }

    #[test]
    fn screen_override_is_independent_live_and_preserves_screen_identity() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1024.0, 768.0);
        rt.run_page_init();
        rt.execute_script(
            "remember-screen",
            "globalThis.__screenBefore = screen;\
             globalThis.__screenSizeBefore = [screen.width, screen.height];",
        )
        .unwrap();

        rt.set_screen_size_override(Some((1440.0, 900.0)), true);
        assert_eq!(
            rt.evaluate(
                "[innerWidth, innerHeight, screen.width, screen.height,\
                  screen.availWidth, screen.availHeight, screen === __screenBefore]"
            )
            .unwrap(),
            serde_json::json!([1024, 768, 1440, 900, 1440, 900, true])
        );

        rt.set_screen_size_override(None, false);
        assert_eq!(
            rt.evaluate(
                "[innerWidth, innerHeight, screen.width === __screenSizeBefore[0],\
                  screen.height === __screenSizeBefore[1],\
                  screen.availHeight === screen.height - 40,\
                  screen === __screenBefore]"
            )
            .unwrap(),
            serde_json::json!([1024, 768, true, true, true, true])
        );
    }

    #[test]
    fn match_media_evaluates_query_lists_conjunctions_ranges_and_orientation() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1280.0, 720.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                return [
                    matchMedia("(min-width: 1024px) and (min-height: 700px)").matches,
                    matchMedia("(min-width: 1024px) and (min-height: 900px)").matches,
                    matchMedia("(max-width: 600px), screen and (orientation: landscape)").matches,
                    matchMedia("not print").matches,
                    matchMedia("not screen").matches,
                    matchMedia("only screen and (width: 1280px) and (height = 720px)").matches,
                    matchMedia("(1000px <= width < 1400px) and (height > 700px)").matches,
                    matchMedia("(orientation: portrait)").matches,
                    matchMedia("(prefers-color-scheme: light) and (pointer: fine) and (hover: hover)").matches,
                    matchMedia("(obscura-unknown-feature: yes)").matches
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, false, true, true, false, true, true, false, true, false])
        );
    }

    #[test]
    fn match_media_matches_are_live_across_viewport_resizes() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(900.0, 600.0);
        rt.run_page_init();
        assert_eq!(
            rt.evaluate(
                r#"
                return [
                    (globalThis.__wideAndShort = matchMedia(
                        "(min-width: 800px) and (max-height: 700px)"
                    )).matches,
                    (globalThis.__portrait = matchMedia(
                        "(orientation: portrait)"
                    )).matches
                ];
                "#,
            )
            .unwrap(),
            serde_json::json!([true, false])
        );

        rt.set_viewport(600.0, 900.0);
        assert_eq!(
            rt.evaluate(
                "return [__wideAndShort.matches, __portrait.matches,\
                         matchMedia('(max-width: 600px), print').matches];",
            )
            .unwrap(),
            serde_json::json!([false, true, true])
        );
    }

    #[test]
    fn computed_style_access_does_not_get_shadowed_by_inline_style_proxy() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="box" style="opacity:.5;width:40px"></div></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const computed = getComputedStyle(box);
                return [
                    computed.display,
                    computed.visibility,
                    computed.opacity,
                    computed.width,
                    computed.getPropertyValue("display"),
                    computed.getPropertyValue("background-color")
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "block",
                "visible",
                "0.5",
                "40px",
                "block",
                "rgba(0, 0, 0, 0)"
            ])
        );
    }

    #[test]
    fn hyperlink_content_attributes_reflect_through_the_idl_surface() {
        let mut rt = setup_runtime(
            r#"<html><body>
                <a id="locale" hreflang="en-US" rel="alternate"
                   target="_blank" download="guide.pdf"
                   ping="/audit" referrerpolicy="no-referrer">English</a>
            </body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const link = document.getElementById("locale");
                const initial = [
                    link.hreflang, link.rel, link.target, link.download,
                    link.ping, link.referrerPolicy,
                    link.hreflang.split("-")[1]
                ];
                link.hreflang = "de-DE";
                link.referrerPolicy = "origin";
                return [
                    initial,
                    link.getAttribute("hreflang"),
                    link.getAttribute("referrerpolicy")
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [
                    "en-US",
                    "alternate",
                    "_blank",
                    "guide.pdf",
                    "/audit",
                    "no-referrer",
                    "US"
                ],
                "de-DE",
                "origin"
            ])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn ordinary_inline_keeps_computed_sizes_but_uses_content_geometry() {
        let dom = parse_html(
            r#"<html><head><style>
                html,body,p { margin:0 }
                #host { width:300px; font-size:16px; line-height:20px }
                #token {
                    position:relative;
                    width:100%; height:100px;
                    min-width:100%; min-height:100px;
                    max-width:100%; max-height:100px;
                    padding:0 5px; background:red
                }
                #after { position:relative }
                #atomic {
                    display:inline-block; box-sizing:border-box;
                    width:80px; height:30px; padding:0; border:0
                }
                #replaced {
                    display:inline; box-sizing:border-box;
                    width:80px; height:30px; min-width:0; min-height:0;
                    max-width:none; max-height:none; padding:0; border:0
                }
                #items { display:flex }
                #item {
                    display:inline; box-sizing:border-box; flex:none;
                    width:90px; height:25px; padding:0; border:0
                }
            </style></head><body>
                <p id="host">A <code id="token">token</code> <span id="after">after</span></p>
                <span id="atomic"></span>
                <input id="replaced">
                <div id="items"><span id="item"></span></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 240.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const token = document.getElementById("token");
                const after = document.getElementById("after");
                const computed = getComputedStyle(token);
                const rect = token.getBoundingClientRect();
                const afterRect = after.getBoundingClientRect();
                const atomic = document.getElementById("atomic").getBoundingClientRect();
                const replaced = document.getElementById("replaced").getBoundingClientRect();
                const item = document.getElementById("item").getBoundingClientRect();
                return {
                    computed: [
                        computed.width, computed.height,
                        computed.minWidth, computed.minHeight,
                        computed.maxWidth, computed.maxHeight
                    ],
                    rect: [rect.x, rect.y, rect.width, rect.height],
                    after: [afterRect.x, afterRect.y],
                    client: [token.clientWidth, token.clientHeight],
                    clientRects: Array.from(token.getClientRects(), r => [
                        r.x, r.y, r.width, r.height
                    ]),
                    atomic: [atomic.width, atomic.height],
                    replaced: [replaced.width, replaced.height],
                    item: [item.width, item.height, getComputedStyle(item).display]
                };
                "#,
            )
            .unwrap();

        assert_eq!(
            result["computed"],
            serde_json::json!(["100%", "100px", "100%", "100px", "100%", "100px"])
        );
        let rect = result["rect"].as_array().unwrap();
        let token_x = rect[0].as_f64().unwrap();
        let token_y = rect[1].as_f64().unwrap();
        let token_width = rect[2].as_f64().unwrap();
        let token_height = rect[3].as_f64().unwrap();
        assert!(
            token_width > 20.0 && token_width < 100.0,
            "ordinary inline should hug text and padding: {rect:?}"
        );
        assert!(
            token_height < 40.0,
            "ignored block size leaked into geometry"
        );
        assert_eq!(result["client"], serde_json::json!([0, 0]));
        let client_rects = result["clientRects"].as_array().unwrap();
        assert_eq!(client_rects.len(), 1);
        let client_rect = client_rects[0].as_array().unwrap();
        for (actual, expected) in client_rect
            .iter()
            .map(|value| value.as_f64().unwrap())
            .zip([token_x, token_y, token_width, token_height])
        {
            assert!(
                (actual - expected).abs() < 0.001,
                "getClientRects must expose the renderer's inline fragments"
            );
        }
        let after = result["after"].as_array().unwrap();
        assert!(after[0].as_f64().unwrap() >= token_x + token_width - 0.01);
        assert!((after[1].as_f64().unwrap() - token_y).abs() < 0.01);
        assert_eq!(result["atomic"], serde_json::json!([80, 30]));
        assert_eq!(result["replaced"], serde_json::json!([80, 30]));
        assert_eq!(result["item"], serde_json::json!([90, 25, "block"]));
    }

    #[cfg(feature = "render")]
    #[test]
    fn computed_style_uses_renderer_stylesheet_cascade_and_invalidates() {
        let dom = parse_html(
            r#"<html><head><style>
                .base {
                    display:flex; position:relative; z-index:7;
                    visibility:hidden; opacity:.35;
                    background-color:rgb(10,20,30); color:rgb(40,50,60);
                    width:120px; height:40px; min-width:20px; max-width:160px;
                    box-sizing:border-box; overflow-x:clip; overflow-y:visible;
                    margin:1px 2px 3px 4px; padding:5px 6px 7px 8px;
                    border:2px solid rgb(70,80,90);
                    flex-direction:column; flex-wrap:wrap;
                    align-items:center; justify-content:space-between;
                    gap:6px 9px; transform:translate(3px,4px);
                }
                .alt { display:grid; width:150px; opacity:.8; }
            </style></head><body><div id="box" class="base"></div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 200.0);
        rt.run_page_init();

        let initial = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const c = getComputedStyle(box);
                return [
                    c.display, c.position, c.zIndex, c.visibility, c.opacity,
                    c.backgroundColor, c.getPropertyValue("color"),
                    c.width, c.height, c.minWidth, c.maxWidth, c.boxSizing,
                    c.overflowX, c.overflowY,
                    c.marginTop, c.marginRight, c.marginBottom, c.marginLeft,
                    c.paddingTop, c.borderLeftWidth, c.borderLeftColor,
                    c.flexDirection, c.flexWrap, c.alignItems,
                    c.justifyContent, c.rowGap, c.columnGap, c.transform
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            initial,
            serde_json::json!([
                "flex",
                "relative",
                "7",
                "hidden",
                "0.35",
                "rgb(10, 20, 30)",
                "rgb(40, 50, 60)",
                "120px",
                "40px",
                "20px",
                "160px",
                "border-box",
                "clip",
                "visible",
                "1px",
                "2px",
                "3px",
                "4px",
                "5px",
                "2px",
                "rgb(70, 80, 90)",
                "column",
                "wrap",
                "center",
                "space-between",
                "6px",
                "9px",
                "matrix(1, 0, 0, 1, 3, 4)"
            ])
        );

        assert_eq!(
            rt.evaluate(
                r#"
                const box = document.getElementById("box");
                box.className = "alt";
                const c = getComputedStyle(box);
                return [c.display, c.width, c.opacity];
                "#,
            )
            .unwrap(),
            serde_json::json!(["grid", "150px", "0.8"])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn webkit_truncation_computed_names_use_native_support_and_vendor_prefixes() {
        let dom = parse_html(
            r#"<html><head><style>
              #clamp { display:-webkit-box; -webkit-box-orient:vertical;
                       -webkit-line-clamp:2; overflow:hidden; }
              #legacy { display:-webkit-inline-box; -webkit-box-orient:horizontal; }
            </style></head><body>
              <div id="clamp">one two three four five six seven eight</div>
              <span id="legacy">legacy</span>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(120.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const clamp = getComputedStyle(document.getElementById("clamp"));
                const legacy = getComputedStyle(document.getElementById("legacy"));
                return {
                  supports: [
                    CSS.supports("text-overflow", "ellipsis"),
                    CSS.supports("-webkit-line-clamp", "2"),
                    CSS.supports("-webkit-line-clamp", "0"),
                    CSS.supports("display", "-webkit-box"),
                    CSS.supports("-webkit-box-orient", "vertical")
                  ],
                  clamp: [clamp.display, clamp.webkitLineClamp,
                    clamp.webkitBoxOrient,
                    clamp.getPropertyValue("-webkit-line-clamp")],
                  legacy: [legacy.display, legacy.webkitBoxOrient]
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "supports": [true, true, false, true, true],
                "clamp": ["flow-root", "2", "vertical", "2"],
                "legacy": ["-webkit-inline-box", "horizontal"]
            })
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn computed_typography_uses_resolved_renderer_values() {
        let dom = parse_html(
            r#"<html><head><style>
                #parent {
                    font-size:20px; line-height:1.5;
                    letter-spacing:-.05em; white-space:pre-wrap;
                    text-align:end
                }
                #child { font-size:10px }
                #zero { letter-spacing:0px; white-space:break-spaces }
            </style></head><body>
                <div id="parent"><span id="child">child</span></div>
                <div id="zero">zero</div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const sample = id => {
                    const s = getComputedStyle(document.getElementById(id));
                    return [s.lineHeight, s.letterSpacing, s.whiteSpace, s.textAlign];
                };
                return [sample("parent"), sample("child"), sample("zero")];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                ["30px", "-1px", "pre-wrap", "end"],
                ["15px", "-1px", "pre-wrap", "end"],
                ["normal", "normal", "break-spaces", "start"],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn computed_style_exposes_cascaded_custom_properties_and_invalidates() {
        let dom = parse_html(
            r#"<html><head><style>
                :root { --inherited-space: 17px; --derived-space: var(--inherited-space); }
                #nav {
                    --r-globalnav-font-size:17px;
                    --local-scale:1.25;
                    font-size:var(--r-globalnav-font-size);
                }
            </style></head><body>
                <nav id="nav"><span id="child"></span></nav>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 200.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"
                const nav = document.getElementById("nav");
                const child = document.getElementById("child");
                const navStyle = getComputedStyle(nav);
                const childStyle = getComputedStyle(child);
                let enumeratesBase = false;
                for (let i = 0; i < navStyle.length; i++) {
                    if (navStyle.item(i) === "--r-globalnav-font-size")
                        enumeratesBase = true;
                }
                return [
                    navStyle.fontSize,
                    navStyle.getPropertyValue("--r-globalnav-font-size"),
                    parseInt(navStyle.fontSize) /
                        parseInt(navStyle.getPropertyValue("--r-globalnav-font-size")),
                    navStyle.getPropertyValue("--inherited-space"),
                    navStyle.getPropertyValue("--derived-space"),
                    navStyle.getPropertyValue("--local-scale"),
                    childStyle.getPropertyValue("--inherited-space"),
                    childStyle.getPropertyValue("--local-scale"),
                    enumeratesBase
                ];
                "#,
            )
            .unwrap(),
            serde_json::json!(["17px", "17px", 1, "17px", "17px", "1.25", "17px", "1.25", true])
        );

        assert_eq!(
            rt.evaluate(
                r#"
                const nav = document.getElementById("nav");
                const computed = getComputedStyle(nav);
                nav.style.setProperty("--inherited-space", "23px");
                nav.style.fontSize = "19px";
                return [
                    computed.fontSize,
                    computed.getPropertyValue("--inherited-space"),
                    computed.getPropertyValue("--derived-space")
                ];
                "#,
            )
            .unwrap(),
            serde_json::json!(["19px", "23px", "17px"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_event_loop_flushes_resolved_promise_continuations() {
        let mut rt = setup_runtime("<html><body><div id='state'>pending</div></body></html>");
        rt.execute_script(
            "font-ready",
            "document.fonts.load('normal 1px Example').then(() => {\
                 document.getElementById('state').textContent = 'ready';\
             });",
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("document.getElementById('state').textContent")
                .unwrap(),
            serde_json::json!("ready")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_does_not_wait_for_analytics_interval() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-long-interval",
            "setInterval(() => { globalThis.__analyticsTicks = (globalThis.__analyticsTicks || 0) + 1; }, 1000);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(1_000, 50).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "a future analytics interval must not consume the full settle budget"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fixed_duration_event_loop_yields_from_continuously_ready_tasks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "fixed-duration-continuously-ready",
            "globalThis.__fixedTicks = 0;\
             setInterval(() => { __fixedTicks++; }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_bounded(40).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(300),
            "a continuously-ready queue must return between tasks instead of waiting for the watchdog: {elapsed:?}",
        );
        assert!(
            rt.evaluate("globalThis.__fixedTicks > 0")
                .unwrap()
                .as_bool()
                .unwrap_or(false),
            "the cooperative fixed wait must still execute queued tasks",
        );
        assert_eq!(
            rt.evaluate(
                "(document.body.setAttribute('data-after-fixed-wait', 'usable'), \
                 document.body.getAttribute('data-after-fixed-wait'))",
            )
            .unwrap(),
            serde_json::json!("usable"),
            "the isolate must remain usable after the fixed wait",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn short_observation_deadline_does_not_terminate_the_active_task() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "task-crossing-observation-deadline",
            "globalThis.__longTaskCompleted = false;\
             setTimeout(() => {\
               const end = performance.now() + 600;\
               while (performance.now() < end) {}\
               __longTaskCompleted = true;\
             }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_bounded(20).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(500)
                && elapsed < std::time::Duration::from_millis(1_500),
            "capture must wait for the active task boundary without becoming unbounded: {elapsed:?}",
        );
        assert_eq!(
            rt.evaluate("globalThis.__longTaskCompleted").unwrap(),
            serde_json::json!(true),
            "a screenshot/readiness deadline must not terminate valid page work",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn adaptive_observation_deadline_does_not_terminate_the_active_task() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "adaptive-task-crossing-observation-deadline",
            "globalThis.__adaptiveLongTaskCompleted = false;\
             setTimeout(() => {\
               const end = performance.now() + 600;\
               while (performance.now() < end) {}\
               __adaptiveLongTaskCompleted = true;\
             }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(20, 10).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(500)
                && elapsed < std::time::Duration::from_millis(1_500),
            "adaptive settle must wait for the active task boundary: {elapsed:?}",
        );
        assert_eq!(
            rt.evaluate("globalThis.__adaptiveLongTaskCompleted")
                .unwrap(),
            serde_json::json!(true),
            "adaptive readiness must not terminate valid page work",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_yields_from_continuously_ready_non_visual_work() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-continuously-ready",
            "setInterval(() => {\
                 globalThis.__schedulerTicks = (globalThis.__schedulerTicks || 0) + 1;\
             }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(2_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "a continuously-ready non-visual scheduler pinned adaptive settle: {elapsed:?}"
        );
        assert!(
            rt.evaluate("globalThis.__schedulerTicks > 0")
                .unwrap()
                .as_bool()
                .unwrap_or(false),
            "the cooperative policy must still drive scheduler work"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_bounds_a_single_unyielding_callback_drain() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-unyielding-task",
            "setTimeout(() => { while (true) {} }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(2_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS)
                && elapsed
                    < std::time::Duration::from_millis(
                        SYNCHRONOUS_TASK_FLOOR_MS + 1_500,
                    ),
            "one synchronous callback drain escaped the bounded task allowance: {elapsed:?}"
        );
        assert_eq!(
            rt.evaluate(
                "(document.body.setAttribute('data-after-watchdog', 'usable'), \
                  document.body.getAttribute('data-after-watchdog'))",
            )
            .unwrap(),
            serde_json::json!("usable"),
            "the per-turn watchdog must leave the isolate reusable",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_retains_delayed_network_and_dom_update() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            std::sync::Arc::new(obscura_net::CookieJar::new()),
            None,
            true,
        ));
        let in_flight = rt.state.borrow().page_in_flight.clone();
        in_flight.store(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            in_flight.store(0, std::sync::atomic::Ordering::SeqCst);
        });
        rt.set_http_client(client);
        rt.execute_script(
            "quiescent-delayed-work",
            "setInterval(() => {}, 1000);\
             setTimeout(() => document.body.setAttribute('data-ready', 'ready'), 40);",
        )
        .unwrap();

        rt.run_event_loop_until_quiescent(1_000, 150).await.unwrap();
        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("ready"),
        );
    }

    fn delayed_fetch_runtime(
        response_delay: std::time::Duration,
    ) -> (ObscuraJsRuntime, std::sync::mpsc::Receiver<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            accepted_tx.send(()).unwrap();
            std::thread::sleep(response_delay);
            let body = "hydrated";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
        });

        let origin = format!("http://{address}");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html("<html><body></body></html>"));
        rt.set_url(&format!("{origin}/page"));
        rt.set_http_client(std::sync::Arc::new(
            obscura_net::ObscuraHttpClient::with_full_options(
                std::sync::Arc::new(obscura_net::CookieJar::new()),
                None,
                true,
            ),
        ));
        rt.run_page_init();
        (rt, accepted_rx)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_allows_fetch_hydration_within_network_grace() {
        let (mut rt, accepted) =
            delayed_fetch_runtime(std::time::Duration::from_millis(700));
        rt.execute_script(
            "quiescent-fetch-hydration",
            "fetch('/hydrate').then(response => response.text()).then(text => {\
                 document.body.setAttribute('data-ready', text);\
             });",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(3_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        accepted
            .recv_timeout(std::time::Duration::from_millis(100))
            .expect("fixture fetch was not issued");
        assert!(
            elapsed >= std::time::Duration::from_millis(650),
            "settle returned before the delayed response: {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1_500),
            "completed hydration should only pay its following quiet window: {elapsed:?}"
        );
        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("hydrated"),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_bounds_a_hanging_page_request() {
        let (mut rt, accepted) = delayed_fetch_runtime(std::time::Duration::from_secs(3));
        rt.execute_script(
            "quiescent-hanging-fetch",
            "fetch('/analytics').catch(() => {});",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(4_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        accepted
            .recv_timeout(std::time::Duration::from_millis(100))
            .expect("fixture fetch was not issued");
        assert!(
            elapsed >= std::time::Duration::from_millis(900),
            "pending page work must receive the network grace: {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1_700),
            "a hanging request consumed more than its bounded grace: {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_gives_post_grace_dom_activity_a_quiet_window() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.state
            .borrow()
            .page_in_flight
            .store(1, std::sync::atomic::Ordering::SeqCst);
        rt.execute_script(
            "quiescent-post-grace-commit",
            "setInterval(() => {}, 1000);\
             setTimeout(() => document.body.setAttribute('data-ready', 'late'), 1100);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(4_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("late"),
        );
        assert!(
            elapsed >= std::time::Duration::from_millis(1_200),
            "the late commit did not receive a following quiet window: {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1_800),
            "late observable work escaped the bounded activity tail: {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescence_ignores_another_pages_shared_client_request() {
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            std::sync::Arc::new(obscura_net::CookieJar::new()),
            None,
            true,
        ));
        client
            .in_flight
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.set_http_client(client);
        rt.execute_script("quiescent-shared-client", "setInterval(() => {}, 1000);")
            .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(1_000, 50).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "an unrelated page request on the shared client must not pin settle"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_retains_near_term_render_timeout() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-render-timeout",
            "setInterval(() => {}, 1000);\
             setTimeout(() => document.body.setAttribute('data-ready', 'ready'), 200);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(1_000, 150).await.unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(180));
        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("ready"),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn quiescent_event_loop_bounds_continuous_visual_mutations() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-animated-page",
            "let tick=0;setInterval(() =>\
               document.body.setAttribute('data-frame', String(++tick)), 10);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(2_000, 150).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1_000),
            "an animated document must not consume the complete settle budget"
        );
        assert!(
            rt.evaluate("Number(document.body.getAttribute('data-frame')) > 0")
                .unwrap()
                .as_bool()
                .unwrap_or(false),
            "the policy must still pump animation work before capture"
        );
    }

    #[test]
    fn font_face_set_tracks_authored_and_script_created_faces() {
        let mut rt = setup_runtime(
            r#"<html><head><style>
                @font-face {
                    font-family: "Authored One";
                    src: url("https://assets.test/one.woff2") format("woff2");
                    font-weight: 350 650;
                }
                @font-face {
                    font-family: AuthoredTwo;
                    src: url(data:font/woff2;base64,d09GMg==);
                    font-style: italic;
                }
            </style></head><body></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"(() => {
                    const authored = Array.from(document.fonts);
                    const cssDelete = document.fonts.delete(authored[0]);
                    const dynamic = new FontFace("Dynamic", "url('/dynamic.ttf')", {
                        style: "oblique 12deg",
                        weight: "700",
                        stretch: "condensed",
                        unicodeRange: "U+20-7E",
                        display: "swap"
                    });
                    const addResult = document.fonts.add(dynamic);
                    const visited = [];
                    document.fonts.forEach((value, key, set) => {
                        visited.push(value === key && set === document.fonts);
                    });
                    const afterAdd = [
                        document.fonts.size,
                        document.fonts.has(dynamic),
                        addResult === document.fonts,
                        dynamic.family,
                        dynamic.style,
                        dynamic.weight,
                        dynamic.stretch,
                        dynamic.unicodeRange,
                        dynamic.display,
                        visited.every(Boolean)
                    ];
                    const deleted = document.fonts.delete(dynamic);
                    document.fonts.clear();
                    const bytes = new Uint8Array([0, 1, 2, 253, 254, 255]);
                    const binary = new FontFace("Binary", bytes, { weight: 600 });
                    return [
                        authored.length,
                        authored.map(face => face.family),
                        cssDelete,
                        afterAdd,
                        deleted,
                        document.fonts.size,
                        binary.status,
                        binary.loaded === binary.load()
                    ];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                2,
                ["Authored One", "AuthoredTwo"],
                false,
                [
                    3,
                    true,
                    true,
                    "Dynamic",
                    "oblique 12deg",
                    "700",
                    "condensed",
                    "U+20-7E",
                    "swap",
                    true
                ],
                true,
                2,
                "loaded",
                true
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn font_face_load_updates_status_set_readiness_and_matching() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "font-face-lifecycle",
            r#"
                globalThis.__fontEvents = [];
                const face = new FontFace("Lifecycle", "url('/lifecycle.woff2')", {
                    weight: "700"
                });
                document.fonts.onloading = event => __fontEvents.push([event.type, event.fontfaces.length]);
                document.fonts.onloadingdone = event => __fontEvents.push([event.type, event.fontfaces.length]);
                document.fonts.add(face);
                globalThis.__fontBefore = [
                    face.status,
                    document.fonts.status,
                    document.fonts.check("700 16px Lifecycle")
                ];
                globalThis.__fontLoadResult = "pending";
                document.fonts.load("700 16px Lifecycle").then(faces => {
                    __fontLoadResult = [faces.length, faces[0] === face, face.status,
                        document.fonts.check("700 16px Lifecycle")];
                });
                document.fonts.ready.then(set => {
                    globalThis.__fontReady = set === document.fonts;
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        let result = rt
            .evaluate("return [__fontBefore, __fontLoadResult, __fontReady, __fontEvents];")
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                ["unloaded", "loaded", false],
                [1, true, "loaded", true],
                true,
                [["loading", 1], ["loadingdone", 1]]
            ])
        );
    }

    #[test]
    fn animation_frame_requires_a_callable_callback() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    try {
                        requestAnimationFrame(null);
                        return [false, ""];
                    } catch (error) {
                        return [error instanceof TypeError, error.name];
                    }
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, "TypeError"]));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn animation_frames_are_ordered_batches_with_rendering_timestamps() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "animation-frame-order",
            r#"
                globalThis.__rafEvents = [];
                globalThis.__rafStamps = [];
                Promise.resolve().then(() => __rafEvents.push("microtask-before"));
                setTimeout(() => __rafEvents.push("timer"), 1);
                requestAnimationFrame((timestamp) => {
                    __rafEvents.push("raf-a");
                    __rafStamps.push(timestamp);
                    Promise.resolve().then(() => __rafEvents.push("microtask-in-raf"));
                    requestAnimationFrame((nextTimestamp) => {
                        __rafEvents.push("raf-next");
                        __rafStamps.push(nextTimestamp);
                    });
                });
                requestAnimationFrame((timestamp) => {
                    __rafEvents.push("raf-b");
                    __rafStamps.push(timestamp);
                });
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(150).await.unwrap();
        let result = rt
            .evaluate(
                r#"[
                    __rafEvents,
                    __rafStamps.length,
                    __rafStamps[0] === __rafStamps[1],
                    __rafStamps[2] > __rafStamps[1]
                ]"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [
                    "microtask-before",
                    "timer",
                    "raf-a",
                    "raf-b",
                    "microtask-in-raf",
                    "raf-next"
                ],
                3,
                true,
                true
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rendering_opportunity_orders_raf_resize_and_intersection_phases() {
        let mut rt = setup_runtime(
            "<html><body><div id='target' style='width:20px;height:20px'></div></body></html>",
        );
        rt.execute_script(
            "rendering-opportunity-order",
            r#"
                globalThis.__renderPhaseOrder = [];
                const target = document.getElementById("target");
                new ResizeObserver(() => __renderPhaseOrder.push("resize")).observe(target);
                new IntersectionObserver(() => __renderPhaseOrder.push("intersection")).observe(target);
                requestAnimationFrame(() => {
                    __renderPhaseOrder.push("raf");
                    target.style.width = "40px";
                });
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__renderPhaseOrder.slice(0, 3)").unwrap(),
            serde_json::json!(["raf", "resize", "intersection"]),
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn raf_geometry_mutation_reaches_settled_intersection_before_next_frame() {
        let mut rt = setup_runtime(
            "<html><body style='margin:0'><div id='spacer' style='height:150px'></div><div id='target' style='height:20px'></div></body></html>",
        );
        rt.set_viewport(200.0, 100.0);
        rt.execute_script(
            "settle-intersection",
            r#"
                globalThis.__sameFrameOrder = [];
                globalThis.__sameFrameInitial = false;
                const target = document.getElementById("target");
                globalThis.__sameFrameObserver = new IntersectionObserver(entries => {
                    if (!__sameFrameInitial) {
                        __sameFrameInitial = true;
                        return;
                    }
                    if (entries.some(entry => entry.isIntersecting)) {
                        __sameFrameOrder.push("intersection");
                    }
                });
                __sameFrameObserver.observe(target);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(50).await.unwrap();

        rt.execute_script(
            "mutate-in-animation-frame",
            r#"
                requestAnimationFrame(() => {
                    __sameFrameOrder.push("raf");
                    document.getElementById("spacer").style.height = "0px";
                    requestAnimationFrame(() => __sameFrameOrder.push("next-raf"));
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(80).await.unwrap();

        assert_eq!(
            rt.evaluate("__sameFrameOrder").unwrap(),
            serde_json::json!(["raf", "intersection", "next-raf"]),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_animation_frame_removes_pending_and_current_batch_callbacks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "animation-frame-cancel",
            r#"
                globalThis.__rafEvents = [];
                const pending = requestAnimationFrame(() => __rafEvents.push("pending"));
                cancelAnimationFrame(pending);
                let sameBatch;
                requestAnimationFrame(() => {
                    __rafEvents.push("first");
                    cancelAnimationFrame(sameBatch);
                });
                sameBatch = requestAnimationFrame(() => __rafEvents.push("same-batch"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__rafEvents").unwrap(),
            serde_json::json!(["first"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn self_requeueing_animation_frame_yields_to_timer_tasks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "animation-frame-yield",
            r#"
                globalThis.__rafCount = 0;
                globalThis.__rafStopped = false;
                globalThis.__timerAfterAnimation = false;
                let frameId = 0;
                function frame() {
                    __rafCount++;
                    frameId = requestAnimationFrame(frame);
                }
                frameId = requestAnimationFrame(frame);
                setTimeout(() => {
                    cancelAnimationFrame(frameId);
                    __rafStopped = true;
                }, 55);
                setTimeout(() => {
                    __timerAfterAnimation = true;
                }, 65);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(200).await.unwrap();
        let result = rt
            .evaluate("[__rafCount, __rafStopped, __timerAfterAnimation]")
            .unwrap();
        let values = result.as_array().unwrap();
        let frame_count = values[0].as_u64().unwrap();
        assert!(
            (2..=5).contains(&frame_count),
            "expected a few paced animation frames before cancellation, got {frame_count}"
        );
        assert_eq!(values[1], serde_json::json!(true));
        assert_eq!(values[2], serde_json::json!(true));
    }

    #[test]
    fn test_document_url() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let url = rt.evaluate("document.URL").unwrap();
        assert_eq!(url, serde_json::json!("http://example.com/test"));
    }

    #[test]
    fn test_query_selector() {
        let mut rt = setup_runtime("<html><body><h1>Hello</h1><p>World</p></body></html>");
        let text = rt
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("Hello"));
    }

    #[test]
    fn test_query_selector_all() {
        let mut rt = setup_runtime("<ul><li>A</li><li>B</li><li>C</li></ul>");
        let count = rt
            .evaluate("document.querySelectorAll('li').length")
            .unwrap();
        assert_eq!(count.as_f64().unwrap() as i64, 3);
    }

    #[test]
    fn css_supports_matches_capabilities_and_boolean_conditions() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"JSON.stringify([
                    CSS.supports("-webkit-hyphens", "none"),
                    CSS.supports("margin-trim", "inline"),
                    CSS.supports("-moz-orient", "inline"),
                    CSS.supports("color", "rgb(from red r g b)"),
                    CSS.supports("(((-webkit-hyphens:none)) and (not (margin-trim:inline))) or ((-moz-orient:inline) and (not (color:rgb(from red r g b))))"),
                    CSS.supports("display", "grid"),
                    CSS.supports("(display:grid) and (selector(.card > *))"),
                    CSS.supports("not (unknown-engine-prop:value)"),
                    CSS.supports("selector(.card >)"),
                    CSS.supports("selector(:obscura-unknown)"),
                    CSS.supports("selector(.card,)"),
                    CSS.supports("scrollbar-gutter", "stable"),
                    CSS.supports("scrollbar-gutter", "floating"),
                    CSS.supports("color", "light-dark(rgb(1, 2, 3), color-mix(in srgb, white 50%, black))"),
                    CSS.supports("(color:light-dark(red, light-dark(white, black)))"),
                    CSS.supports("color", "light-dark(red)"),
                    CSS.supports("color", "light-dark(red, rgb(1, 2, 3)"),
                    CSS.supports("border", "2px dashed red"),
                    CSS.supports("border-width", "10%"),
                    CSS.supports("word-break", "break-all"),
                    CSS.supports("filter", "blur(2px)"),
                    CSS.supports("content", "attr(data-label)"),
                    CSS.supports("display", "grid;"),
                    CSS.supports("flex-flow", "column"),
                    CSS.supports("flex-flow", "wrap column"),
                    CSS.supports("flex-flow", "column wrap"),
                    CSS.supports("flex-flow", "row column"),
                    CSS.supports("flex-flow", "nowrap wrap-reverse"),
                    CSS.supports("(flex-flow:column)")
                ])"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!("[false,false,false,false,false,true,true,true,false,false,false,true,false,true,true,false,false,true,false,true,false,true,false,true,true,true,false,false,true]")
        );
    }

    #[test]
    fn test_get_element_by_id() {
        let mut rt = setup_runtime(r#"<div id="test">Content</div>"#);
        let tag = rt
            .evaluate("document.getElementById('test').tagName")
            .unwrap();
        assert_eq!(tag, serde_json::json!("DIV"));
    }

    #[test]
    fn attributes_named_node_map_is_live() {
        let mut rt = setup_runtime(r#"<div id="test" class="card" data-state="ready"></div>"#);
        let result = rt
            .evaluate(
                r#"
                const element = document.getElementById("test");
                const attributes = element.attributes;
                const sameObject = attributes === element.attributes;
                const firstName = attributes[0].name;
                let removed = 0;
                while (attributes.length) {
                    element.removeAttributeNode(attributes[0]);
                    removed++;
                    if (removed > 10) throw new Error("NamedNodeMap is not live");
                }
                return {
                    sameObject,
                    namedNodeMap: attributes instanceof NamedNodeMap,
                    firstName,
                    removed,
                    length: attributes.length,
                    hasAttributes: element.hasAttributes(),
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "sameObject": true,
                "namedNodeMap": true,
                "firstName": "id",
                "removed": 3,
                "length": 0,
                "hasAttributes": false,
            })
        );
    }

    #[test]
    fn script_created_attribute_reads_stay_coherent_across_mutation_apis() {
        let mut rt = setup_runtime(r#"<html><body></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const element = document.createElement("DIV");
                    const initial = element.getAttribute("data-state");
                    element.setAttribute("DATA-STATE", "ready");
                    const ordinary = [
                        element.getAttribute("data-state"),
                        element.getAttribute("DATA-STATE"),
                    ];
                    element.setAttributeNS(null, "data-state", "namespaced");
                    const namespaced = element.getAttribute("data-state");
                    element.removeAttributeNS(null, "data-state");
                    const removed = element.getAttribute("data-state");
                    return { initial, ordinary, namespaced, removed };
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "initial": null,
                "ordinary": ["ready", "ready"],
                "namespaced": "namespaced",
                "removed": null,
            })
        );
    }

    #[test]
    fn structural_cache_tracks_detach_reparent_and_rejected_mutations() {
        let mut rt = setup_runtime(r#"<html><body></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const host = document.createElement("div");
                    const child = document.createElement("span");
                    const text = document.createTextNode("hello");
                    const fresh = [host.parentNode, host.isConnected, text.parentNode, text.isConnected];

                    host.appendChild(child);
                    const detachedTree = [child.parentNode === host, host.isConnected, child.isConnected];
                    document.body.appendChild(host);
                    const connectedTree = [host.isConnected, child.isConnected, child.parentNode === host];

                    const other = document.createElement("section");
                    document.body.appendChild(other);
                    const afterUnrelatedMutation = [host.parentNode === document.body, child.isConnected];

                    other.appendChild(child);
                    const reparented = [child.parentNode === other, child.isConnected, host.firstChild === null];

                    let wrongReference = "";
                    try { other.insertBefore(document.createElement("b"), host); }
                    catch (error) { wrongReference = error.name; }
                    let wrongReplacement = "";
                    try { other.replaceChild(document.createElement("i"), host); }
                    catch (error) { wrongReplacement = error.name; }
                    let cycle = "";
                    try { child.appendChild(other); }
                    catch (error) { cycle = error.name; }

                    document.body.removeChild(other);
                    const removedTree = [other.parentNode, other.isConnected, child.isConnected, child.parentNode === other];
                    document.body.appendChild(other);
                    const reattachedTree = [other.isConnected, child.isConnected];
                    return {
                        fresh,
                        detachedTree,
                        connectedTree,
                        afterUnrelatedMutation,
                        reparented,
                        wrongReference,
                        wrongReplacement,
                        cycle,
                        removedTree,
                        reattachedTree,
                    };
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "fresh": [null, false, null, false],
                "detachedTree": [true, false, false],
                "connectedTree": [true, true, true],
                "afterUnrelatedMutation": [true, true],
                "reparented": [true, true, true],
                "wrongReference": "NotFoundError",
                "wrongReplacement": "NotFoundError",
                "cycle": "HierarchyRequestError",
                "removedTree": [null, false, false, true],
                "reattachedTree": [true, true],
            })
        );
    }

    #[test]
    fn element_scroll_methods_update_scroll_offsets() {
        let mut rt = setup_runtime(
            r#"<div id="scroller" style="width:100px;height:100px;overflow:auto">
                   <div style="width:300px;height:300px"></div>
               </div>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const element = document.getElementById("scroller");
                element.scrollTo({left: 12, top: 20, behavior: "smooth"});
                element.scrollBy(3, -5);
                element.scroll({left: 7});
                return {
                    left: element.scrollLeft,
                    top: element.scrollTop,
                    methods: [
                        typeof element.scroll,
                        typeof element.scrollTo,
                        typeof element.scrollBy,
                    ],
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "left": 7,
                "top": 15,
                "methods": ["function", "function", "function"],
            })
        );
    }

    #[test]
    fn document_fragment_get_element_by_id_searches_descendants() {
        let mut rt = setup_runtime(r#"<div id="target">document</div>"#);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const frag = document.createDocumentFragment();
                    const section = document.createElement('section');
                    section.innerHTML = '<div><span id="target">fragment</span></div><p id="a.b">literal</p>';
                    frag.appendChild(section);

                    const dup = document.createDocumentFragment();
                    const deepParent = document.createElement('div');
                    deepParent.innerHTML = '<span id="dup">deep</span>';
                    const shallow = document.createElement('p');
                    shallow.id = 'dup';
                    shallow.textContent = 'shallow';
                    dup.appendChild(deepParent);
                    dup.appendChild(shallow);

                    return [
                        frag.getElementById('target').textContent,
                        frag.getElementById('missing') === null,
                        frag.getElementById('a.b').textContent,
                        frag.getElementById(123) === null,
                        dup.getElementById('dup').textContent,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["fragment", true, "literal", true, "deep"])
        );
    }

    /// Issue #461: FILTER_REJECT must prune the rejected node's whole subtree,
    /// while FILTER_SKIP only skips the node and leaves descendants eligible.
    /// Collapsing both into "not accepted" let a TreeWalker yield nodes from
    /// inside a subtree the page explicitly rejected.
    #[test]
    fn tree_walker_filter_reject_prunes_the_whole_subtree() {
        let mut rt = setup_runtime(r#"<div id="root"><section><p>deep</p></section><a></a></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                function walk(verdict) {
                    const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                        acceptNode(node) {
                            return node.tagName === 'SECTION' ? verdict : NodeFilter.FILTER_ACCEPT;
                        }
                    });
                    const seen = [];
                    let node;
                    while ((node = w.nextNode())) seen.push(node.tagName);
                    return seen;
                }
                return [walk(NodeFilter.FILTER_REJECT), walk(NodeFilter.FILTER_SKIP)];
                "#,
            )
            .unwrap();
        // REJECT drops <p> with its <section> parent; SKIP drops only <section>.
        assert_eq!(result, serde_json::json!([["A"], ["P", "A"]]));
    }

    /// Issue #462: previousNode() must walk reverse document order until a node
    /// is accepted, not give up as soon as the first candidate is filtered out.
    #[test]
    fn previous_node_walks_reverse_document_order() {
        let mut rt = setup_runtime(r#"<div id="root"><a><b></b></a><c></c></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'B'
                            ? NodeFilter.FILTER_SKIP
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                const forward = [];
                let node;
                while ((node = w.nextNode())) forward.push(node.tagName);
                const backward = [];
                while ((node = w.previousNode())) backward.push(node.tagName);
                return [forward, backward];
                "#,
            )
            .unwrap();
        // From <c>, the previous sibling's deepest last child <b> is skipped, so
        // the walk must keep going up to <a> instead of returning null.
        assert_eq!(result, serde_json::json!([["A", "C"], ["A"]]));
    }

    /// Issue #462: a backward walk must retrace a forward walk exactly, and stop
    /// at the root without ever returning it.
    #[test]
    fn previous_node_retraces_a_full_forward_walk() {
        let mut rt = setup_runtime(
            r#"<div id="root"><section><p>one</p><span></span></section><a><b></b></a></div>"#,
        );
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                const forward = [];
                let node;
                while ((node = w.nextNode())) forward.push(node.tagName);
                const backward = [];
                while ((node = w.previousNode())) backward.push(node.tagName);
                backward.reverse();
                // previousNode never yields root, and never yields the node the
                // forward walk ended on, so compare against forward minus its last.
                // A failed traversal leaves currentNode untouched (DOM 6.1), so
                // it stays on the last node previousNode did return.
                return [forward, backward, w.currentNode.tagName];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                ["SECTION", "P", "SPAN", "A", "B"],
                ["SECTION", "P", "SPAN", "A"],
                "SECTION"
            ])
        );
    }

    /// Issue #462: FILTER_REJECT prunes a subtree in the backward direction too
    /// — the descent into a rejected node's last children must stop.
    #[test]
    fn previous_node_honours_filter_reject_subtree_pruning() {
        let mut rt =
            setup_runtime(r#"<div id="root"><a></a><section><p>deep</p></section><c></c></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'SECTION'
                            ? NodeFilter.FILTER_REJECT
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                while (w.nextNode()) { /* advance to the last accepted node */ }
                const backward = [];
                let node;
                while ((node = w.previousNode())) backward.push(node.tagName);
                return backward;
                "#,
            )
            .unwrap();
        // <p> lives inside the rejected <section>, so the backward walk from <c>
        // must jump straight to <a>.
        assert_eq!(result, serde_json::json!(["A"]));
    }

    /// Issue #461: NodeIterator has no subtree pruning — DOM 6.2 says
    /// FILTER_REJECT behaves as FILTER_SKIP there. The shared walker must not
    /// Issue #475: parentNode() must never surface a node above `root`. With
    /// currentNode at root, the old guard stepped to root's own parent and
    /// returned it — escaping the walker's subtree entirely.
    #[test]
    fn tree_walker_parent_node_does_not_escape_above_root() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                const escaped = w.parentNode();
                return [escaped, w.currentNode.id];
                "#,
            )
            .unwrap();
        // No parent within the subtree, and currentNode stays put at root.
        assert_eq!(result, serde_json::json!([null, "root"]));
    }

    /// Issue #475: when the accepted ancestor is `root` itself, parentNode()
    /// returns it and moves currentNode there — the old `parent !== root` guard
    /// wrongly excluded it.
    #[test]
    fn tree_walker_parent_node_can_return_the_root() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                w.currentNode = root.querySelector('a');
                const p = w.parentNode();
                return [p ? p.id : null, w.currentNode === root];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["root", true]));
    }

    /// Issue #475: parentNode() climbs past a skipped ancestor to the first
    /// accepted one, instead of stopping at the immediate parent.
    #[test]
    fn tree_walker_parent_node_climbs_past_skipped_ancestors() {
        let mut rt =
            setup_runtime(r#"<div id="root"><main id="m"><section><a></a></section></main></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(n) {
                        return n.tagName === 'SECTION'
                            ? NodeFilter.FILTER_SKIP
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                w.currentNode = root.querySelector('a');
                const p = w.parentNode();
                return p ? p.id : null;
                "#,
            )
            .unwrap();
        // <a>'s parent <section> is skipped, so <main> is the first accepted
        // ancestor — not null, and not the immediate <section>.
        assert_eq!(result, serde_json::json!("m"));
    }

    /// leak TreeWalker's pruning into it.
    #[test]
    fn node_iterator_treats_filter_reject_as_skip() {
        let mut rt = setup_runtime(r#"<div id="root"><section><p>deep</p></section><a></a></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'SECTION'
                            ? NodeFilter.FILTER_REJECT
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                const seen = [];
                let node;
                while ((node = it.nextNode())) seen.push(node.tagName);
                return seen;
                "#,
            )
            .unwrap();
        // The rejected <section> is skipped but not pruned, so <p> still shows.
        // The leading root is #467: an iterator yields the node it is rooted at.
        assert_eq!(result, serde_json::json!(["DIV", "P", "A"]));
    }

    /// Issue #467: a NodeIterator starts *before* its root, so the first
    /// nextNode() returns the root itself. Aliasing createTreeWalker silently
    /// dropped exactly the element the iterator was rooted at.
    #[test]
    fn node_iterator_yields_the_root_node_first() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT);
                const seen = [];
                let node;
                while ((node = it.nextNode())) seen.push(node.tagName);
                return seen;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["DIV", "A"]));
    }

    /// Issue #467: the NodeIterator interface surface, and that TreeWalker-only
    /// members are not exposed on it.
    #[test]
    fn node_iterator_exposes_its_own_interface() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT);
                const before = [it.referenceNode === root, it.pointerBeforeReferenceNode];
                it.nextNode();
                return [
                    before,
                    typeof it.detach,
                    it.detach() === undefined,
                    typeof it.previousNode,
                    it.root === root,
                    it.whatToShow,
                    // TreeWalker-only members must not leak onto a NodeIterator.
                    typeof it.currentNode,
                    typeof it.firstChild,
                    typeof it.parentNode,
                    // The pointer advanced past the root it just returned.
                    [it.referenceNode.tagName, it.pointerBeforeReferenceNode],
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, true],
                "function",
                true,
                "function",
                true,
                1,
                "undefined",
                "undefined",
                "undefined",
                ["DIV", false]
            ])
        );
    }

    /// Issue #467: previousNode() retraces the iterator, and the root is the
    /// last node it yields going backwards.
    #[test]
    fn node_iterator_previous_node_retraces_the_walk() {
        let mut rt = setup_runtime(r#"<div id="root"><a><b></b></a><c></c></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT);
                const forward = [];
                let node;
                while ((node = it.nextNode())) forward.push(node.tagName);
                const backward = [];
                while ((node = it.previousNode())) backward.push(node.tagName);
                return [forward, backward];
                "#,
            )
            .unwrap();
        // Forward ends on <c>; going back re-yields <c> (the pointer sits after
        // it), then the rest in reverse, root included.
        assert_eq!(
            result,
            serde_json::json!([["DIV", "A", "B", "C"], ["C", "B", "A", "DIV"]])
        );
    }

    /// Issue #463: `<template>` contents are parsed into the node's
    /// `template_contents` document, but no op exposed it, so `.content` handed
    /// back a fabricated empty fragment and the parsed markup was unreachable.
    #[test]
    fn template_content_exposes_parsed_markup() {
        let mut rt = setup_runtime(
            r#"<body><template id="t"><p class="row">a</p><p class="row">b</p></template></body>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                return [
                    t.content.childNodes.length,
                    t.content.querySelectorAll('.row').length,
                    t.content.firstElementChild.textContent,
                    t.innerHTML,
                    t.content.nodeType,
                    t.content instanceof DocumentFragment,
                    // Identity is stable: frameworks stash `.content` and reuse it.
                    t.content === t.content,
                    // The children stay off the element itself, per the HTML spec.
                    t.childNodes.length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                2,
                2,
                "a",
                r#"<p class="row">a</p><p class="row">b</p>"#,
                11,
                true,
                true,
                0
            ])
        );
    }

    /// Setting innerHTML on the <html> element parses in the "before head"
    /// insertion mode, which synthesizes head and body. The importer must keep
    /// both; it previously returned the synthesized body and dropped the head
    /// (so a <title>/<meta> assigned this way vanished).
    #[test]
    fn documentelement_inner_html_keeps_head_and_body() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){ document.documentElement.innerHTML = '<head><title>T</title></head><body><p>hi</p></body>'; \
                 var t = document.querySelector('title'); var p = document.querySelector('p'); \
                 return (t ? t.textContent : 'no-title') + '|' + (p ? p.textContent : 'no-p'); })()",
            )
            .unwrap();
        assert_eq!(v, serde_json::json!("T|hi"));
    }

    /// Regression guard: innerHTML on an ordinary element still imports the
    /// parsed nodes directly (no head/body is synthesized for a div context),
    /// so the fix above must not change the common case.
    #[test]
    fn ordinary_element_inner_html_imports_content_directly() {
        let mut rt = setup_runtime("<html><body><div id=\"d\"></div></body></html>");
        let v = rt
            .evaluate(
                "(function(){ var d=document.getElementById('d'); d.innerHTML='<span>a</span><span>b</span>'; \
                 return d.children.length + '|' + d.textContent; })()",
            )
            .unwrap();
        assert_eq!(v, serde_json::json!("2|ab"));
    }

    /// Issue #463: the same must hold for a template that arrives via innerHTML
    /// rather than the initial document parse — that is how most frameworks
    /// inject templates.
    #[test]
    fn template_content_works_for_templates_added_via_inner_html() {
        let mut rt = setup_runtime(r#"<body><div id="host"></div></body>"#);
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                host.innerHTML = '<template id="t2"><li class="item">x</li></template>';
                const t = document.getElementById('t2');
                const stamped = t.content.cloneNode(true);
                host.appendChild(stamped);
                return [
                    t.content.childNodes.length,
                    t.content.querySelector('.item').textContent,
                    host.querySelectorAll('li.item').length,
                ];
                "#,
            )
            .unwrap();
        // cloneNode(true) of the content is the canonical stamping idiom.
        assert_eq!(result, serde_json::json!([1, "x", 1]));
    }

    /// Issue #463: a template built with createElement has no parsed contents,
    /// so `.content` must allocate a backing fragment on demand and round-trip
    /// through innerHTML.
    #[test]
    fn template_content_round_trips_for_created_templates() {
        let mut rt = setup_runtime(r#"<body></body>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.createElement('template');
                t.innerHTML = '<span class="s">hi</span>';
                return [
                    t.content.childNodes.length,
                    t.content.querySelector('.s').textContent,
                    t.innerHTML,
                    t.childNodes.length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([1, "hi", r#"<span class="s">hi</span>"#, 0])
        );
    }

    /// Issue #463: serializing a `<template>` must emit its contents, or the
    /// markup silently disappears from outerHTML/innerHTML round-trips — and
    /// `cloneNode(true)`, which round-trips through outer_html, yields an empty
    /// template.
    #[test]
    fn template_contents_survive_serialization_and_clone() {
        let mut rt =
            setup_runtime(r#"<body><template id="t"><li class="item">x</li></template></body>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                const clone = t.cloneNode(true);
                return [
                    t.outerHTML,
                    document.body.innerHTML,
                    clone.content.childNodes.length,
                    clone.content.querySelector('.item').textContent,
                    // The clone's contents are its own, not shared with the original.
                    (clone.content.firstElementChild === t.content.firstElementChild),
                ];
                "#,
            )
            .unwrap();
        let expected = r#"<template id="t"><li class="item">x</li></template>"#;
        assert_eq!(
            result,
            serde_json::json!([expected, expected, 1, "x", false])
        );
    }

    /// Issue #468: window.scrollTo/scrollBy/scroll were no-op stubs, so the
    /// dominant infinite-scroll idiom never advanced the page offset.
    #[cfg(not(feature = "render"))]
    #[test]
    fn window_scroll_methods_move_the_page_offset() {
        let mut rt = setup_runtime(r#"<html><body><div id="d"></div></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                const scrolled = window.scrollTo(0, 500);
                const afterTo = [window.scrollX, window.scrollY];
                window.scrollBy(0, 200);
                const afterBy = [window.pageXOffset, window.pageYOffset];
                window.scrollTo({ left: 10, top: 40 });
                const afterOptions = [window.scrollX, window.scrollY];
                window.scroll(5, 5);
                const afterScroll = [window.scrollX, window.scrollY];
                // Negative offsets clamp to 0, as they do for elements.
                window.scrollTo(0, -100);
                return [afterTo, afterBy, afterOptions, afterScroll, window.scrollY];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([[0, 500], [0, 700], [10, 40], [5, 5], 0])
        );
    }

    /// Issue #468: the page offset is one value, readable and writable through
    /// either `window.scrollY` or `document.scrollingElement.scrollTop`.
    #[cfg(not(feature = "render"))]
    #[test]
    fn window_scroll_offset_is_shared_with_the_scrolling_element() {
        let mut rt = setup_runtime(r#"<html><body><div id="d"></div></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                const isDocEl = document.scrollingElement === document.documentElement;
                window.scrollTo(0, 300);
                // Written through the window, read through the element...
                const viaElement = document.scrollingElement.scrollTop;
                // ...and the reverse.
                document.scrollingElement.scrollTop = 90;
                return [isDocEl, viaElement, window.scrollY, window.pageYOffset];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, 300, 90, 90]));
    }

    /// Issue #468: a scroll event must reach listeners on both the window and
    /// the document — that is the signal lazy loaders wait for.
    #[cfg(not(feature = "render"))]
    #[tokio::test(flavor = "current_thread")]
    async fn window_scroll_fires_a_scroll_event() {
        let mut rt = setup_runtime(r#"<html><body><div id="d"></div></body></html>"#);
        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener('scroll', () => win++);
                    document.addEventListener('scroll', () => doc++);
                    window.scrollBy(0, 400);
                    setTimeout(() => resolve([win, doc, window.scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!([1, 1, 400]));
    }

    #[cfg(feature = "render")]
    #[test]
    fn rendered_window_scroll_clamps_and_geometry_is_viewport_relative() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="wide" style="width:600px;height:700px"></div>
                <div id="target" style="width:20px;height:300px"></div>
                <div id="fixed" style="position:fixed;left:12px;top:14px;width:30px;height:25px">
                    <span id="fixed-child">fixed</span>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const target = document.getElementById("target");
                const fixed = document.getElementById("fixed");
                const fixedChild = document.getElementById("fixed-child");
                const before = {
                    target: target.getBoundingClientRect(),
                    fixed: fixed.getBoundingClientRect(),
                    fixedChild: fixedChild.getBoundingClientRect(),
                };
                window.scrollTo(99999, 99999);
                const maxX = document.documentElement.scrollWidth - innerWidth;
                const maxY = document.documentElement.scrollHeight - innerHeight;
                const after = {
                    target: target.getBoundingClientRect(),
                    fixed: fixed.getBoundingClientRect(),
                    fixedChild: fixedChild.getBoundingClientRect(),
                };
                return [
                    innerWidth, innerHeight,
                    document.documentElement.clientWidth,
                    document.documentElement.clientHeight,
                    document.documentElement.scrollWidth,
                    document.documentElement.scrollHeight,
                    window.scrollX, window.scrollY,
                    document.scrollingElement.scrollLeft,
                    document.scrollingElement.scrollTop,
                    maxX, maxY,
                    Math.abs(after.target.left - (before.target.left - maxX)) < 0.01,
                    Math.abs(after.target.top - (before.target.top - maxY)) < 0.01,
                    Math.abs(after.fixed.left - before.fixed.left) < 0.01,
                    Math.abs(after.fixed.top - before.fixed.top) < 0.01,
                    Math.abs(after.fixedChild.left - before.fixedChild.left) < 0.01,
                    Math.abs(after.fixedChild.top - before.fixedChild.top) < 0.01,
                ];
                "#,
            )
            .unwrap();
        let values = result.as_array().expect("array");
        assert_eq!(
            &values[0..4],
            &serde_json::json!([320, 200, 320, 200]).as_array().unwrap()[..]
        );
        let scroll_width = values[4].as_f64().expect("scrollWidth");
        let scroll_height = values[5].as_f64().expect("scrollHeight");
        assert!(scroll_width >= 600.0, "scrollWidth was {scroll_width}");
        assert!(scroll_height >= 1000.0, "scrollHeight was {scroll_height}");
        assert_eq!(values[6], values[10]);
        assert_eq!(values[7], values[11]);
        assert_eq!(values[8], values[10]);
        assert_eq!(values[9], values[11]);
        assert!(values[12..]
            .iter()
            .all(|value| value == &serde_json::json!(true)));
    }

    #[cfg(feature = "render")]
    #[test]
    fn nested_scroll_metrics_geometry_pixels_and_relayout_share_one_state() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="outer" style="box-sizing:border-box;width:120px;height:100px;
                     border:4px solid red;overflow:hidden;position:relative;background:red">
                  <div id="inner" style="width:220px;height:200px;overflow:hidden;
                       position:relative;background:blue">
                    <div id="target" style="position:absolute;left:300px;top:280px;
                         width:30px;height:20px;background:lime"></div>
                  </div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(360.0, 240.0);
        rt.run_page_init();

        let top = rt
            .screenshot_prepared((360.0, 240.0), Some("about:blank"))
            .expect("top screenshot");
        let result = rt
            .evaluate(
                r#"
                const outer = document.getElementById('outer');
                const inner = document.getElementById('inner');
                const target = document.getElementById('target');
                const before = {
                  outer: outer.getBoundingClientRect(),
                  inner: inner.getBoundingClientRect(),
                  target: target.getBoundingClientRect(),
                };
                outer.scrollTo(9999, 9999);
                inner.scrollTo(9999, 9999);
                const after = {
                  outer: outer.getBoundingClientRect(),
                  inner: inner.getBoundingClientRect(),
                  target: target.getBoundingClientRect(),
                };
                const first = [target.getBoundingClientRect().left, target.getBoundingClientRect().top];
                outer.scrollTo(0, 0); inner.scrollTo(0, 0);
                outer.scrollTo(9999, 9999); inner.scrollTo(9999, 9999);
                const repeated = [target.getBoundingClientRect().left, target.getBoundingClientRect().top];
                return {
                  outerMetrics: [outer.clientWidth, outer.clientHeight, outer.scrollWidth, outer.scrollHeight],
                  innerMetrics: [inner.clientWidth, inner.clientHeight, inner.scrollWidth, inner.scrollHeight],
                  offsets: [outer.scrollLeft, outer.scrollTop, inner.scrollLeft, inner.scrollTop],
                  outerDelta: [after.outer.left - before.outer.left, after.outer.top - before.outer.top],
                  innerDelta: [after.inner.left - before.inner.left, after.inner.top - before.inner.top],
                  targetDelta: [after.target.left - before.target.left, after.target.top - before.target.top],
                  repeated: [first[0] === repeated[0], first[1] === repeated[1]],
                };
                "#,
            )
            .expect("nested scroll state");
        assert_eq!(
            result["outerMetrics"],
            serde_json::json!([112, 92, 220, 200])
        );
        assert_eq!(
            result["innerMetrics"],
            serde_json::json!([220, 200, 330, 300])
        );
        assert_eq!(result["offsets"], serde_json::json!([108, 108, 110, 100]));
        assert_eq!(result["outerDelta"], serde_json::json!([0, 0]));
        assert_eq!(result["innerDelta"], serde_json::json!([-108, -108]));
        assert_eq!(result["targetDelta"], serde_json::json!([-218, -208]));
        assert_eq!(result["repeated"], serde_json::json!([true, true]));

        let scrolled = rt
            .screenshot_prepared((360.0, 240.0), Some("about:blank"))
            .expect("scrolled screenshot");
        let scrolled_repeat = rt
            .screenshot_prepared((360.0, 240.0), Some("about:blank"))
            .expect("repeat screenshot");
        assert_ne!(top, scrolled, "nested scroll must move painted pixels");
        assert_eq!(
            scrolled, scrolled_repeat,
            "capture must not accumulate movement"
        );

        let retained = rt
            .evaluate(
                r#"
                const outer = document.getElementById('outer');
                const inner = document.getElementById('inner');
                outer.setAttribute('data-relayout', '1');
                return [outer.scrollLeft, outer.scrollTop, inner.scrollLeft, inner.scrollTop];
                "#,
            )
            .expect("retained offsets");
        assert_eq!(retained, serde_json::json!([108, 108, 110, 100]));

        let reclamped = rt
            .evaluate(
                r#"
                const outer = document.getElementById('outer');
                const inner = document.getElementById('inner');
                inner.setAttribute('style', 'width:150px;height:120px;overflow:hidden;position:relative;background:blue');
                document.getElementById('target').setAttribute(
                  'style',
                  'position:absolute;left:100px;top:80px;width:30px;height:20px;background:lime'
                );
                return [outer.scrollLeft, outer.scrollTop, inner.scrollLeft, inner.scrollTop];
                "#,
            )
            .expect("reclamped offsets");
        assert_eq!(reclamped, serde_json::json!([38, 28, 0, 0]));

        rt.evaluate("(function(){ document.getElementById('outer').remove(); document.documentElement.getBoundingClientRect(); return true; })()")
            .expect("remove scroller");
        assert!(
            rt.state.borrow().element_scroll_offsets.is_empty(),
            "removed scroll containers must be pruned after relayout"
        );
    }

    /// Chromium 150 oracle for CSSOM scrolling overflow. Visible and clip
    /// boxes expose descendant overflow but cannot move; an actual scrolling
    /// box includes trailing padding. A clip boundary suppresses propagation
    /// only on its clipped axis, and ordinary inline boxes expose zero metrics.
    #[cfg(feature = "render")]
    #[test]
    fn element_scroll_metrics_match_chromium_overflow_oracles() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
              <style>
                .box { width:100px;height:80px;padding:10px;border:2px solid;position:absolute }
                .child { width:200px;height:150px }
              </style>
              <div id="visible" class="box" style="overflow:visible;top:0"><div class="child"></div></div>
              <div id="clip" class="box" style="overflow:clip;top:150px"><div class="child"></div></div>
              <div id="hidden" class="box" style="overflow:hidden;top:300px"><div class="child"></div></div>
              <div id="outer" style="width:100px;height:80px;overflow:visible;position:absolute;top:450px">
                <div id="axis" style="width:150px;height:120px;overflow-x:visible;overflow-y:clip">
                  <div style="width:300px;height:250px"></div>
                </div>
              </div>
              <div id="f1" style="width:10px;overflow:visible"><div style="width:100.1px;height:1px"></div></div>
              <div id="f2" style="width:10px;overflow:visible"><div style="width:100.6px;height:1px"></div></div>
              <span id="inline">long inline text</span>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(420.0, 700.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const visible = document.getElementById('visible');
                const clip = document.getElementById('clip');
                const hidden = document.getElementById('hidden');
                const outer = document.getElementById('outer');
                const axis = document.getElementById('axis');
                const inline = document.getElementById('inline');
                visible.scrollTo(99, 99);
                clip.scrollTo(99, 99);
                hidden.scrollTo(99, 99);
                return {
                  visible: [visible.scrollWidth, visible.scrollHeight, visible.scrollLeft, visible.scrollTop],
                  clip: [clip.scrollWidth, clip.scrollHeight, clip.scrollLeft, clip.scrollTop],
                  hidden: [hidden.scrollWidth, hidden.scrollHeight, hidden.scrollLeft, hidden.scrollTop],
                  axis: [outer.scrollWidth, outer.scrollHeight, axis.scrollWidth, axis.scrollHeight],
                  fractional: [document.getElementById('f1').scrollWidth, document.getElementById('f2').scrollWidth],
                  inline: [inline.scrollWidth, inline.scrollHeight, inline.clientWidth, inline.clientHeight],
                };
                "#,
            )
            .expect("overflow oracle metrics");
        assert_eq!(result["visible"], serde_json::json!([210, 160, 0, 0]));
        assert_eq!(result["clip"], serde_json::json!([210, 160, 0, 0]));
        assert_eq!(result["hidden"], serde_json::json!([220, 170, 99, 70]));
        assert_eq!(result["axis"], serde_json::json!([300, 120, 300, 250]));
        assert_eq!(result["fractional"], serde_json::json!([100, 101]));
        assert_eq!(result["inline"], serde_json::json!([0, 0, 0, 0]));
    }

    /// Chromium quantizes effective scrolling ranges and assigned offsets to
    /// the current device-pixel grid. At the renderer's present 1x scale a
    /// 100.4px area cannot move a 100px scrollport, while 100.6px rounds to a
    /// one-pixel range and assigning `.5` moves geometry and paint by 1px.
    #[cfg(feature = "render")]
    #[test]
    fn fractional_scroll_ranges_quantize_geometry_and_pixels_at_one_x() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
              <div id="low" style="width:100px;height:40px;overflow:auto;position:absolute;top:0">
                <div style="width:100.4px;height:40px;position:relative;background:white">
                  <div id="lowChild" style="position:absolute;left:40px;top:5px;width:10px;height:25px;background:red"></div>
                </div>
              </div>
              <div id="high" style="width:100px;height:40px;overflow:auto;position:absolute;top:60px">
                <div style="width:100.6px;height:40px;position:relative;background:white">
                  <div id="highChild" style="position:absolute;left:40px;top:5px;width:10px;height:25px;background:blue"></div>
                </div>
              </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(160.0, 120.0);
        rt.run_page_init();

        let initial = rt
            .screenshot_prepared((160.0, 120.0), Some("about:blank"))
            .expect("initial fractional screenshot");
        let low = rt
            .evaluate(
                r#"
                const low = document.getElementById('low');
                const child = document.getElementById('lowChild');
                const before = child.getBoundingClientRect();
                low.scrollLeft = 999;
                const after = child.getBoundingClientRect();
                return [low.scrollWidth, low.clientWidth, low.scrollLeft, after.left - before.left];
                "#,
            )
            .expect("low fractional range");
        assert_eq!(low, serde_json::json!([100, 100, 0, 0]));
        let after_low = rt
            .screenshot_prepared((160.0, 120.0), Some("about:blank"))
            .expect("low fractional screenshot");
        assert_eq!(
            initial, after_low,
            "a rounded-zero range cannot move pixels"
        );

        let high = rt
            .evaluate(
                r#"
                const high = document.getElementById('high');
                const child = document.getElementById('highChild');
                const before = child.getBoundingClientRect();
                high.scrollLeft = .5;
                const after = child.getBoundingClientRect();
                return [high.scrollWidth, high.clientWidth, high.scrollLeft, after.left - before.left];
                "#,
            )
            .expect("high fractional range");
        assert_eq!(high, serde_json::json!([101, 100, 1, -1]));
        let after_high = rt
            .screenshot_prepared((160.0, 120.0), Some("about:blank"))
            .expect("high fractional screenshot");
        assert_ne!(after_low, after_high, "the quantized pixel must repaint");

        let root_dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                 <div id="wide" style="width:100.6px;height:20px"></div>
               </body></html>"#,
        );
        let mut root_rt = ObscuraJsRuntime::new();
        root_rt.set_dom(root_dom);
        root_rt.set_viewport(100.0, 60.0);
        root_rt.run_page_init();
        let root = root_rt
            .evaluate(
                r#"
                const wide = document.getElementById('wide');
                const before = wide.getBoundingClientRect();
                window.scrollTo(.5, 0);
                const after = wide.getBoundingClientRect();
                const high = [document.documentElement.scrollWidth, window.scrollX, after.left - before.left];
                wide.style.width = '100.4px';
                window.scrollTo(999, 0);
                return { high, low: [document.documentElement.scrollWidth, window.scrollX] };
                "#,
            )
            .expect("root fractional range");
        assert_eq!(root["high"], serde_json::json!([101, 1, -1]));
        assert_eq!(root["low"], serde_json::json!([100, 0]));
    }

    #[cfg(feature = "render")]
    #[test]
    fn element_scroll_offsets_follow_chromium_box_and_dom_lifecycles() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
              <div id="first"><div id="scroller" style="width:100px;height:80px;overflow:auto">
                <div style="width:250px;height:200px"></div>
              </div></div>
              <div id="second"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(360.0, 240.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const scroller = document.getElementById('scroller');
                const first = document.getElementById('first');
                const second = document.getElementById('second');
                scroller.scrollTo(70, 60);
                const initial = [scroller.scrollLeft, scroller.scrollTop];
                scroller.style.overflow = 'visible';
                const visible = [scroller.scrollLeft, scroller.scrollTop];
                scroller.style.overflow = 'auto';
                const restoredStyle = [scroller.scrollLeft, scroller.scrollTop];
                scroller.style.display = 'none';
                const noBox = [scroller.scrollWidth, scroller.scrollHeight, scroller.scrollLeft, scroller.scrollTop];
                scroller.scrollTo(5, 5);
                scroller.style.display = 'block';
                const restoredDisplay = [scroller.scrollLeft, scroller.scrollTop];
                second.appendChild(scroller);
                const moved = [scroller.scrollLeft, scroller.scrollTop];
                scroller.scrollTo(40, 30);
                second.removeChild(scroller);
                first.appendChild(scroller);
                const reattached = [scroller.scrollLeft, scroller.scrollTop];
                scroller.scrollTo(20, 10);
                first.textContent = 'replacement';
                document.body.appendChild(scroller);
                const textReplacement = [scroller.scrollLeft, scroller.scrollTop];
                const detached = document.createElement('div');
                detached.style.cssText = 'width:100px;height:80px;overflow:auto';
                let recomputes = 0;
                globalThis.__obscura_recompute_intersections = () => { recomputes++; };
                detached.scrollTo(30, 20);
                scroller.scrollTo(11, 12);
                return {
                  initial, visible, restoredStyle, noBox, restoredDisplay,
                  moved, reattached, textReplacement,
                  detached: [detached.scrollWidth, detached.scrollHeight, detached.scrollLeft, detached.scrollTop],
                  atomic: [scroller.scrollLeft, scroller.scrollTop, recomputes],
                };
                "#,
            )
            .expect("scroll lifecycle state");
        assert_eq!(result["initial"], serde_json::json!([70, 60]));
        assert_eq!(result["visible"], serde_json::json!([0, 0]));
        assert_eq!(result["restoredStyle"], serde_json::json!([70, 60]));
        assert_eq!(result["noBox"], serde_json::json!([0, 0, 0, 0]));
        assert_eq!(result["restoredDisplay"], serde_json::json!([70, 60]));
        assert_eq!(result["moved"], serde_json::json!([0, 0]));
        assert_eq!(result["reattached"], serde_json::json!([0, 0]));
        assert_eq!(result["textReplacement"], serde_json::json!([0, 0]));
        assert_eq!(result["detached"], serde_json::json!([0, 0, 0, 0]));
        assert_eq!(result["atomic"], serde_json::json!([11, 12, 1]));
    }

    #[cfg(feature = "render")]
    #[test]
    fn fixed_panels_scroll_locally_and_transformed_descendants_remain_supported() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:1800px">
              <div id="modal" style="position:fixed;left:20px;top:20px;width:140px;height:120px;background:red">
                <div id="panel" style="width:100px;height:80px;overflow:hidden;position:relative;background:blue">
                  <div id="fixedTarget" style="position:absolute;left:180px;top:160px;width:20px;height:20px;background:lime"></div>
                </div>
              </div>
              <div id="transformedScroller" style="position:absolute;top:300px;width:100px;height:80px;overflow:hidden">
                <div id="transformedTarget" style="width:240px;height:180px;transform:scale(1.1)"></div>
              </div>
              <div style="position:absolute;top:600px;transform:scale(1.2)">
                <div id="affineAncestorScroller" style="width:100px;height:80px;overflow:hidden">
                  <div style="width:240px;height:180px"></div>
                </div>
              </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(360.0, 240.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const modal = document.getElementById('modal');
                const panel = document.getElementById('panel');
                const fixedTarget = document.getElementById('fixedTarget');
                const transformedScroller = document.getElementById('transformedScroller');
                const transformedTarget = document.getElementById('transformedTarget');
                const affineAncestorScroller = document.getElementById('affineAncestorScroller');
                const before = {
                  modal: modal.getBoundingClientRect(),
                  fixedTarget: fixedTarget.getBoundingClientRect(),
                  transformedTarget: transformedTarget.getBoundingClientRect(),
                };
                panel.scrollTo(60, 50);
                transformedScroller.scrollTo(50, 40);
                affineAncestorScroller.scrollTo(50, 40);
                window.scrollTo(0, 500);
                const after = {
                  modal: modal.getBoundingClientRect(),
                  fixedTarget: fixedTarget.getBoundingClientRect(),
                  transformedTarget: transformedTarget.getBoundingClientRect(),
                };
                return {
                  modalDelta: [after.modal.left - before.modal.left, after.modal.top - before.modal.top],
                  fixedDelta: [after.fixedTarget.left - before.fixedTarget.left, after.fixedTarget.top - before.fixedTarget.top],
                  transformedDelta: [after.transformedTarget.left - before.transformedTarget.left, after.transformedTarget.top - before.transformedTarget.top],
                  offsets: [panel.scrollLeft, panel.scrollTop, transformedScroller.scrollLeft, transformedScroller.scrollTop, affineAncestorScroller.scrollLeft, affineAncestorScroller.scrollTop],
                };
                "#,
            )
            .expect("fixed and transformed scroll state");
        assert_eq!(result["modalDelta"], serde_json::json!([0, 0]));
        assert_eq!(result["fixedDelta"], serde_json::json!([-60, -50]));
        assert_eq!(result["transformedDelta"], serde_json::json!([-50, -540]));
        assert_eq!(result["offsets"], serde_json::json!([60, 50, 50, 40, 0, 0]));
    }

    /// CSSOM View exposes the viewport through the standards-mode root, but
    /// ordinary elements (including body) report their padding box. Modern
    /// animation libraries commonly measure a fixed 100vh sentinel through
    /// clientHeight; the old synthetic 100x20 fallback collapsed all of their
    /// viewport-relative trigger ranges.
    #[cfg(feature = "render")]
    #[test]
    fn rendered_client_metrics_use_the_live_padding_box() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="tracker"
                     style="position:fixed;top:0;width:100%;height:100vh"></div>
                <div id="box"
                     style="box-sizing:content-box;width:100.4px;height:50.6px;
                            padding:5px 8.2px 6px 7.2px;
                            border-style:solid;
                            border-width:2px 4.1px 3px 3.1px"></div>
                <div style="height:900px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let initial = rt
            .evaluate(
                r#"
                const tracker = document.getElementById("tracker");
                const box = document.getElementById("box");
                return {
                    root: [
                        document.documentElement.clientWidth,
                        document.documentElement.clientHeight
                    ],
                    body: [document.body.clientWidth, document.body.clientHeight],
                    tracker: [
                        tracker.clientWidth, tracker.clientHeight,
                        tracker.offsetWidth, tracker.offsetHeight
                    ],
                    box: [
                        box.clientWidth, box.clientHeight,
                        box.getBoundingClientRect().width,
                        box.getBoundingClientRect().height
                    ]
                };
                "#,
            )
            .unwrap();
        assert_eq!(initial["root"], serde_json::json!([320, 200]));
        assert_eq!(initial["body"], serde_json::json!([320, 967]));
        assert_eq!(initial["tracker"], serde_json::json!([320, 200, 320, 200]));
        assert_eq!(initial["box"][0], serde_json::json!(116));
        assert_eq!(initial["box"][1], serde_json::json!(62));
        assert!((initial["box"][2].as_f64().unwrap() - 123.0).abs() < 0.05);
        assert_eq!(initial["box"][3], serde_json::json!(67));

        // Attribute-backed inline-style changes invalidate the retained
        // render. Borders do not change the padding box; padding does.
        let mutated = rt
            .evaluate(
                r#"
                const tracker = document.getElementById("tracker");
                const box = document.getElementById("box");
                tracker.style.height = "50vh";
                box.style.borderLeftWidth = "13px";
                box.style.paddingLeft = "17px";
                return [
                    tracker.clientHeight,
                    box.clientWidth,
                    box.getBoundingClientRect().width
                ];
                "#,
            )
            .unwrap();
        assert_eq!(mutated[0], serde_json::json!(100));
        assert_eq!(mutated[1], serde_json::json!(126));
        assert_eq!(mutated[2], serde_json::json!(143));

        // A later CDP/emulation viewport update invalidates the layout too;
        // both the root special case and an ordinary 100vh box are live.
        rt.set_viewport(640.0, 360.0);
        assert_eq!(
            rt.evaluate(
                r#"const tracker = document.getElementById("tracker");
                return [
                    document.documentElement.clientWidth,
                    document.documentElement.clientHeight,
                    tracker.clientWidth,
                    tracker.clientHeight
                ]"#,
            )
            .unwrap(),
            serde_json::json!([640, 360, 640, 180])
        );
    }

    /// CSSOM View distinguishes "no associated CSS box" from a real box whose
    /// dimensions happen to be zero. Blink and Gecko return an all-zero
    /// bounding rect and no client rects for display:none/detached elements;
    /// a laid-out zero-size box still contributes one client rect.
    #[cfg(feature = "render")]
    #[test]
    fn rendered_cssom_rects_distinguish_no_box_from_zero_size_box() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="hidden" style="display:none;width:80px;height:40px"></div>
                <div id="zero" style="display:block;width:0;height:0"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const hidden = document.getElementById("hidden");
                const detached = document.createElement("div");
                detached.style.cssText = "display:block;width:90px;height:50px";
                const zero = document.getElementById("zero");
                const sample = element => {
                    const rect = element.getBoundingClientRect();
                    const rects = element.getClientRects();
                    return {
                        rect: [
                            rect.x, rect.y, rect.width, rect.height,
                            rect.top, rect.right, rect.bottom, rect.left
                        ],
                        rectCount: rects.length,
                        firstWidth: rects.length ? rects[0].width : null,
                    };
                };
                return {
                    hidden: sample(hidden),
                    detached: sample(detached),
                    zero: sample(zero),
                };
                "#,
            )
            .unwrap();

        for name in ["hidden", "detached"] {
            assert_eq!(
                result[name]["rect"],
                serde_json::json!([0, 0, 0, 0, 0, 0, 0, 0]),
                "{name} must expose the CSSOM View no-box bounding rect"
            );
            assert_eq!(
                result[name]["rectCount"],
                serde_json::json!(0),
                "{name} must expose an empty client rect list"
            );
            assert_eq!(result[name]["firstWidth"], serde_json::Value::Null);
        }
        assert_eq!(result["zero"]["rect"][2], serde_json::json!(0));
        assert_eq!(result["zero"]["rect"][3], serde_json::json!(0));
        assert_eq!(
            result["zero"]["rectCount"],
            serde_json::json!(1),
            "a real zero-size layout box must not be mistaken for no box"
        );
        assert_eq!(result["zero"]["firstWidth"], serde_json::json!(0));
    }

    #[cfg(not(feature = "render"))]
    #[test]
    fn non_render_cssom_rects_keep_compatibility_geometry() {
        let mut rt = setup_runtime(r#"<html><body><div id="box"></div></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const detached = document.createElement("div");
                return [box, detached].map(element => {
                    const rect = element.getBoundingClientRect();
                    return [rect.width, rect.height, element.getClientRects().length];
                });
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([[100, 20, 1], [100, 20, 1]]));
    }

    /// Chromium 150 reference (800x513 CSS-pixel viewport):
    /// top=[60,20,20,-267], bottom=[448,448,231,31,-496] at the sampled
    /// root scroll offsets. This keeps sticky distinct from fixed positioning,
    /// verifies subtree movement, bottom-only sticking, and the containing
    /// block's lower boundary without depending on a live site.
    #[cfg(feature = "render")]
    #[test]
    fn root_scroll_sticky_geometry_matches_chromium_constraints() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:40px"></div>
                <div id="cb" style="box-sizing:border-box;height:900px;padding:10px 12px;border:4px solid #333">
                    <div id="top" style="box-sizing:border-box;position:sticky;top:20px;height:60px;margin:6px">
                        <div id="top-child" style="height:12px"></div>
                    </div>
                    <div style="height:500px"></div>
                    <div id="bottom" style="box-sizing:border-box;position:sticky;bottom:15px;height:50px;margin:5px"></div>
                </div>
                <div style="height:700px"></div>
                <div id="fixed" style="position:fixed;left:600px;top:20px;width:60px;height:60px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(800.0, 513.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const top = document.getElementById("top");
                const child = document.getElementById("top-child");
                const bottom = document.getElementById("bottom");
                const fixed = document.getElementById("fixed");
                const sample = y => {
                    window.scrollTo(0, y);
                    return [
                        window.scrollY,
                        top.getBoundingClientRect().top,
                        child.getBoundingClientRect().top,
                        bottom.getBoundingClientRect().top,
                        fixed.getBoundingClientRect().top,
                    ];
                };
                return [sample(0), sample(100), sample(400), sample(600), sample(9999)];
                "#,
            )
            .unwrap();
        let rows = result.as_array().expect("rows");
        let number =
            |row: usize, column: usize| rows[row].as_array().unwrap()[column].as_f64().unwrap();
        let close = |actual: f64, expected: f64| {
            assert!(
                (actual - expected).abs() < 0.05,
                "expected {expected}, got {actual}"
            );
        };

        close(number(0, 1), 60.0);
        close(number(0, 3), 448.0);
        close(number(1, 1), 20.0);
        close(number(1, 2), 20.0);
        close(number(1, 3), 448.0);
        close(number(2, 1), 20.0);
        close(number(2, 3), 231.0);
        close(number(3, 1), 20.0);
        close(number(3, 3), 31.0);
        close(number(4, 0), 1127.0);
        close(number(4, 1), -267.0);
        close(number(4, 2), -267.0);
        close(number(4, 3), -496.0);
        for row in 0..rows.len() {
            close(number(row, 4), 20.0);
        }
    }

    /// Chromium 150 horizontal reference for the same constraint algorithm:
    /// the sticky subtree pins at x=20, remains distinct from fixed, then
    /// leaves with its 500px containing block at the right boundary.
    #[cfg(feature = "render")]
    #[test]
    fn root_scroll_sticky_supports_the_inline_axis() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="box-sizing:border-box;margin-left:40px;width:500px;height:100px;padding:10px;border:4px solid">
                    <div id="sticky" style="box-sizing:border-box;position:sticky;left:20px;width:60px;height:30px;margin:6px">
                        <div id="child" style="width:10px;height:10px"></div>
                    </div>
                </div>
                <div style="width:1600px;height:600px"></div>
                <div id="fixed" style="position:fixed;left:20px;top:100px;width:60px;height:30px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(800.0, 513.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const sticky = document.getElementById("sticky");
                const child = document.getElementById("child");
                const fixed = document.getElementById("fixed");
                const sample = x => {
                    window.scrollTo(x, 0);
                    return [
                        window.scrollX,
                        sticky.getBoundingClientRect().left,
                        child.getBoundingClientRect().left,
                        fixed.getBoundingClientRect().left,
                    ];
                };
                return [sample(0), sample(100), sample(400), sample(800)];
                "#,
            )
            .unwrap();
        let rows = result.as_array().unwrap();
        let expected = [
            [0.0, 60.0, 60.0, 20.0],
            [100.0, 20.0, 20.0, 20.0],
            [400.0, 20.0, 20.0, 20.0],
            [800.0, -340.0, -340.0, 20.0],
        ];
        for (row, expected) in rows.iter().zip(expected) {
            for (actual, expected) in row.as_array().unwrap().iter().zip(expected) {
                let actual = actual.as_f64().unwrap();
                assert!(
                    (actual - expected).abs() < 0.05,
                    "expected {expected}, got {actual}"
                );
            }
        }
    }

    #[cfg(feature = "render")]
    #[test]
    fn prepared_render_shares_resource_geometry_with_cssom_and_screenshots() {
        let dom = parse_html(
            r#"<html style="margin:0"><head>
                <base href="/assets/">
            </head><body style="margin:0">
                <div id="frame" style="width:160px">
                    <img id="hero" src="hero.svg" style="display:block;width:100%;height:auto">
                </div>
                <div style="height:400px;background:#0000ff"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/docs/page");
        rt.set_viewport(200.0, 100.0);

        let loads = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let loader_loads = std::sync::Arc::clone(&loads);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(move |url: &str| {
                assert_eq!(url, "http://example.test/assets/hero.svg");
                *loader_loads.lock().expect("loader count") += 1;
                Some(
                    br##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="100">
                        <rect width="400" height="100" fill="#ffff00"/>
                    </svg>"##
                        .to_vec(),
                )
            });
        rt.run_page_init();

        let before = rt
            .evaluate(
                r#"
                const hero = document.getElementById("hero");
                const rect = hero.getBoundingClientRect();
                return [rect.width, rect.height, document.documentElement.scrollHeight];
                "#,
            )
            .expect("initial geometry");
        let before = before.as_array().expect("initial tuple");
        assert_eq!(before[0].as_f64(), Some(160.0));
        assert_eq!(before[1].as_f64(), Some(40.0));
        let cssom_height = before[2].as_f64().expect("scroll height") as f32;
        let (prepared_address, prepared_height) = {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("prepared by CSSOM");
            (
                prepared as *const obscura_render::PreparedRender as usize,
                prepared.content_size().1,
            )
        };
        assert_eq!(cssom_height, prepared_height);
        assert_eq!(*loads.lock().expect("prepare load count"), 1);

        let base_url = Some("http://example.test/assets/");
        let top = rt
            .screenshot_prepared((200.0, 100.0), base_url)
            .expect("top screenshot");
        rt.evaluate(
            "(function(){ window.scrollTo(0, document.documentElement.scrollHeight); return window.scrollY; })()",
        )
        .expect("scroll to bottom");
        let bottom = rt
            .screenshot_prepared((200.0, 100.0), base_url)
            .expect("bottom screenshot");
        let bottom_repeat = rt
            .screenshot_prepared((200.0, 100.0), base_url)
            .expect("repeated bottom screenshot");
        assert_ne!(top, bottom);
        assert_eq!(bottom, bottom_repeat);
        {
            let state = rt.state.borrow();
            let prepared = state
                .prepared_render
                .as_ref()
                .expect("retained prepared render");
            assert_eq!(
                prepared as *const obscura_render::PreparedRender as usize, prepared_address,
                "screenshots must consume the CSSOM-prepared layout"
            );
            assert_eq!(prepared.content_size().1, cssom_height);
        }
        assert_eq!(*loads.lock().expect("paint load count"), 1);

        let after = rt
            .evaluate(
                r#"
                const hero = document.getElementById("hero");
                document.getElementById("frame").setAttribute("style", "width:80px");
                const rect = hero.getBoundingClientRect();
                return [rect.width, rect.height, document.documentElement.scrollHeight];
                "#,
            )
            .expect("mutated geometry");
        let after = after.as_array().expect("mutated tuple");
        assert_eq!(after[0].as_f64(), Some(80.0));
        assert_eq!(after[1].as_f64(), Some(20.0));
        let mutated_height = after[2].as_f64().expect("mutated scroll height") as f32;
        assert_eq!(
            rt.state
                .borrow()
                .prepared_render
                .as_ref()
                .expect("rebuilt prepared render")
                .content_size()
                .1,
            mutated_height
        );
        rt.screenshot_prepared((200.0, 100.0), base_url)
            .expect("mutated screenshot");
        assert_eq!(
            *loads.lock().expect("mutation load count"),
            1,
            "relayout must retain successful resource bytes"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn missing_render_resource_preserves_prepared_layout_and_scroll() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:240px">
                <div style="height:240px;background:blue"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();
        rt.evaluate("window.scrollTo(0, 40)")
            .expect("scroll fixture");
        let before_png = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("prepare retained render");
        let (prepared_address, resolved_address, scroll_generation, root_offset) = {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("prepared layout");
            let resolved = &state.resolved_scroll.as_ref().expect("resolved scroll").1;
            (
                prepared as *const obscura_render::PreparedRender as usize,
                resolved as *const obscura_render::ResolvedScrollState as usize,
                state.scroll_generation,
                resolved.root_offset(),
            )
        };

        let missing_url = "http://example.test/missing.svg".to_string();
        rt.seed_render_resource(missing_url.clone(), None);
        assert!(rt.render_resource_is_known(&missing_url));
        {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("retained layout");
            let resolved = &state.resolved_scroll.as_ref().expect("retained scroll").1;
            assert_eq!(
                prepared as *const obscura_render::PreparedRender as usize, prepared_address,
                "negative cache entries cannot change intrinsic geometry"
            );
            assert_eq!(
                resolved as *const obscura_render::ResolvedScrollState as usize, resolved_address,
                "negative cache entries must retain resolved scrolling"
            );
            assert_eq!(state.scroll_generation, scroll_generation);
            assert_eq!(resolved.root_offset(), root_offset);
        }
        assert_eq!(
            rt.screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
                .expect("capture retained render"),
            before_png,
        );

        rt.seed_render_resource(
            "http://example.test/loaded.svg".to_string(),
            Some(br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"/>"#.to_vec()),
        );
        let state = rt.state.borrow();
        let prepared = state.prepared_render.as_ref().expect("retained style graph");
        assert_eq!(
            prepared as *const obscura_render::PreparedRender as usize,
            prepared_address,
            "resource arrival waits for the next geometry flush"
        );
        assert_eq!(
            state.pending_style_mutations,
            vec![obscura_render::RetainedStyleMutation::Resource],
            "successful bytes queue one resource-dependent rebuild"
        );
        assert!(
            state.resolved_scroll.is_none(),
            "successful bytes invalidate scroll geometry"
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn image_resource_arrival_retains_styles_and_rebuilds_intrinsic_geometry() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <img id="hero" src="http://example.test/late.png" style="display:block">
                <div id="after" style="height:10px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();

        let before = rt
            .evaluate("[hero.getBoundingClientRect().height, after.getBoundingClientRect().top]")
            .expect("geometry before image arrival");
        assert_eq!(before, serde_json::json!([0, 0]));
        let prepared_address = {
            let state = rt.state.borrow();
            state
                .prepared_render
                .as_ref()
                .expect("initial prepared render") as *const obscura_render::PreparedRender
                as usize
        };

        // Preserve already queued framework damage and coalesce repeated
        // notification of the same shared resource into one refresh marker.
        rt.evaluate("after.setAttribute('data-ready', 'true')")
            .expect("queued DOM mutation");
        let png = two_by_three_png();
        rt.seed_render_image_resource(
            "http://example.test/late.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(png.clone()),
        );
        rt.seed_render_image_resource(
            "http://example.test/late.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(png),
        );
        {
            let state = rt.state.borrow();
            assert_eq!(
                state
                    .prepared_render
                    .as_ref()
                    .expect("style graph remains available")
                    as *const obscura_render::PreparedRender as usize,
                prepared_address,
            );
            assert_eq!(
                state
                    .pending_style_mutations
                    .iter()
                    .filter(|mutation| matches!(mutation, obscura_render::RetainedStyleMutation::Resource))
                    .count(),
                1,
            );
            assert!(state.pending_style_mutations.iter().any(|mutation| matches!(
                mutation,
                obscura_render::RetainedStyleMutation::Attribute(_)
            )));
        }

        let after = rt
            .evaluate("[hero.getBoundingClientRect().height, after.getBoundingClientRect().top]")
            .expect("geometry after image arrival");
        assert_eq!(after, serde_json::json!([3, 3]));
        assert!(rt.state.borrow().pending_style_mutations.is_empty());
    }

    #[cfg(feature = "render")]
    #[test]
    fn fixed_image_resource_arrival_repaints_without_rebuilding_geometry() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <img id="hero" src="http://example.test/fixed.png"
                     style="display:block;width:20px;height:10px">
                <div id="after" style="height:10px;background:blue"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("[hero.offsetWidth, hero.offsetHeight, after.offsetTop]")
                .expect("fixed geometry"),
            serde_json::json!([20, 10, 10])
        );
        let before_png = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("capture before image arrival");
        let (prepared_address, resolved_address, activity_before) = {
            let state = rt.state.borrow();
            (
                state.prepared_render.as_ref().unwrap() as *const _ as usize,
                &state.resolved_scroll.as_ref().unwrap().1 as *const _ as usize,
                state.activity_generation,
            )
        };

        rt.seed_render_image_resource(
            "http://example.test/fixed.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(two_by_three_png()),
        );
        {
            let state = rt.state.borrow();
            assert_eq!(
                state.prepared_render.as_ref().unwrap() as *const _ as usize,
                prepared_address,
                "fixed replaced content must keep the prepared geometry"
            );
            assert_eq!(
                &state.resolved_scroll.as_ref().unwrap().1 as *const _ as usize,
                resolved_address,
                "paint-only resource damage must keep resolved scrolling"
            );
            assert!(state.pending_style_mutations.is_empty());
            assert!(state.activity_generation > activity_before);
        }
        assert_eq!(
            rt.evaluate("[hero.offsetWidth, hero.offsetHeight, after.offsetTop]")
                .expect("retained fixed geometry"),
            serde_json::json!([20, 10, 10])
        );
        let after_png = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("capture after image arrival");
        assert_ne!(after_png, before_png, "new image pixels must reach paint");
    }

    #[cfg(feature = "render")]
    #[test]
    fn fixed_flex_image_resource_arrival_still_rebuilds_geometry() {
        let dom = parse_html(
            r#"<html><body><div style="display:flex">
                <img id="hero" src="http://example.test/flex.png"
                     style="width:20px;height:10px">
            </div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();
        rt.evaluate("hero.getBoundingClientRect().width")
            .expect("prepare flex geometry");
        assert!(rt.state.borrow().resolved_scroll.is_some());

        rt.seed_render_image_resource(
            "http://example.test/flex.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(two_by_three_png()),
        );
        let state = rt.state.borrow();
        assert_eq!(
            state.pending_style_mutations,
            vec![obscura_render::RetainedStyleMutation::Resource]
        );
        assert!(state.resolved_scroll.is_none());
    }

    #[cfg(feature = "render")]
    #[test]
    fn fixed_css_content_image_arrival_still_rebuilds_intrinsic_geometry() {
        let dom = parse_html(
            r#"<html><body><img id="hero" src="fallback.png"
                style="display:block;width:20px;height:10px;content:url('http://example.test/content.png')">
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();
        rt.evaluate("hero.getBoundingClientRect().width")
            .expect("prepare CSS replaced content");
        assert!(rt.state.borrow().resolved_scroll.is_some());

        rt.seed_render_image_resource(
            "http://example.test/content.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(two_by_three_png()),
        );
        let state = rt.state.borrow();
        assert_eq!(
            state.pending_style_mutations,
            vec![obscura_render::RetainedStyleMutation::Resource]
        );
        assert!(state.resolved_scroll.is_none());
    }

    #[cfg(feature = "render")]
    #[test]
    fn document_region_capture_preserves_live_runtime_state_and_resource_cache() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:260px">
                <div style="height:120px;background:red"></div>
                <img src="http://example.test/marker.svg"
                     style="display:block;width:20px;height:20px">
                <div style="height:120px;background:blue"></div>
                <div style="position:fixed;left:0;top:0;width:10px;height:10px;background:lime"></div>
            </body></html>"#,
        );
        let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_loads = loads.clone();
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(move |url: &str| {
                assert_eq!(url, "http://example.test/marker.svg");
                loader_loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(
                    br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
                        <rect width="20" height="20" fill="#ffff00"/>
                    </svg>"##
                        .to_vec(),
                )
            });
        rt.run_page_init();
        assert_eq!(
            rt.evaluate("(function(){ window.scrollTo(0, 50); return window.scrollY; })()")
                .expect("live scroll")
                .as_f64(),
            Some(50.0)
        );
        let live_before = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("live screenshot");
        let (
            prepared_address,
            viewport,
            scroll_offset,
            scroll_generation,
            resolved_root,
            full_height,
        ) = {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("prepared render");
            (
                prepared as *const obscura_render::PreparedRender as usize,
                state.viewport,
                state.scroll_offset,
                state.scroll_generation,
                state
                    .resolved_scroll
                    .as_ref()
                    .expect("resolved scroll")
                    .1
                    .root_offset(),
                prepared.content_size().1,
            )
        };
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);

        let region_png = rt
            .screenshot_prepared_region(obscura_render::CaptureRegion::new(
                0.0, 115.0, 80.0, 40.0, 1.5,
            ))
            .expect("offscreen scaled region");
        let full_png = rt
            .screenshot_prepared_region(obscura_render::CaptureRegion::new(
                0.0,
                0.0,
                80.0,
                full_height,
                1.0,
            ))
            .expect("full-content region");
        let png_size = |bytes: &[u8]| {
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
            (
                u32::from_be_bytes(bytes[16..20].try_into().expect("PNG width")),
                u32::from_be_bytes(bytes[20..24].try_into().expect("PNG height")),
            )
        };
        assert_eq!(png_size(&region_png), (120, 60));
        assert_eq!(png_size(&full_png), (80, full_height.ceil() as u32));

        let live_after = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("unchanged live screenshot");
        assert_eq!(live_after, live_before);
        let state = rt.state.borrow();
        let prepared = state.prepared_render.as_ref().expect("retained render");
        assert_eq!(
            prepared as *const obscura_render::PreparedRender as usize,
            prepared_address
        );
        assert_eq!(state.viewport, viewport);
        assert_eq!(state.scroll_offset, scroll_offset);
        assert_eq!(state.scroll_generation, scroll_generation);
        assert_eq!(
            state
                .resolved_scroll
                .as_ref()
                .expect("retained resolved scroll")
                .1
                .root_offset(),
            resolved_root
        );
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    fn script_registered_url_font_reaches_render_resource_collection() {
        let loads = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let loader_loads = std::sync::Arc::clone(&loads);
        let font = include_bytes!("../../obscura-render/assets/liberation-serif.ttf").to_vec();
        let mut rt = parser_image_runtime(
            r#"<html><body style="margin:0">
                <span id="sample" style="display:inline-block;width:max-content;
                    font-family:DynamicFixture;font-size:40px;white-space:nowrap">WWWWiiii</span>
            </body></html>"#,
            move |url: &str| {
                loader_loads
                    .lock()
                    .expect("font loads")
                    .push(url.to_string());
                (url == "http://example.com/fonts/dynamic.ttf").then(|| font.clone())
            },
        );
        rt.set_viewport(400.0, 100.0);
        let before = rt
            .evaluate("document.getElementById('sample').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .expect("fallback width");
        assert!(loads.lock().expect("initial loads").is_empty());

        let registered = rt
            .evaluate(
                r#"(() => {
                    const face = new FontFace("DynamicFixture",
                        "url('../fonts/dynamic.ttf') format('truetype')",
                        { weight: "normal", style: "normal", unicodeRange: "U+20-7E" });
                    return [document.fonts.add(face) === document.fonts,
                        document.fonts.size, document.fonts.has(face)];
                })()"#,
            )
            .unwrap();
        assert_eq!(registered, serde_json::json!([true, 1, true]));
        {
            let state = rt.state.borrow();
            assert_eq!(state.dynamic_fonts.len(), 1);
            assert!(state.prepared_render.is_some());
            assert_eq!(
                state.pending_style_mutations,
                vec![obscura_render::RetainedStyleMutation::Resource],
                "font registry changes need reshaping and layout, not a fresh cascade"
            );
        }

        let after = rt
            .evaluate("document.getElementById('sample').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .expect("dynamic font width");
        assert_ne!(
            before, after,
            "registered face must affect final text geometry"
        );
        assert_eq!(
            *loads.lock().expect("dynamic font loads"),
            vec!["http://example.com/fonts/dynamic.ttf".to_string()]
        );
        rt.screenshot_prepared((400.0, 100.0), Some("http://example.com/page/index.html"))
            .expect("dynamic font screenshot");
        assert_eq!(loads.lock().expect("repeated font loads").len(), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    fn rendered_layout_cache_is_invalidated_by_style_mutations() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="box" style="height:300px;width:40px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const before = [
                    document.documentElement.scrollHeight,
                    box.getBoundingClientRect().height,
                ];
                box.setAttribute("style", "height:900px;width:80px");
                const after = [
                    document.documentElement.scrollHeight,
                    box.getBoundingClientRect().height,
                    box.getBoundingClientRect().width,
                ];
                return [before, after];
                "#,
            )
            .unwrap();
        let values = result.as_array().expect("result");
        let before = values[0].as_array().expect("before");
        let after = values[1].as_array().expect("after");
        assert!(after[0].as_f64().unwrap() > before[0].as_f64().unwrap());
        assert_eq!(before[1].as_f64(), Some(300.0));
        assert_eq!(after[1].as_f64(), Some(900.0));
        assert_eq!(after[2].as_f64(), Some(80.0));
    }

    #[cfg(feature = "render")]
    #[test]
    fn element_text_content_replacement_recomputes_empty_selector() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<style>#x { width: 10px; height: 5px } #x:empty { width: 30px }</style>
               <div id="x">text</div>"#,
        ));
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const x = document.getElementById("x");
                    const before = x.getBoundingClientRect().width;
                    x.textContent = "";
                    return [before, x.matches(":empty"), x.getBoundingClientRect().width];
                })()"#,
            )
            .unwrap(),
            serde_json::json!([10, true, 30])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn prepared_render_survives_detached_no_op_and_same_viewport_updates() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="box" class="box" style="height:30px;width:40px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        assert!(rt.state.borrow().prepared_render.is_some());

        // Modern frameworks build and decorate substantial detached trees.
        // None of this can affect the connected document's style or geometry.
        rt.evaluate(
            r#"
            const parent = document.createElement('section');
            const child = document.createElement('div');
            child.setAttribute('class', 'box');
            child.setAttribute('style', 'height:900px');
            parent.appendChild(child);
            child.setAttribute('data-state', 'ready');
            "#,
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "detached subtree construction must retain connected layout"
        );

        // Attribute setters still fire their DOM/observer semantics when the
        // assigned value is identical, but layout is not dirtied.
        rt.evaluate(
            r#"
            const box = document.getElementById('box');
            box.setAttribute('class', 'box');
            box.removeAttribute('data-absent');
            "#,
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "no-op connected attributes must retain prepared layout"
        );

        rt.set_viewport(200.0, 100.0);
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "reapplying the current viewport must not force layout"
        );

        rt.evaluate(
            "document.getElementById('box').setAttribute('style', 'height:60px;width:40px')",
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            assert!(
                state.prepared_render.is_some(),
                "a retained inline-style change must keep the prior style maps until flush"
            );
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Attribute(
                    obscura_render::AttributeStyleMutation { name, .. }
                )] if name == "style"
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().height")
                .unwrap()
                .as_f64(),
            Some(60.0)
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn waapi_pause_seek_and_cancel_preserve_authored_inline_style() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="box" style="opacity:.2;width:20px;height:20px"></div></body></html>"#,
        );
        rt.execute_script(
            "waapi",
            r#"
                globalThis.box = document.getElementById('box');
                globalThis.__animation = box.animate(
                    [{opacity:.2, transform:'translateX(0px)'}, {opacity:1, transform:'translateX(100px)'}],
                    {duration:100, fill:'both', easing:'linear'}
                );
                __animation.pause();
                __animation.currentTime = 50;
            "#,
        ).unwrap();
        assert_eq!(rt.evaluate("box.style.opacity").unwrap(), serde_json::json!(".2"));
        assert_eq!(rt.evaluate("box.getAnimations()[0] === __animation").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("document.getAnimations()[0] === __animation").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__animation.playState").unwrap(), serde_json::json!("paused"));
        assert_eq!(
            rt.evaluate("!('easingBezier' in __animation.effect.getTiming()) && !('linearEasing' in __animation.effect.getComputedTiming())").unwrap(),
            serde_json::json!(true),
        );
        let opacity = rt.evaluate("getComputedStyle(box).opacity").unwrap();
        let opacity = opacity.as_str().unwrap().parse::<f32>().unwrap();
        assert!((opacity - 0.6).abs() < 0.001, "midpoint opacity was {opacity}");

        rt.execute_script("cancel", "__animation.cancel()").unwrap();
        assert_eq!(rt.evaluate("box.style.opacity").unwrap(), serde_json::json!(".2"));
        assert_eq!(rt.evaluate("getComputedStyle(box).opacity").unwrap(), serde_json::json!("0.2"));
        assert_eq!(rt.evaluate("box.getAnimations().length").unwrap(), serde_json::json!(0.0));
        assert_eq!(rt.evaluate("document.getAnimations().length").unwrap(), serde_json::json!(0.0));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn waapi_zero_duration_finishes_asynchronously_and_fires_lifecycle() {
        let mut rt = setup_runtime(r#"<div id="box" style="opacity:.1"></div>"#);
        rt.execute_script(
            "waapi-lifecycle",
            r#"
                globalThis.box = document.getElementById('box');
                globalThis.__ready = false;
                globalThis.__finished = false;
                globalThis.__finishEvent = false;
                globalThis.__animation = box.animate([{opacity:.1}, {opacity:1}], {duration:0, fill:'both'});
                __animation.onfinish = () => { __finishEvent = true; };
                __animation.ready.then(() => { __ready = true; });
                __animation.finished.then(() => { __finished = true; });
            "#,
        ).unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        assert_eq!(rt.evaluate("__ready").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__finished").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__finishEvent").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__animation.playState").unwrap(), serde_json::json!("finished"));
        assert_eq!(rt.evaluate("getComputedStyle(box).opacity").unwrap(), serde_json::json!("1"));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn waapi_positive_infinite_iterations_remain_active() {
        let mut rt = setup_runtime(r#"<div id="box" style="opacity:.1"></div>"#);
        rt.execute_script(
            "waapi-infinite",
            r#"
                globalThis.__infiniteFinished = false;
                globalThis.__infiniteAnimation = document.getElementById('box').animate(
                    [{opacity:.1}, {opacity:1}],
                    {duration:1, iterations:Infinity, fill:'both', easing:'linear'}
                );
                __infiniteAnimation.finished.then(() => { __infiniteFinished = true; });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[__infiniteAnimation.playState, __infiniteAnimation.effect.getTiming().iterations === Infinity, __infiniteFinished]",
            )
            .unwrap(),
            serde_json::json!(["running", true, false]),
        );
        rt.evaluate("getComputedStyle(document.getElementById('box')).opacity")
            .unwrap();
        assert!(rt.prepared_has_active_css_animations());
    }

    #[cfg(feature = "render")]
    #[test]
    fn forward_animation_samples_retain_static_prepared_render() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="width:80px;height:60px;background:#1769aa"></div>
            </body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 100.0,
        }));
        let first = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("first static frame");
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 250.0,
        }));
        let second = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("second static frame");
        let state = rt.state.borrow();
        assert_eq!(
            state.prepared_render.as_ref().unwrap() as *const _ as usize,
            prepared_address,
            "a live timestamp alone must not relayout a static document"
        );
        assert_eq!(first, second);
    }

    #[cfg(feature = "render")]
    #[test]
    fn forward_active_animation_sample_updates_geometry_and_paint_from_retained_frame() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @keyframes grow {
                    from { width:20px; background-color:#ff0000 }
                    to { width:100px; background-color:#0000ff }
                }
                #box { height:40px; animation:grow 1000ms linear both }
            </style></head><body style="margin:0"><div id="box"></div></body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(120.0, 40.0);
        rt.run_page_init();

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(0.0)));
        let initial = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.test/page"))
            .expect("initial animation frame");
        assert!((animation_test_width(&rt, "box") - 20.0).abs() < 0.1);

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(500.0)));
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "a forward active sample should retain the previous style graph until flush"
        );
        let midpoint = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.test/page"))
            .expect("retained midpoint animation frame");

        assert!((animation_test_width(&rt, "box") - 60.0).abs() < 0.1);
        assert_ne!(initial, midpoint, "animated paint output must advance");
        assert_eq!(
            rt.state
                .borrow()
                .prepared_render
                .as_ref()
                .unwrap()
                .animation_sample_time()
                .milliseconds,
            500.0
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn forward_waapi_sample_updates_retained_style_and_paint() {
        let mut rt = setup_runtime(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="box" style="width:40px;height:40px;background:#1769aa"></div>
            </body></html>"#,
        );
        rt.set_viewport(120.0, 40.0);
        rt.state.borrow_mut().animation_timeline_origin = std::time::Instant::now();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(0.0)));
        rt.screenshot_prepared((120.0, 40.0), Some("http://example.com/test"))
            .expect("static frame before WAAPI registration");
        rt.execute_script(
            "waapi-retained-frame",
            r#"document.getElementById('box').animate(
                [{opacity:0,transform:'translateX(0px)'},
                 {opacity:1,transform:'translateX(80px)'}],
                {duration:1000,fill:'both',easing:'linear'}
            )"#,
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            let box_node = state
                .dom
                .as_ref()
                .unwrap()
                .get_element_by_id("box")
                .unwrap();
            assert!(
                state.prepared_render.is_some(),
                "registering one WAAPI effect must retain the previous style graph"
            );
            assert_eq!(
                state.pending_style_mutations,
                vec![obscura_render::RetainedStyleMutation::WaapiAnimation {
                    node: box_node
                }]
            );
        }
        let initial = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.com/test"))
            .expect("initial WAAPI frame");

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(500.0)));
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "a forward WAAPI sample should preserve the prepared style graph until flush"
        );
        let midpoint = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.com/test"))
            .expect("retained WAAPI midpoint");
        let midpoint_opacity = {
            let state = rt.state.borrow();
            let dom = state.dom.as_ref().unwrap();
            let box_node = dom.get_element_by_id("box").unwrap();
            state.prepared_render.as_ref().unwrap().layout().styles[&box_node]
                .opacity
                .unwrap()
        };

        assert!(
            (0.45..0.55).contains(&midpoint_opacity),
            "WAAPI midpoint opacity={midpoint_opacity}"
        );
        assert_ne!(initial, midpoint, "WAAPI paint output must advance");
    }

    #[cfg(feature = "render")]
    #[test]
    fn waapi_cancel_retains_static_style_graph_and_restores_authored_style() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="box" style="opacity:.25;width:20px;height:20px"></div></body></html>"#,
        );
        rt.set_viewport(40.0, 40.0);
        rt.screenshot_prepared((40.0, 40.0), Some("http://example.com/test"))
            .expect("static frame");
        rt.execute_script(
            "waapi-retained-cancel",
            r#"globalThis.__cancelAnimation = document.getElementById('box').animate(
                [{opacity:1}, {opacity:0}], {duration:1000, fill:'both'}
            )"#,
        )
        .unwrap();
        let animated_opacity = rt
            .evaluate("Number(getComputedStyle(document.getElementById('box')).opacity)")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!(animated_opacity > 0.9, "animated opacity={animated_opacity}");

        rt.evaluate("__cancelAnimation.cancel()").unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "canceling one WAAPI effect must retain the previous style graph until recascade"
        );
        assert_eq!(
            rt.evaluate("getComputedStyle(document.getElementById('box')).opacity")
                .unwrap(),
            serde_json::json!("0.25")
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn completed_animation_retains_forward_frame_but_backward_seek_rebuilds() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @keyframes fade { from { opacity:1 } to { opacity:0 } }
                #box { width:80px; height:60px; background:#ff0000;
                       animation:fade 100ms linear forwards }
            </style></head><body style="margin:0"><div id="box"></div></body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 150.0,
        }));
        let completed = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("completed animation frame");
        assert!(!rt.prepared_has_active_css_animations());
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 300.0,
        }));
        let later = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("later completed frame");
        assert_eq!(completed, later);
        assert_eq!(
            rt.state
                .borrow()
                .prepared_render
                .as_ref()
                .unwrap() as *const _ as usize,
            prepared_address,
            "a finite fill-forwards animation must not relayout after completion"
        );

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 0.0,
        }));
        assert!(
            rt.state.borrow().prepared_render.is_none(),
            "backward timeline seeks must invalidate the completed frame"
        );
        let initial = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("initial animation frame");
        assert_ne!(completed, initial);
        assert!(rt.prepared_has_active_css_animations());
    }

    #[cfg(feature = "render")]
    #[test]
    fn unsupported_custom_property_animation_does_not_keep_render_damage_active() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @property --brand-cycle { syntax:"<color>"; inherits:true; initial-value:#2dacf9 }
                @keyframes brand-cycle {
                    from { --brand-cycle:#2dacf9 }
                    to { --brand-cycle:#7ce95a }
                }
                :root { animation:brand-cycle 10s linear infinite }
            </style></head><body style="margin:0">
                <div style="width:80px;height:60px;background:#1769aa"></div>
            </body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();

        let first = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("initial frame");
        assert!(
            !rt.prepared_has_active_css_animations(),
            "an unsupported custom-property-only animation has no render damage"
        );
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };
        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 5_000.0,
        }));
        let later = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("later frame");
        assert_eq!(first, later);
        assert_eq!(
            rt.state.borrow().prepared_render.as_ref().unwrap() as *const _ as usize,
            prepared_address
        );
    }

    #[cfg(feature = "render")]
    fn animation_test_width(rt: &ObscuraJsRuntime, id: &str) -> f32 {
        let state = rt.state.borrow();
        let dom = state.dom.as_ref().unwrap();
        let node = dom.query_selector(&format!("#{id}")).unwrap().unwrap();
        match state.prepared_render.as_ref().unwrap().layout().styles[&node].width {
            obscura_render::Dimension::Px(width) => width,
            ref other => panic!("expected animated pixel width, got {other:?}"),
        }
    }

    #[cfg(feature = "render")]
    fn animation_epoch_runtime() -> ObscuraJsRuntime {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @keyframes grow { from { width:0px } to { width:100px } }
                .anim { height:10px; animation:grow 1000ms linear forwards }
            </style></head><body style="margin:0"><i id="anchor"></i></body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(200.0, 80.0);
        rt.run_page_init();
        rt
    }

    #[cfg(feature = "render")]
    #[test]
    fn remove_and_reappend_restarts_animation_without_intermediate_flush() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(1_000.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        assert!(animation_test_width(&rt, "box") > 95.0);

        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(1_000);
        rt.evaluate("var box=document.getElementById('box');box.remove();document.body.appendChild(box)")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(1_100.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let restarted = animation_test_width(&rt, "box");
        assert!((5.0..20.0).contains(&restarted), "restarted width={restarted}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn scoped_animation_epochs_survive_later_unrelated_mutations_and_t0_capture() {
        let mut rt = animation_epoch_runtime();
        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(100);
        rt.evaluate("var a=document.createElement('div');a.id='first';a.className='anim';document.body.appendChild(a)")
            .unwrap();
        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(500);
        rt.evaluate("var b=document.createElement('div');b.id='second';b.className='anim';document.body.appendChild(b)")
            .unwrap();
        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(600);
        rt.evaluate("document.getElementById('anchor').setAttribute('data-unrelated','yes')")
            .unwrap();

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::local_override(0.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        assert_eq!(animation_test_width(&rt, "first"), 0.0);
        assert_eq!(animation_test_width(&rt, "second"), 0.0);

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(700.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let first = animation_test_width(&rt, "first");
        let second = animation_test_width(&rt, "second");
        assert!((55.0..65.0).contains(&first), "first width={first}");
        assert!((15.0..25.0).contains(&second), "second width={second}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn timing_edits_preserve_identity_and_pause_holds_then_resumes() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(300.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        assert!((25.0..35.0).contains(&animation_test_width(&rt, "box")));

        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(300);
        rt.evaluate("document.getElementById('box').setAttribute('style','animation-duration:2000ms;animation-play-state:paused')")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(700.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let held = animation_test_width(&rt, "box");
        assert!((12.0..18.0).contains(&held), "held width={held}");

        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(800);
        rt.evaluate("document.getElementById('box').setAttribute('style','animation-duration:2000ms;animation-play-state:running')")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(1_000.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let resumed = animation_test_width(&rt, "box");
        assert!((22.0..28.0).contains(&resumed), "resumed width={resumed}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn cssom_geometry_samples_live_document_time() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        let initial = rt
            .evaluate("document.getElementById('box').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let later = rt
            .evaluate("document.getElementById('box').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!(later >= initial + 8.0, "initial={initial}, later={later}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn fixed_animation_capture_is_invariant_after_geometry_flush() {
        let make_runtime = || {
            let mut rt = ObscuraJsRuntime::new();
            rt.set_dom(parse_html(
                r#"<html style="margin:0"><head><style>
                    @keyframes dismiss {
                        from { opacity:1; transform:translateY(0) }
                        to { opacity:0; transform:translateY(-80px) }
                    }
                    body { margin:0; width:160px; height:100px; background:#f5f7fa }
                    #content { width:120px; height:50px; margin:20px; background:#1769aa }
                    #shell { position:fixed; inset:0; background:#111827 }
                    #shell.dismissed { animation:dismiss 600ms linear forwards }
                </style></head><body><div id="content"></div>
                    <div id="shell"></div></body></html>"#,
            ));
            rt.set_url("http://example.test/github-like-shell");
            rt.set_viewport(160.0, 100.0);
            rt.run_page_init();
            rt.evaluate("document.getElementById('shell').className='dismissed'")
                .unwrap();
            rt
        };
        let fixed = obscura_render::AnimationSample::local_override(750.0);
        let direct_rt = make_runtime();
        assert!(direct_rt.set_animation_sample(fixed));
        let direct = direct_rt
            .screenshot_prepared((160.0, 100.0), Some("http://example.test/github-like-shell"))
            .expect("direct fixed-time capture");

        let mut geometry_rt = make_runtime();
        let rect = geometry_rt
            .evaluate("document.getElementById('content').getBoundingClientRect().toJSON()")
            .expect("geometry flush before capture");
        assert_eq!(rect["width"].as_f64(), Some(120.0));
        assert!(geometry_rt.set_animation_sample(fixed));
        let after_geometry = geometry_rt
            .screenshot_prepared((160.0, 100.0), Some("http://example.test/github-like-shell"))
            .expect("fixed-time capture after geometry");

        assert_eq!(
            direct, after_geometry,
            "a CSSOM geometry flush must not change fixed-time capture output"
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn cssom_animation_sample_is_frozen_within_one_javascript_task() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        let values = rt
            .evaluate(
                r#"(function(){
                    const box = document.getElementById('box');
                    const first = box.getBoundingClientRect().width;
                    const deadline = Date.now() + 120;
                    while (Date.now() < deadline) {}
                    return [first, box.getBoundingClientRect().width];
                })()"#,
            )
            .unwrap();
        let widths = values.as_array().unwrap();
        assert_eq!(
            widths[0].as_f64(),
            widths[1].as_f64(),
            "forced layout reads in one long task must share one animation frame"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn timer_callback_starts_a_fresh_lazy_animation_sample() {
        let mut rt = animation_epoch_runtime();
        rt.execute_script(
            "timer-animation-sample",
            r#"
                var box=document.createElement('div');
                box.id='box';box.className='anim';document.body.appendChild(box);
                globalThis.__beforeTimerWidth=box.getBoundingClientRect().width;
                setTimeout(() => {
                    globalThis.__afterTimerWidth=box.getBoundingClientRect().width;
                }, 100);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(250).await.unwrap();
        let values = rt
            .evaluate("[globalThis.__beforeTimerWidth, globalThis.__afterTimerWidth]")
            .unwrap();
        let widths = values.as_array().unwrap();
        let before = widths[0].as_f64().unwrap();
        let after = widths[1].as_f64().unwrap();
        assert!(after >= before + 7.0, "before={before}, after={after}");
    }

    #[cfg(feature = "render")]
    #[test]
    fn autocomplete_attribute_retains_prepared_render_until_geometry_flush() {
        let dom = parse_html(
            r#"<html style="margin:0"><head><style>
                input { display:block; width:40px; height:20px }
                input[autocomplete="off"] { width:90px }
            </style></head><body style="margin:0">
                <input id="field" autocomplete="on">
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('field').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        assert!(rt.state.borrow().prepared_render.is_some());

        rt.evaluate("document.getElementById('field').setAttribute('autocomplete', 'off')")
            .unwrap();
        {
            let state = rt.state.borrow();
            assert!(
                state.prepared_render.is_some(),
                "ordinary selector attributes must retain the prepared render until flush"
            );
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Attribute(
                    obscura_render::AttributeStyleMutation { name, old_value, new_value, .. }
                )] if name == "autocomplete"
                    && old_value.as_deref() == Some("on")
                    && new_value.as_deref() == Some("off")
            ));
        }

        assert_eq!(
            rt.evaluate("document.getElementById('field').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(90.0),
            "the retained selector invalidation must observe the new attribute value"
        );
        assert!(rt.state.borrow().pending_style_mutations.is_empty());
    }

    #[cfg(feature = "render")]
    #[test]
    fn namespaced_attribute_mutations_participate_in_id_and_render_invalidation() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0"><div id="box" class="box" style="height:30px;width:40px"></div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().height")
                .unwrap()
                .as_f64(),
            Some(30.0)
        );
        assert!(rt.state.borrow().prepared_render.is_some());

        rt.evaluate(
            "document.getElementById('box').setAttributeNS(null, 'class', 'box')",
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "an identical null-namespace attribute must retain layout"
        );

        rt.evaluate(
            "document.getElementById('box').setAttributeNS(null, 'style', 'height:70px;width:40px')",
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_none(),
            "a connected namespace-aware style mutation must invalidate layout"
        );
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().height")
                .unwrap()
                .as_f64(),
            Some(70.0)
        );

        let id_result = rt
            .evaluate(
                "(function(){const box=document.getElementById('box');box.setAttributeNS(null,'id','renamed');const found=document.getElementById('renamed')===box;box.removeAttributeNS(null,'id');return found && document.getElementById('renamed')===null;})()",
            )
            .unwrap();
        assert_eq!(id_result, serde_json::json!(true));
    }

    #[cfg(feature = "render")]
    #[test]
    fn stylesheet_index_cache_reuses_sources_but_not_live_cascade_or_viewport() {
        let dom = parse_html(
            r#"<html style="margin:0"><head><style id="sheet">
                .a { width:40px; height:20px }
                .b { width:80px; height:20px }
            </style></head><body style="margin:0">
                <div id="box" class="a"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        {
            let state = rt.state.borrow();
            assert_eq!(state.stylesheet_cache.miss_count(), 1);
            assert_eq!(state.stylesheet_cache.hit_count(), 0);
            assert!(state.stylesheet_cache.retained_source_bytes() > 0);
        }

        // The compiled selector index is reusable, but matching and cascade
        // must observe the new class on the live connected element.
        rt.evaluate("document.getElementById('box').className = 'b'")
            .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert_eq!(state.pending_style_mutations.len(), 1);
        }
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(80.0)
        );
        {
            let state = rt.state.borrow();
            assert_eq!(state.stylesheet_cache.miss_count(), 1);
            assert_eq!(state.stylesheet_cache.hit_count(), 1);
        }

        // Style text is part of the exact key and cannot reuse stale rules.
        rt.evaluate(
            r#"document.getElementById('sheet').textContent =
                '.b{width:120px;height:20px}@media(min-width:250px){.b{width:160px}}'"#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(120.0)
        );
        {
            let state = rt.state.borrow();
            assert_eq!(state.stylesheet_cache.miss_count(), 2);
            assert_eq!(state.stylesheet_cache.hit_count(), 1);
        }

        // Media-query filtering is viewport-dependent, so an exact source hit
        // at a different viewport must still reparse and reindex.
        rt.set_viewport(300.0, 100.0);
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(160.0)
        );
        let state = rt.state.borrow();
        assert_eq!(state.stylesheet_cache.miss_count(), 3);
        assert_eq!(state.stylesheet_cache.hit_count(), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    fn connected_tree_mutations_queue_retained_styles_until_geometry_flush() {
        let dom = parse_html(
            r#"<html style="margin:0"><head><style>
                .item{display:block;width:40px;height:12px}
                .item:nth-child(2){width:80px}
            </style></head><body style="margin:0">
                <main id="list"><div id="first" class="item"></div></main>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('first').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        rt.evaluate(
            "const added=document.createElement('div');added.id='added';added.className='item';document.getElementById('list').appendChild(added)",
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Tree(
                    obscura_render::TreeStyleMutation::Insert { .. }
                )]
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('added').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(80.0)
        );
        assert!(rt.state.borrow().pending_style_mutations.is_empty());

        rt.evaluate("document.getElementById('added').style.width='65px'")
            .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Attribute(
                    obscura_render::AttributeStyleMutation { name, .. }
                )] if name == "style"
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('added').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(65.0)
        );
        assert_eq!(
            rt.evaluate("(function(){const added=document.getElementById('added');added.style.width='';return added.getBoundingClientRect().width})()")
                .unwrap()
                .as_f64(),
            Some(80.0)
        );

        rt.evaluate(
            "document.getElementById('list').removeChild(document.getElementById('first'))",
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Tree(
                    obscura_render::TreeStyleMutation::Remove { .. }
                )]
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('added').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        assert!(rt.state.borrow().pending_style_mutations.is_empty());
    }

    #[cfg(feature = "render")]
    #[test]
    fn root_overflow_clip_preserves_cssom_scroll_range() {
        let dom = parse_html(
            r#"<html style="margin:0;height:100%;overflow:hidden">
                <body style="margin:0;height:100%">
                    <main id="main" style="padding-top:48px">
                        <div style="height:5000px"></div>
                    </main>
                </body>
            </html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(900.0, 1000.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const main = document.getElementById("main");
                const before = main.getBoundingClientRect();
                scrollTo(0, 99999);
                const after = main.getBoundingClientRect();
                return [
                    before.height,
                    document.documentElement.scrollHeight,
                    document.body.scrollHeight,
                    scrollY,
                    after.top,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([5048, 5048, 5048, 4048, -4048]));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn rendered_window_scroll_events_require_actual_movement() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:1000px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let moved = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    window.scrollTo(0, 100);
                    setTimeout(() => resolve([win, doc, window.scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(moved.value.unwrap(), serde_json::json!([1, 1, 100]));

        rt.evaluate("window.scrollTo(0, 99999)").unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        let no_op = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    const before = window.scrollY;
                    window.scrollTo(0, 99999);
                    setTimeout(() => resolve([win, doc, before, window.scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        let values = no_op.value.unwrap();
        let values = values.as_array().expect("array");
        assert_eq!(
            &values[0..2],
            &serde_json::json!([0, 0]).as_array().unwrap()[..]
        );
        assert_eq!(values[2], values[3]);
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn fingerprinted_screen_does_not_invent_a_device_scale_factor() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html("<html><body></body></html>"));
        rt.set_viewport(300.0, 200.0);
        // Force the fingerprint seed whose screen-pool entry is 2560x1440.
        // That physical screen must not silently turn a 1x render surface into
        // a 2x devicePixelContentBoxSize surface.
        rt.execute_script(
            "deterministic-high-resolution-screen",
            "Date.now = () => 0; Math.random = () => 2 / 0xFFFFFFFF;",
        )
        .unwrap();
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("[screen.width, screen.height, devicePixelRatio]")
                .unwrap(),
            serde_json::json!([2560, 1440, 1])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn resize_observer_reports_real_boxes_only_when_selected_size_changes() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="target" style="box-sizing:border-box;width:120px;height:80px;
                     padding:5px 7px;border:2px solid black"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(300.0, 200.0);
        rt.run_page_init();
        rt.execute_script(
            "resize-observer-boxes",
            r#"
                globalThis.__resizeRecords = [];
                globalThis.__resizeObserver = new ResizeObserver(entries => {
                    __resizeRecords.push(...entries.map(entry => ({
                        interfaces: [
                            entry instanceof ResizeObserverEntry,
                            entry.contentBoxSize[0] instanceof ResizeObserverSize,
                            entry.borderBoxSize[0] instanceof ResizeObserverSize,
                            entry.devicePixelContentBoxSize[0] instanceof ResizeObserverSize,
                        ],
                        contentRect: [
                            entry.contentRect.x, entry.contentRect.y,
                            entry.contentRect.width, entry.contentRect.height,
                        ],
                        content: [
                            entry.contentBoxSize[0].inlineSize,
                            entry.contentBoxSize[0].blockSize,
                        ],
                        border: [
                            entry.borderBoxSize[0].inlineSize,
                            entry.borderBoxSize[0].blockSize,
                        ],
                        device: [
                            entry.devicePixelContentBoxSize[0].inlineSize,
                            entry.devicePixelContentBoxSize[0].blockSize,
                        ],
                    })));
                });
                __resizeObserver.observe(document.getElementById("target"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__resizeRecords").unwrap(),
            serde_json::json!([{
                "interfaces": [true, true, true, true],
                "contentRect": [7, 5, 102, 66],
                "content": [102, 66],
                "border": [120, 80],
                "device": [102, 66],
            }])
        );

        // A style mutation still causes a rendering checkpoint, but unchanged
        // observed geometry must not produce a speculative notification.
        rt.evaluate(r#"document.getElementById("target").style.color = "red""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__resizeRecords.length").unwrap().as_f64(),
            Some(1.0)
        );

        rt.evaluate(r#"document.getElementById("target").style.width = "140px""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "__resizeRecords.map(record => [record.content[0], record.border[0]])"
            )
            .unwrap(),
            serde_json::json!([[102, 120], [122, 140]])
        );
        assert_eq!(
            rt.evaluate("__obscura_nextPendingTimeoutDelay()")
                .unwrap()
                .as_f64(),
            Some(-1.0)
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn resize_observer_batches_unique_targets_into_one_native_layout_read() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="a" style="box-sizing:border-box;width:100px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="b" style="box-sizing:border-box;width:110px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="c" style="box-sizing:border-box;width:120px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="d" style="box-sizing:border-box;width:130px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="vertical" style="box-sizing:border-box;width:140px;height:30px;padding:2px 3px;border:1px solid;writing-mode:vertical-rl"></div>
                <div id="hidden" style="display:none;width:50px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 300.0);
        rt.run_page_init();
        rt.execute_script(
            "batch-resize-observer-targets",
            r#"
                globalThis.__resizeBulkCalls = 0;
                globalThis.__resizeBulkSizes = [];
                globalThis.__resizeLegacyGeometryCalls = 0;
                globalThis.__resizeComputedStyleCalls = 0;
                const nativeBulk = Deno.core.ops.op_resize_observer_measurements;
                const nativeGeometry = Deno.core.ops.op_layout_geometry;
                const nativeComputedStyle = Deno.core.ops.op_computed_style;
                Deno.core.ops.op_resize_observer_measurements = input => {
                    __resizeBulkCalls++;
                    __resizeBulkSizes.push(JSON.parse(input).length);
                    return nativeBulk(input);
                };
                Deno.core.ops.op_layout_geometry = (...args) => {
                    __resizeLegacyGeometryCalls++;
                    return nativeGeometry(...args);
                };
                Deno.core.ops.op_computed_style = (...args) => {
                    __resizeComputedStyleCalls++;
                    return nativeComputedStyle(...args);
                };

                globalThis.__resizeBatchRecords = [];
                const detached = document.createElement("div");
                detached.id = "detached";
                detached.style.cssText = "width:60px;height:20px";
                const targets = ["a", "b", "c", "d", "vertical", "hidden"]
                    .map(id => document.getElementById(id));
                targets.push(detached);
                const observer = new ResizeObserver(entries => {
                    __resizeBatchRecords.push(entries.map(entry => [
                        entry.target.id,
                        entry.contentBoxSize[0].inlineSize,
                        entry.contentBoxSize[0].blockSize,
                        entry.borderBoxSize[0].inlineSize,
                        entry.borderBoxSize[0].blockSize,
                    ]));
                });
                for (const target of targets) observer.observe(target);
                // A second observer of an existing target must share the same
                // native measurement rather than adding it to the batch twice.
                globalThis.__duplicateResizeRecords = 0;
                const duplicate = new ResizeObserver(entries => {
                    __duplicateResizeRecords += entries.length;
                });
                duplicate.observe(targets[2], { box: "border-box" });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();

        assert_eq!(
            rt.evaluate(
                r#"[
                    __resizeBulkCalls,
                    __resizeBulkSizes,
                    __resizeLegacyGeometryCalls,
                    __resizeComputedStyleCalls,
                    __duplicateResizeRecords,
                    __resizeBatchRecords,
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                1,
                [7],
                0,
                0,
                1,
                [[
                    ["a", 92, 24, 100, 30],
                    ["b", 102, 24, 110, 30],
                    ["c", 112, 24, 120, 30],
                    ["d", 122, 24, 130, 30],
                    ["vertical", 24, 132, 30, 140],
                    ["hidden", 0, 0, 0, 0],
                    ["detached", 0, 0, 0, 0],
                ]],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn resize_observer_selected_box_and_viewport_lifecycle_match_chromium() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="target" style="box-sizing:border-box;width:50vw;height:40px;
                     padding:4px;border:2px solid"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "resize-observer-selected-box",
            r#"
                globalThis.__contentWidths = [];
                globalThis.__borderWidths = [];
                const target = document.getElementById("target");
                globalThis.__contentObserver = new ResizeObserver(entries => {
                    __contentWidths.push(entries[0].contentBoxSize[0].inlineSize);
                });
                globalThis.__borderObserver = new ResizeObserver(entries => {
                    __borderWidths.push(entries[0].borderBoxSize[0].inlineSize);
                });
                __contentObserver.observe(target, { box: "content-box" });
                __borderObserver.observe(target, { box: "border-box" });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__contentWidths, __borderWidths]").unwrap(),
            serde_json::json!([[88], [100]])
        );

        // A viewport update is a rendering update even without a DOM mutation.
        rt.set_viewport(300.0, 100.0);
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__contentWidths, __borderWidths]").unwrap(),
            serde_json::json!([[88, 138], [100, 150]])
        );

        // With border-box sizing a thicker border shrinks the content box but
        // leaves the selected border box unchanged.
        rt.evaluate(r#"document.getElementById("target").style.borderWidth = "4px""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__contentWidths, __borderWidths]").unwrap(),
            serde_json::json!([[88, 138, 134], [100, 150]])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn scrolling_does_not_remeasure_resize_observer_targets() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:1000px">
                <div id="probe" style="width:40px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "observe-before-scroll",
            r#"
                globalThis.__scrollResizeRecords = 0;
                globalThis.__scrollResizeObserver = new ResizeObserver(entries => {
                    __scrollResizeRecords += entries.length;
                });
                __scrollResizeObserver.observe(document.getElementById("probe"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__scrollResizeRecords").unwrap().as_f64(),
            Some(1.0)
        );

        rt.execute_script(
            "count-scroll-geometry-reads",
            r#"
                globalThis.__scrollGeometryReads = 0;
                globalThis.__nativeLayoutGeometry = Deno.core.ops.op_layout_geometry;
                Deno.core.ops.op_layout_geometry = (...args) => {
                    __scrollGeometryReads++;
                    return __nativeLayoutGeometry(...args);
                };
                window.scrollTo(0, 50);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        let result = rt
            .evaluate("[scrollY, __scrollGeometryReads, __scrollResizeRecords]")
            .unwrap();
        rt.execute_script(
            "restore-layout-geometry-op",
            "Deno.core.ops.op_layout_geometry = __nativeLayoutGeometry;",
        )
        .unwrap();
        assert_eq!(result, serde_json::json!([50, 0, 1]));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn resize_observer_disconnect_is_reusable_and_inline_boxes_are_empty() {
        let dom = parse_html(
            r#"<html><body><div id="first" style="width:40px;height:20px"></div>
                <span id="inline" style="padding:8px;border:2px solid">text</span>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const deliveries = [];
                    const observer = new ResizeObserver(entries => {
                        deliveries.push(entries.map(entry => [
                            entry.target.id,
                            entry.contentRect.width,
                            entry.borderBoxSize[0].inlineSize,
                        ]));
                        if (deliveries.length === 1) {
                            observer.disconnect();
                            observer.observe(document.getElementById("inline"));
                        } else {
                            observer.disconnect();
                            resolve([deliveries, __resizeObservers.length]);
                        }
                    });
                    observer.observe(document.getElementById("first"));
                    setTimeout(() => resolve(["timed out"]), 100);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([[[["first", 40, 40]], [["inline", 0, 0]]], 0])
        );

        assert_eq!(
            rt.evaluate(
                r#"[
                    (() => { try { new ResizeObserver(null); } catch (e) { return e.name; } })(),
                    (() => { try { new ResizeObserver(() => {}).observe(document); } catch (e) { return e.name; } })(),
                    (() => { try { new ResizeObserver(() => {}).observe(document.body, {box:"margin-box"}); } catch (e) { return e.name; } })(),
                ]"#,
            )
            .unwrap(),
            serde_json::json!(["TypeError", "TypeError", "TypeError"])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn resize_observer_self_resize_is_depth_bounded_without_timer_spin() {
        let dom = parse_html(
            r#"<html><body><div id="target" style="width:40px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "resize-observer-loop-limit",
            r#"
                globalThis.__resizeCallbacks = 0;
                globalThis.__resizeLoopErrors = 0;
                addEventListener("error", event => {
                    if (event.message === "ResizeObserver loop completed with undelivered notifications.") {
                        __resizeLoopErrors++;
                    }
                });
                const target = document.getElementById("target");
                globalThis.__loopingResizeObserver = new ResizeObserver(() => {
                    __resizeCallbacks++;
                    target.style.width = (40 + __resizeCallbacks) + "px";
                });
                __loopingResizeObserver.observe(target);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[__resizeCallbacks, __resizeLoopErrors, __obscura_nextPendingTimeoutDelay()]"
            )
            .unwrap(),
            serde_json::json!([1, 1, -1])
        );

        // A later external rendering change starts a fresh bounded cycle; the
        // suppressed same-depth observation did not poison future delivery.
        rt.evaluate(r#"document.getElementById("target").style.width = "60px""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__resizeCallbacks, __resizeLoopErrors]").unwrap(),
            serde_json::json!([2, 2])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_tracks_viewport_threshold_crossings() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:150px"></div>
                <div id="target" style="height:100px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const records = [];
                    const target = document.getElementById("target");
                    const observer = new IntersectionObserver(entries => {
                        for (const entry of entries) {
                            records.push([
                                entry.isIntersecting,
                                Math.round(entry.intersectionRatio * 100) / 100,
                                Math.round(entry.boundingClientRect.top),
                                Math.round(entry.intersectionRect.height),
                            ]);
                        }
                    }, { threshold: [0, 0.5, 1] });
                    observer.observe(target);
                    setTimeout(() => window.scrollTo(0, 100), 25);
                    setTimeout(() => window.scrollTo(0, 260), 50);
                    setTimeout(() => resolve(records), 80);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([[false, 0, 150, 0], [true, 0.5, 50, 50], [false, 0, -110, 0],])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_batches_unique_clip_graph_into_one_native_layout_read() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="root" style="width:200px;height:100px;overflow:hidden">
                    <div id="clip" style="width:150px;height:80px;overflow:auto">
                        <div id="a" style="width:30px;height:20px"></div>
                        <div id="b" style="width:40px;height:20px"></div>
                        <div id="hidden" style="display:none"></div>
                    </div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(300.0, 200.0);
        rt.run_page_init();
        rt.execute_script(
            "batch-intersection-observer-clip-graph",
            r#"
                globalThis.__intersectionBulkCalls = 0;
                globalThis.__intersectionBulkSizes = [];
                globalThis.__intersectionLegacyGeometryCalls = 0;
                globalThis.__intersectionComputedStyleCalls = 0;
                const nativeBulk = Deno.core.ops.op_intersection_observer_measurements;
                const nativeGeometry = Deno.core.ops.op_layout_geometry;
                const nativeComputedStyle = Deno.core.ops.op_computed_style;
                Deno.core.ops.op_intersection_observer_measurements = input => {
                    __intersectionBulkCalls++;
                    __intersectionBulkSizes.push(JSON.parse(input).length);
                    return nativeBulk(input);
                };
                Deno.core.ops.op_layout_geometry = (...args) => {
                    __intersectionLegacyGeometryCalls++;
                    return nativeGeometry(...args);
                };
                Deno.core.ops.op_computed_style = (...args) => {
                    __intersectionComputedStyleCalls++;
                    return nativeComputedStyle(...args);
                };

                const root = document.getElementById("root");
                const a = document.getElementById("a");
                const b = document.getElementById("b");
                const hidden = document.getElementById("hidden");
                const detached = document.createElement("div");
                detached.id = "detached";
                globalThis.__intersectionBatchRecords = [];
                const first = new IntersectionObserver(entries => {
                    __intersectionBatchRecords.push(entries.map(entry => [
                        entry.target.id,
                        entry.isIntersecting,
                    ]));
                }, { root });
                const second = new IntersectionObserver(entries => {
                    __intersectionBatchRecords.push(entries.map(entry => [
                        entry.target.id,
                        entry.isIntersecting,
                    ]));
                }, { root });
                first.observe(a);
                first.observe(b);
                first.observe(hidden);
                first.observe(detached);
                // The second observer shares its target, root, and clip ancestor
                // with the first and must not duplicate any native measurements.
                second.observe(b);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();

        assert_eq!(
            rt.evaluate(
                r#"[
                    __intersectionBulkCalls,
                    __intersectionBulkSizes,
                    __intersectionLegacyGeometryCalls,
                    __intersectionComputedStyleCalls,
                    __intersectionBatchRecords,
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                1,
                [6],
                0,
                0,
                [
                    [
                        ["a", true],
                        ["b", true],
                        ["hidden", false],
                        ["detached", false],
                    ],
                    [["b", true]],
                ],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_delivers_document_batch_before_callback_posted_tasks() {
        let dom = parse_html(
            r#"<html><body><div id="first"></div><div id="second"></div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.run_page_init();
        rt.execute_script(
            "intersection-document-delivery-batch",
            r#"
                globalThis.__intersectionDeliveryOrder = [];
                const first = new IntersectionObserver(() => {
                    __intersectionDeliveryOrder.push("first-observer");
                    scheduler.postTask(() => {
                        __intersectionDeliveryOrder.push("callback-posted-task");
                    }, { priority: "user-blocking" });
                });
                const second = new IntersectionObserver(() => {
                    __intersectionDeliveryOrder.push("second-observer");
                });
                first.observe(document.getElementById("first"));
                second.observe(document.getElementById("second"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__intersectionDeliveryOrder").unwrap(),
            serde_json::json!([
                "first-observer",
                "second-observer",
                "callback-posted-task",
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_element_root_uses_live_padding_box_and_scroll() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="root" style="position:absolute;left:10px;top:20px;width:100px;
                     height:80px;padding:10px;border:5px solid;overflow:auto">
                    <div style="height:100px"></div>
                    <div id="target" style="height:20px"></div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(300.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const records = [];
                    const root = document.getElementById("root");
                    const observer = new IntersectionObserver(entries => {
                        records.push(...entries.map(entry => ({
                            intersecting: entry.isIntersecting,
                            ratio: entry.intersectionRatio,
                            root: [
                                entry.rootBounds.x, entry.rootBounds.y,
                                entry.rootBounds.width, entry.rootBounds.height,
                            ],
                            intersection: [
                                entry.intersectionRect.x, entry.intersectionRect.y,
                                entry.intersectionRect.width, entry.intersectionRect.height,
                            ],
                        })));
                    }, { root, threshold: [0, 1] });
                    observer.observe(document.getElementById("target"));
                    setTimeout(() => { root.scrollTop = 999; }, 25);
                    setTimeout(() => resolve(records), 60);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                {
                    "intersecting": false,
                    "ratio": 0,
                    "root": [15, 25, 120, 100],
                    "intersection": [0, 0, 0, 0],
                },
                {
                    "intersecting": true,
                    "ratio": 1,
                    "root": [15, 25, 120, 100],
                    "intersection": [25, 95, 100, 20],
                },
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_clips_through_intermediate_overflow_ancestors() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="root" style="position:absolute;left:10px;top:20px;
                     width:300px;height:300px;overflow:visible">
                    <div id="clip" style="width:100px;height:100px;overflow:hidden">
                        <div style="height:150px"></div>
                        <div id="target" style="height:20px"></div>
                    </div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 400.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const records = [];
                    const clip = document.getElementById("clip");
                    const observer = new IntersectionObserver(entries => {
                        records.push(...entries.map(entry => [
                            entry.isIntersecting,
                            entry.intersectionRatio,
                            [
                                entry.intersectionRect.x,
                                entry.intersectionRect.y,
                                entry.intersectionRect.width,
                                entry.intersectionRect.height,
                            ],
                        ]));
                    }, {
                        root: document.getElementById("root"),
                        threshold: [0, 1],
                    });
                    observer.observe(document.getElementById("target"));
                    setTimeout(() => { clip.scrollTop = 999; }, 25);
                    setTimeout(() => resolve(records), 60);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        // Chromium reports the initial target as non-intersecting: although it
        // lies inside the explicit root, the intermediate overflow container
        // clips it. Programmatic scrolling then reveals the complete box.
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                [false, 0, [0, 0, 0, 0]],
                [true, 1, [10, 100, 100, 20]],
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_initial_geometry_waits_for_one_render_checkpoint() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="first"></div><div id="second"></div></body></html>"#,
        );
        rt.execute_script(
            "intersection-render-checkpoint",
            r#"
                globalThis.__ioOrder = ["sync"];
                globalThis.__ioReads = 0;
                const first = document.getElementById("first");
                const second = document.getElementById("second");
                for (const element of [first, second]) {
                    const nativeRect = element.getBoundingClientRect.bind(element);
                    element.getBoundingClientRect = () => {
                        __ioReads++;
                        return nativeRect();
                    };
                }
                const observer = new IntersectionObserver(
                    () => __ioOrder.push("observer")
                );
                observer.observe(first);
                observer.observe(second);
                Promise.resolve().then(() => __ioOrder.push("microtask"));
                __ioOrder.push("after-observe-" + __ioReads);
            "#,
        )
        .unwrap();

        assert_eq!(
            rt.evaluate("[__ioOrder, __ioReads]").unwrap(),
            serde_json::json!([["sync", "after-observe-0", "microtask"], 0])
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        #[cfg(feature = "render")]
        let expected_geometry_reads = 0;
        #[cfg(not(feature = "render"))]
        let expected_geometry_reads = 2;
        assert_eq!(
            rt.evaluate("[__ioOrder, __ioReads]").unwrap(),
            serde_json::json!([
                ["sync", "after-observe-0", "microtask", "observer"],
                expected_geometry_reads,
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_honors_root_margin_zero_area_and_no_fake_refires() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;position:relative">
                <div style="height:110px"></div>
                <div id="margin-target" style="height:10px"></div>
                <div id="zero" style="position:absolute;left:20px;top:50px;width:0;height:0"></div>
                <div id="root" style="height:20px;overflow:auto"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const marginRecords = [], zeroRecords = [];
                    const marginObserver = new IntersectionObserver(
                        entries => marginRecords.push(...entries.map(entry => [
                            entry.isIntersecting,
                            entry.intersectionRatio,
                            entry.rootBounds.bottom,
                        ])),
                        { rootMargin: "0px 0px 20px", threshold: [0, 1] }
                    );
                    const zeroObserver = new IntersectionObserver(
                        entries => zeroRecords.push(...entries.map(entry => [
                            entry.isIntersecting,
                            entry.intersectionRatio,
                        ]))
                    );
                    marginObserver.observe(document.getElementById("margin-target"));
                    zeroObserver.observe(document.getElementById("zero"));
                    let elementRoot = false;
                    try {
                        const rooted = new IntersectionObserver(() => {}, {
                            root: document.getElementById("root")
                        });
                        elementRoot = rooted.root === document.getElementById("root");
                    } catch (error) {
                        elementRoot = error.name;
                    }
                    setTimeout(() => resolve([
                        marginRecords, zeroRecords, elementRoot,
                        marginObserver.rootMargin, marginObserver.thresholds,
                    ]), 200);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                [[true, 1, 120]],
                [[true, 1]],
                true,
                "0px 0px 20px 0px",
                [0, 1],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_does_not_refire_while_target_stays_intersecting() {
        let dom = parse_html(
            r#"<html><body>
                <div id="feed"></div>
                <div id="sentinel"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1280.0, 720.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const feed = document.getElementById("feed");
                    let loaded = 0;
                    const observer = new IntersectionObserver(entries => {
                        for (const entry of entries) {
                            if (!entry.isIntersecting) continue;
                            for (let i = 0; i < 10; i++) {
                                const card = document.createElement("div");
                                card.textContent = "Item " + loaded++;
                                feed.appendChild(card);
                            }
                        }
                    });
                    observer.observe(document.getElementById("sentinel"));
                    setTimeout(() => resolve([
                        loaded,
                        feed.querySelectorAll("div").length,
                    ]), 200);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!([10, 10]));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_can_be_reused_after_disconnect() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="stale" style="height:10px"></div>
                <div id="first" style="height:10px"></div>
                <div id="second" style="height:10px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const deliveries = [];
                    const observer = new IntersectionObserver(entries => {
                        deliveries.push(entries.map(entry => entry.target.id));
                        if (deliveries.length === 1) {
                            observer.disconnect();
                            observer.observe(document.getElementById("second"));
                        } else {
                            observer.disconnect();
                            resolve([
                                deliveries,
                                globalThis.__intersectionObservers.length,
                            ]);
                        }
                    });

                    // A pending record from before disconnect must be discarded.
                    observer.observe(document.getElementById("stale"));
                    observer.disconnect();
                    observer.observe(document.getElementById("first"));
                    setTimeout(() => resolve(["timed out"]), 100);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([[["first"], ["second"]], 0,])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_recomputes_after_style_mutation_and_resize() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="spacer" style="height:150px"></div>
                <div id="target" style="height:20px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "intersection-mutation",
            r#"
                globalThis.__ioRecords = [];
                globalThis.__io = new IntersectionObserver(entries => {
                    __ioRecords.push(...entries.map(entry => [
                        entry.isIntersecting,
                        Math.round(entry.boundingClientRect.top),
                    ]));
                });
                __io.observe(document.getElementById("target"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();

        rt.evaluate(r#"document.getElementById("spacer").setAttribute("style", "height:120px")"#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__ioRecords").unwrap(),
            serde_json::json!([[false, 150]])
        );

        rt.set_viewport(200.0, 160.0);
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__ioRecords").unwrap(),
            serde_json::json!([[false, 150], [true, 120]])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn intersection_observer_recomputes_after_root_scroll() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:150px"></div>
                <div id="target" style="height:20px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "intersection-root-scroll",
            r#"
                globalThis.__rootScrollIoRecords = [];
                globalThis.__rootScrollIo = new IntersectionObserver(entries => {
                    __rootScrollIoRecords.push(...entries.map(entry => [
                        entry.isIntersecting,
                        Math.round(entry.boundingClientRect.top),
                    ]));
                });
                __rootScrollIo.observe(document.getElementById("target"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        rt.evaluate("window.scrollTo(0, 100)").unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__rootScrollIoRecords").unwrap(),
            serde_json::json!([[false, 150], [true, 50]])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn scroll_into_view_aligns_the_root_viewport_and_clamps() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:300px"></div>
                <div id="target" style="height:40px"></div>
                <div style="height:340px"></div>
                <div id="bottom" style="height:20px"></div>
                <div id="fixed" style="position:fixed;top:10px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const target = document.getElementById("target");
                const bottom = document.getElementById("bottom");
                const fixed = document.getElementById("fixed");
                target.scrollIntoView();
                const start = scrollY;
                scrollTo(0, 0);
                target.scrollIntoView({ block: "center" });
                const center = scrollY;
                scrollTo(0, 0);
                target.scrollIntoView({ block: "end" });
                const end = scrollY;
                scrollTo(0, 0);
                target.scrollIntoView({ block: "nearest" });
                const nearestOutside = scrollY;
                scrollTo(0, 250);
                target.scrollIntoView({ block: "nearest" });
                const nearestVisible = scrollY;
                fixed.scrollIntoView();
                const afterFixed = scrollY;
                bottom.scrollIntoView({ block: "start" });
                const clamped = scrollY;
                const max = document.documentElement.scrollHeight - innerHeight;
                return [
                    start, center, end, nearestOutside, nearestVisible,
                    afterFixed, clamped, max,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([300, 270, 240, 240, 250, 250, 600, 600])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn scroll_into_view_emits_events_only_when_the_root_moves() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:300px"></div>
                <div id="target" style="height:40px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        rt.evaluate("window.scrollTo(0, 250)").unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        let no_op = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    document.getElementById("target").scrollIntoView({ block: "nearest" });
                    setTimeout(() => resolve([win, doc, scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(no_op.value.unwrap(), serde_json::json!([0, 0, 250]));

        rt.evaluate("window.scrollTo(0, 0)").unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        let moved = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    document.getElementById("target").scrollIntoView({ block: "center" });
                    setTimeout(() => resolve([win, doc, scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(moved.value.unwrap(), serde_json::json!([1, 1, 270]));
    }

    /// Issue #469: FILTER_SKIP leaves a skipped node's children eligible, so
    /// firstChild()/lastChild() must descend into them. FILTER_REJECT must not.
    #[test]
    fn tree_walker_child_movers_descend_on_skip_but_not_on_reject() {
        let mut rt = setup_runtime(r#"<div id="root"><section><a></a><b></b></section></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                function mover(verdict, method) {
                    const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                        acceptNode(node) {
                            return node.tagName === 'SECTION' ? verdict : NodeFilter.FILTER_ACCEPT;
                        }
                    });
                    const found = w[method]();
                    return found ? found.tagName : null;
                }
                return [
                    mover(NodeFilter.FILTER_SKIP, 'firstChild'),
                    mover(NodeFilter.FILTER_SKIP, 'lastChild'),
                    mover(NodeFilter.FILTER_REJECT, 'firstChild'),
                    mover(NodeFilter.FILTER_REJECT, 'lastChild'),
                ];
                "#,
            )
            .unwrap();
        // SKIP descends into <section>; REJECT prunes it and finds nothing else.
        assert_eq!(result, serde_json::json!(["A", "B", null, null]));
    }

    /// Issue #469: nextSibling()/previousSibling() must descend into a skipped
    /// sibling's subtree rather than stepping straight over it.
    #[test]
    fn tree_walker_sibling_movers_descend_into_skipped_siblings() {
        let mut rt = setup_runtime(
            r#"<div id="root"><p id="start"></p><section><a></a></section><q></q></div>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                function mover(verdict, method, from) {
                    const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                        acceptNode(node) {
                            return node.tagName === 'SECTION' ? verdict : NodeFilter.FILTER_ACCEPT;
                        }
                    });
                    w.currentNode = document.getElementById(from);
                    const found = w[method]();
                    return found ? found.tagName : null;
                }
                return [
                    // <section> is skipped, so its child <a> is the next sibling.
                    mover(NodeFilter.FILTER_SKIP, 'nextSibling', 'start'),
                    // Rejected: the subtree is off-limits, so skip past to <q>.
                    mover(NodeFilter.FILTER_REJECT, 'nextSibling', 'start'),
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["A", "Q"]));
    }

    /// Issue #469: the backward sibling mover descends to *last* children.
    #[test]
    fn tree_walker_previous_sibling_descends_to_last_child() {
        let mut rt = setup_runtime(
            r#"<div id="root"><section><a></a><b></b></section><p id="start"></p></div>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'SECTION'
                            ? NodeFilter.FILTER_SKIP
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                w.currentNode = document.getElementById('start');
                const found = w.previousSibling();
                return found ? found.tagName : null;
                "#,
            )
            .unwrap();
        // Reverse order descends to <section>'s last child, not its first.
        assert_eq!(result, serde_json::json!("B"));
    }

    #[test]
    fn append_child_flattens_document_fragment() {
        let mut rt = setup_runtime(r#"<main id="host"></main>"#);
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                const fragment = document.createDocumentFragment();
                const first = document.createElement('article');
                const second = document.createElement('article');
                first.id = 'first';
                second.id = 'second';
                first.className = second.className = 'quote';
                fragment.appendChild(first);
                fragment.appendChild(second);

                const returned = host.appendChild(fragment);
                return [
                    returned === fragment,
                    Array.from(host.children).map(node => node.id),
                    host.querySelectorAll('.quote').length,
                    fragment.childNodes.length,
                    first.parentNode === host,
                    first.parentElement === host,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, ["first", "second"], 2, 0, true, true])
        );
    }

    #[test]
    fn insert_before_flattens_document_fragment_in_order() {
        let mut rt = setup_runtime(r#"<main id="host"><article id="last"></article></main>"#);
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                const last = document.getElementById('last');
                const fragment = document.createDocumentFragment();
                const first = document.createElement('article');
                const second = document.createElement('article');
                first.id = 'first';
                second.id = 'second';
                fragment.appendChild(first);
                fragment.appendChild(second);

                const returned = host.insertBefore(fragment, last);
                return [
                    returned === fragment,
                    Array.from(host.children).map(node => node.id),
                    fragment.childNodes.length,
                    first.parentElement === host,
                    second.parentElement === host,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, ["first", "second", "last"], 0, true, true])
        );
    }

    #[test]
    fn replace_child_flattens_document_fragment_and_removes_old_child() {
        let mut rt = setup_runtime(
            r#"<main id="host"><article id="old"></article><article id="tail"></article></main>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                const old = document.getElementById('old');
                const fragment = document.createDocumentFragment();
                const first = document.createElement('article');
                const second = document.createElement('article');
                first.id = 'first';
                second.id = 'second';
                fragment.appendChild(first);
                fragment.appendChild(second);

                const returned = host.replaceChild(fragment, old);
                return [
                    returned === old,
                    Array.from(host.children).map(node => node.id),
                    fragment.childNodes.length,
                    old.parentNode === null,
                    first.parentElement === host,
                    second.parentElement === host,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, ["first", "second", "tail"], 0, true, true, true])
        );
    }

    #[test]
    fn test_inner_html() {
        let mut rt = setup_runtime(r#"<div id="x"><p>Hello</p></div>"#);
        let html = rt
            .evaluate("document.getElementById('x').innerHTML")
            .unwrap();
        assert!(html.as_str().unwrap().contains("<p>"));
    }

    #[test]
    fn template_inner_html_preserves_table_fragments() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const template = document.createElement('template');
                template.innerHTML = '<tr><td>first</td><td>second</td></tr>';
                const clone = template.content.firstChild.cloneNode(true);
                return [
                    clone.tagName,
                    clone.firstElementChild.tagName,
                    clone.firstElementChild.children.length,
                    clone.textContent,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["TR", "TD", 0, "firstsecond"]));
    }

    #[test]
    fn document_exposes_parent_node_element_children_api() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let result = rt
            .evaluate(
                "return [document.firstElementChild === document.documentElement,\
                         document.lastElementChild === document.documentElement,\
                         document.children.length, document.childElementCount];",
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true, 1, 1]));
    }

    #[test]
    fn atob_decodes_large_payload_without_argument_stack_overflow() {
        let mut rt = setup_runtime("<html><body></body></html>");
        // 60k four-character groups decode to 180k bytes, comfortably above
        // V8's maximum argument count for a single fromCharCode(...bytes).
        let encoded = "QUFB".repeat(60_000);
        let result = rt.evaluate(&format!("atob('{}').length", encoded)).unwrap();
        assert_eq!(result.as_f64().unwrap() as usize, 180_000);
    }

    #[test]
    fn navigation_api_updates_current_entry_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                (() => {
                    navigation.updateCurrentEntry({state: {route: 'home'}});
                    const first = navigation.currentEntry;
                    navigation.navigate('/docs', {state: {route: 'docs'}});
                    return [
                        typeof navigation.updateCurrentEntry,
                        first.getState().route,
                        navigation.currentEntry.getState().route,
                        navigation.currentEntry.url,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["function", "home", "docs", "http://example.com/docs"])
        );
    }

    #[test]
    fn inline_stylesheet_cssom_lists_and_rules_are_live_same_objects() {
        let mut rt = setup_runtime(
            r#"<html><head>
                <style id="first">.one { color:red } .two { width:20px }</style>
                <style id="second">.three { display:block }</style>
            </head><body></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const list = document.styleSheets;
                    const style = document.getElementById('first');
                    const sheet = style.sheet;
                    const rules = sheet.cssRules;
                    const firstRule = rules[0];
                    const initial = [
                        list === document.styleSheets,
                        list.length,
                        list[0] === sheet,
                        list.item(0) === sheet,
                        list.item(9),
                        sheet.ownerNode === style,
                        rules === sheet.cssRules,
                        rules.length,
                        rules.item(0) === firstRule,
                        firstRule instanceof CSSRule,
                        firstRule instanceof CSSStyleRule,
                        firstRule.type,
                        firstRule.selectorText,
                        firstRule.style.color,
                        firstRule.parentStyleSheet === sheet,
                    ];

                    sheet.insertRule('.middle { height: 30px; }', 1);
                    const inserted = [
                        rules.length,
                        rules[0] === firstRule,
                        rules[1].selectorText,
                        style.textContent.includes('.middle'),
                    ];
                    sheet.deleteRule(1);

                    const extra = document.createElement('style');
                    document.head.appendChild(extra);
                    const afterAppend = list.length;
                    const emptySheet = extra.sheet;
                    const emptyIdentity = emptySheet === list[2]
                        && emptySheet.cssRules.length === 0;
                    extra.textContent = '.extra { opacity:.5 }';
                    const emptyBecameLive = emptySheet.cssRules.length === 1;
                    extra.remove();
                    const afterRemove = list.length;

                    style.textContent = '.replacement { padding: 4px; }';
                    const reparsed = [
                        style.sheet === sheet,
                        sheet.cssRules === rules,
                        rules.length,
                        rules[0].selectorText,
                        rules[0].style.padding,
                    ];
                    style.remove();
                    const disconnected = style.sheet === null
                        && sheet.ownerNode === null
                        && list.length === 1;
                    document.head.appendChild(style);
                    const reconnected = style.sheet !== sheet
                        && style.sheet.ownerNode === style
                        && list.length === 2;

                    const left = document.createElement('div');
                    const right = document.createElement('div');
                    document.body.append(left, right);
                    const moving = document.createElement('style');
                    moving.textContent = '.moving { color: red }';
                    left.appendChild(moving);
                    const beforeMove = moving.sheet;
                    right.appendChild(moving);
                    const reparented = beforeMove.ownerNode === null
                        && moving.sheet !== beforeMove
                        && moving.sheet.ownerNode === moving;
                    right.remove();
                    left.remove();

                    const bulk = document.createElement('div');
                    document.body.appendChild(bulk);
                    bulk.innerHTML = '<style>.bulk { color: blue }</style><span></span>';
                    const bulkSheet = bulk.querySelector('style').sheet;
                    bulk.innerHTML = '';
                    const innerHTMLDetached = bulkSheet.ownerNode === null;
                    bulk.innerHTML = '<section><style>.text { color: green }</style></section>';
                    const textSheet = bulk.querySelector('style').sheet;
                    bulk.textContent = '';
                    const textContentDetached = textSheet.ownerNode === null;
                    bulk.remove();
                    return {
                        initial, inserted, afterAppend, emptyIdentity, emptyBecameLive,
                        afterRemove, reparsed, disconnected, reconnected, reparented,
                        innerHTMLDetached, textContentDetached,
                    };
                })()
                "#,
            )
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!({
                "initial": [true, 2, true, true, null, true, true, 2, true,
                    true, true, 1, ".one", "red", true],
                "inserted": [3, true, ".middle", true],
                "afterAppend": 3,
                "emptyIdentity": true,
                "emptyBecameLive": true,
                "afterRemove": 2,
                "reparsed": [true, true, 1, ".replacement", "4px"],
                "disconnected": true,
                "reconnected": true,
                "reparented": true,
                "innerHTMLDetached": true,
                "textContentDetached": true,
            })
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn stylesheet_cssom_mutations_update_the_live_cascade() {
        let mut rt = setup_runtime(
            r#"<html style="margin:0"><head>
                <style>#box { width:11px; height:10px }</style>
                </head><body style="margin:0"><div id="box"></div></body></html>"#,
        );
        rt.set_viewport(200.0, 100.0);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const style = document.createElement('style');
                    style.type = 'text/css';
                    style.setAttribute('data-framer-css', 'true');
                    document.head.appendChild(style);
                    const sheet = style.sheet;
                    const rules = sheet.cssRules;
                    const termius = [
                        !!sheet,
                        rules.length,
                        document.styleSheets[1] === sheet,
                        sheet.ownerNode === style,
                    ];
                    sheet.insertRule('#box { width:73px; height:10px; }', rules.length);
                    termius.push(rules.length);
                    sheet.insertRule('.other { color: blue; }', rules.length);
                    termius.push(rules.length);
                    sheet.insertRule('.semicolon { content: "a;b"; background-image: url("data:image/svg+xml;utf8,<svg/>"); }', rules.length);
                    const semicolonValues = [
                        rules[2].style.content,
                        rules[2].style.backgroundImage,
                    ];
                    let multiRuleSyntaxError = false;
                    try {
                        sheet.insertRule('.invalid-a {} .invalid-b {}', rules.length);
                    } catch (error) {
                        multiRuleSyntaxError = error?.name === 'SyntaxError';
                    }
                    const inserted = document.getElementById('box').getBoundingClientRect().width;
                    rules[0].style.setProperty('width', '91px');
                    const edited = document.getElementById('box').getBoundingClientRect().width;
                    const computed = getComputedStyle(document.getElementById('box')).width;
                    sheet.deleteRule(0);
                    const deleted = document.getElementById('box').getBoundingClientRect().width;
                    sheet.replaceSync('#box { width:64px } .other { color: blue }');
                    const replaced = document.getElementById('box').getBoundingClientRect().width;
                    return [termius, semicolonValues, multiRuleSyntaxError, inserted, edited, computed, deleted,
                            replaced, rules.length, rules[0].selectorText,
                            style.textContent.includes('.other')];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, 0, true, true, 1, 2],
                ["\"a;b\"", "url(\"data:image/svg+xml;utf8,<svg/>\")"], true,
                73, 91, "91px", 11, 64, 2, "#box", true
            ])
        );
    }

    #[test]
    fn adopted_stylesheets_materialize_into_the_document() {
        let mut rt =
            setup_runtime("<html><head></head><body><div class=\"card\"></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const sheet = new CSSStyleSheet();
                    document.adoptedStyleSheets.push(sheet);
                    sheet.insertRule('.card { display: flex; color: red; }', 0);
                    const node = document.querySelector('style[data-obscura-adopted]');
                    const inserted = node.textContent;
                    sheet.replaceSync('.card { content: "a;b"; background-image: url("data:image/svg+xml;utf8,<svg/>"); }');
                    const preserved = [
                        sheet.cssRules[0].style.content,
                        sheet.cssRules[0].style.backgroundImage,
                        node.textContent.includes('a;b'),
                        node.textContent.includes('svg+xml;utf8'),
                    ];
                    sheet.deleteRule(0);
                    return [
                        document.adoptedStyleSheets.length,
                        document.querySelectorAll('style[data-obscura-adopted]').length,
                        inserted.includes('display: flex'),
                        preserved,
                        node.textContent,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                1, 1, true,
                ["\"a;b\"", "url(\"data:image/svg+xml;utf8,<svg/>\")", true, true],
                ""
            ])
        );
    }

    #[test]
    fn shadow_stylesheet_lists_and_adoption_are_live_across_roots() {
        let mut rt = setup_runtime(
            "<html><head></head><body><div id='one'></div><div id='two'></div></body></html>",
        );
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const first = document.getElementById('one').attachShadow({ mode: 'open' });
                    const second = document.getElementById('two').attachShadow({ mode: 'open' });
                    const inline = document.createElement('style');
                    inline.textContent = '.local { width: 17px }';
                    first.appendChild(inline);
                    const inlineSheet = inline.sheet;
                    const firstList = first.styleSheets;
                    const firstAdopted = first.adoptedStyleSheets;
                    const secondAdopted = second.adoptedStyleSheets;
                    const documentAdopted = document.adoptedStyleSheets;

                    const shared = new CSSStyleSheet();
                    first.adoptedStyleSheets = [shared];
                    second.adoptedStyleSheets.push(shared);
                    document.adoptedStyleSheets = [shared];

                    const firstNode = first.querySelector('style[data-obscura-adopted]');
                    const secondNode = second.querySelector('style[data-obscura-adopted]');
                    const documentNode = document.querySelector('style[data-obscura-adopted]');
                    const initial = [
                        first.styleSheets === firstList,
                        firstList.length,
                        firstList[0] === inlineSheet,
                        firstList.item(0) === inlineSheet,
                        second.styleSheets === second.styleSheets,
                        second.styleSheets.length,
                        first.adoptedStyleSheets === firstAdopted,
                        second.adoptedStyleSheets === secondAdopted,
                        document.adoptedStyleSheets === documentAdopted,
                        firstAdopted.length,
                        secondAdopted.length,
                        documentAdopted.length,
                        firstNode.parentNode === first,
                        secondNode.parentNode === second,
                        documentNode.parentNode === document.head,
                    ];

                    shared.insertRule('.shared { width: 31px }', 0);
                    const synchronized = [firstNode, secondNode, documentNode]
                        .map(node => node.textContent.includes('width: 31px'));

                    second.adoptedStyleSheets = [];
                    shared.replaceSync('.shared { width: 47px }');
                    const afterRemoval = [
                        second.adoptedStyleSheets === secondAdopted,
                        secondAdopted.length,
                        secondNode.parentNode,
                        second.querySelectorAll('style[data-obscura-adopted]').length,
                        firstNode.textContent.includes('width: 47px'),
                        documentNode.textContent.includes('width: 47px'),
                        secondNode.textContent.includes('width: 31px'),
                    ];

                    inline.remove();
                    const inlineRemoval = [
                        first.styleSheets === firstList,
                        firstList.length,
                        inlineSheet.ownerNode,
                    ];
                    return { initial, synchronized, afterRemoval, inlineRemoval };
                })()
                "#,
            )
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!({
                "initial": [
                    true, 1, true, true, true, 0,
                    true, true, true, 1, 1, 1,
                    true, true, true
                ],
                "synchronized": [true, true, true],
                "afterRemoval": [true, 0, null, 0, true, true, true],
                "inlineRemoval": [true, 0, null],
            })
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn shadow_adopted_stylesheets_apply_and_sync_the_live_cascade() {
        let mut rt = setup_runtime(
            r#"<html style="margin:0"><head>
                <style>.target { width:11px; height:10px }</style>
                </head><body style="margin:0">
                <div id="one"></div><div id="two"></div><div class="target" id="outside"></div>
                </body></html>"#,
        );
        rt.set_viewport(200.0, 100.0);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const first = document.getElementById('one').attachShadow({ mode: 'open' });
                    const second = document.getElementById('two').attachShadow({ mode: 'open' });
                    first.innerHTML = '<style>.target { width:18px; height:10px }</style><div class="target"></div>';
                    second.innerHTML = '<style>.target { width:23px; height:10px }</style><div class="target"></div>';
                    const firstTarget = first.querySelector('.target');
                    const secondTarget = second.querySelector('.target');
                    const outside = document.getElementById('outside');
                    const widths = () => [firstTarget, secondTarget, outside]
                        .map(node => node.getBoundingClientRect().width);

                    const inline = widths();
                    const shared = new CSSStyleSheet();
                    shared.replaceSync('.target { width:42px; height:10px }');
                    first.adoptedStyleSheets = [shared];
                    second.adoptedStyleSheets = [shared];
                    document.adoptedStyleSheets = [shared];
                    const adopted = widths();

                    shared.cssRules[0].style.setProperty('width', '67px');
                    const mutated = widths();

                    second.adoptedStyleSheets = [];
                    document.adoptedStyleSheets = [];
                    const selectivelyRemoved = widths();

                    shared.replaceSync('.target { width:81px; height:10px }');
                    const remainingRootUpdated = widths();
                    return [
                        inline, adopted, mutated, selectivelyRemoved, remainingRootUpdated,
                        first.styleSheets.length,
                        second.styleSheets.length,
                    ];
                })()
                "#,
            )
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!([
                [18, 23, 11],
                [42, 42, 42],
                [67, 67, 67],
                [67, 23, 11],
                [81, 23, 11],
                1,
                1,
            ])
        );
    }

    #[test]
    fn unavailable_webgl_context_does_not_claim_success() {
        let mut rt = setup_runtime("<html><body><canvas></canvas></body></html>");
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const canvas = document.querySelector('canvas');
                    const fallback = document.createElement('p');
                    if (!canvas.getContext('webgl')) {
                        fallback.textContent = 'static fallback';
                        document.body.appendChild(fallback);
                    }
                    return [
                        canvas.getContext('webgl'),
                        canvas.getContext('webgl2'),
                        canvas.getContext('experimental-webgl'),
                        fallback.isConnected,
                        fallback.textContent,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([null, null, null, true, "static fallback"])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    fn canvas_2d_live_backing_paints_immediately_with_scaling_clips_and_effects() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;width:64px;height:40px;background:#0000ff">
                <div style="position:absolute;left:4px;top:4px;width:18px;height:14px;overflow:hidden">
                  <canvas id="paint" width="2" height="1"
                    style="display:block;width:20px;height:10px;border:2px solid #ffff00;opacity:.5"></canvas>
                </div>
                <canvas id="blank" width="4" height="4"
                  style="position:absolute;left:30px;top:4px;width:10px;height:10px"></canvas>
                <canvas id="padding" width="2" height="1"
                  style="position:absolute;left:30px;top:20px;width:10px;height:4px;padding:2px 2px 2px 4px;border:1px solid #ffff00;background:#00ffff"></canvas>
                <div style="position:absolute;z-index:2;left:10px;top:7px;width:4px;height:4px;background:#ff00ff"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/canvas");
        rt.set_viewport(64.0, 40.0);
        rt.run_page_init();

        let blank_api = rt
            .evaluate(
                r#"(() => {
                    const blank = document.getElementById('blank');
                    const encoded = blank.toDataURL();
                    const blankContext = blank.getContext('2d');
                    const pixels = blankContext.getImageData(0, 0, 4, 4).data;
                    blankContext.fillStyle = '#ff0000'; blankContext.fillRect(0, 0, 1, 1);
                    blank.width = 6;
                    const resetPixel = blankContext.getImageData(0, 0, 1, 1).data;
                    const untouched = document.createElement('canvas');
                    const defaultEncoded = untouched.toDataURL();
                    const defaultPixels = untouched.getContext('2d').getImageData(0, 0, 1, 1).data;
                    return [
                      blank instanceof HTMLCanvasElement,
                      typeof document.createElement('div').getContext,
                      blank.width, blank.height, blank.getContext('2d') === blankContext,
                      encoded.startsWith('data:image/png;base64,'),
                      atob(encoded.split(',')[1]).charCodeAt(25) === 6,
                      Array.from(pixels).every((value, index) => index % 4 !== 3 || value === 0),
                      untouched.width, untouched.height,
                      defaultEncoded.startsWith('data:image/png;base64,'),
                      Array.from(defaultPixels),
                      Array.from(resetPixel),
                    ];
                })()"#,
            )
            .expect("transparent default canvas API");
        assert_eq!(
            blank_api,
            serde_json::json!([
                true,
                "undefined",
                6,
                4,
                true,
                true,
                true,
                true,
                300,
                150,
                true,
                [0, 0, 0, 0],
                [0, 0, 0, 0]
            ])
        );

        // Prepare layout before drawing so the assertions below prove canvas
        // damage retains layout, while the following capture proves pixels
        // are read from the live backing immediately after the script task.
        {
            let mut state = rt.state.borrow_mut();
            ensure_resolved_scroll(&mut state).expect("initial resolved canvas scroll");
        }
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };
        let activity_before = rt.activity_generation();
        rt.execute_script(
            "canvas-fill",
            r#"const canvas = document.getElementById('paint');
               const ctx = canvas.getContext('2d');
               ctx.fillStyle = '#ff0000'; ctx.fillRect(0, 0, 1, 1);
               ctx.fillStyle = '#00ff00'; ctx.fillRect(1, 0, 1, 1);
               const padding = document.getElementById('padding').getContext('2d');
               padding.fillStyle = '#ff0000'; padding.fillRect(0, 0, 1, 1);
               padding.fillStyle = '#00ff00'; padding.fillRect(1, 0, 1, 1);"#,
        )
        .expect("fill live canvas backing");
        assert!(rt.activity_generation() > activity_before);
        assert_eq!(
            rt.state.borrow().prepared_render.as_ref().unwrap() as *const _ as usize,
            prepared_address,
            "canvas damage must not invalidate retained layout"
        );

        let pixmap = {
            let mut state = rt.state.borrow_mut();
            ensure_resolved_scroll(&mut state).expect("resolved canvas scroll");
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = &mut *state;
            let (_, scroll) = resolved_scroll.as_ref().expect("scroll snapshot");
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::paint_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
                dom.as_ref().expect("canvas DOM"),
                prepared_render.as_mut().expect("prepared canvas layout"),
                render_resources,
                scroll,
                [255, 255, 255, 255],
                &canvas_surfaces,
            )
            .expect("canvas pixmap")
        };

        let red_half = pixmap.pixel(7, 8).expect("scaled red canvas pixel");
        assert!(red_half.red() > 100 && red_half.blue() > 100 && red_half.green() < 20);
        // Bilinear filtering blends at the exact source-pixel transition
        // (x=16), so sample several CSS pixels into the green half.
        let green_half = pixmap.pixel(19, 8).expect("scaled green canvas pixel");
        assert!(
            green_half.green() > 45 && green_half.blue() > 100 && green_half.red() < 20,
            "pixel at (19, 8) was rgba({}, {}, {}, {})",
            green_half.red(),
            green_half.green(),
            green_half.blue(),
            green_half.alpha()
        );
        let clipped = pixmap.pixel(23, 8).expect("outside overflow clip");
        assert_eq!((clipped.red(), clipped.green(), clipped.blue()), (0, 0, 255));
        let blank = pixmap.pixel(34, 8).expect("transparent blank canvas");
        assert_eq!((blank.red(), blank.green(), blank.blue()), (0, 0, 255));
        let overlay = pixmap.pixel(11, 8).expect("higher z-index overlay");
        assert!(overlay.red() > 240 && overlay.blue() > 240 && overlay.green() < 20);
        let border = pixmap.pixel(4, 8).expect("canvas border above content");
        assert!(border.red() > 100 && border.green() > 100 && border.blue() > 100);
        let padding = pixmap.pixel(33, 23).expect("canvas padding pixel");
        assert_eq!(
            (padding.red(), padding.green(), padding.blue()),
            (0, 255, 255),
            "canvas pixels must not cover authored padding"
        );
        let padded_content = pixmap.pixel(36, 23).expect("padded canvas content pixel");
        assert!(
            padded_content.red() > 220
                && padded_content.green() < 40
                && padded_content.blue() < 40,
            "canvas bitmap must start at the CSS content-box origin"
        );

    }

    #[test]
    fn test_script_execution() {
        let mut rt = setup_runtime("<ul><li>A</li><li>B</li></ul>");
        rt.execute_script(
            "test",
            r#"
            globalThis.__result = [];
            document.querySelectorAll('li').forEach(function(el) {
                globalThis.__result.push(el.textContent);
            });
        "#,
        )
        .unwrap();
        let result = rt.evaluate("globalThis.__result").unwrap();
        assert_eq!(result, serde_json::json!(["A", "B"]));
    }

    #[test]
    fn page_var_declarations_do_not_collide_with_dom_interfaces() {
        let mut rt = setup_runtime("<html><body></body></html>");

        rt.execute_script(
            "legacy-node-guard",
            "if (!window.Node) { var Node = {}; } globalThis.__legacyNodeRan = true;",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__legacyNodeRan").unwrap(),
            serde_json::json!(true)
        );

        rt.execute_script(
            "page-element",
            "var Element = function PageElement() {}; globalThis.__createdTag = document.createElement('div').tagName;",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__createdTag").unwrap(),
            serde_json::json!("DIV")
        );
    }

    #[test]
    fn dynamic_script_status_bridge_is_hidden_and_idle() {
        let mut rt = setup_runtime("<html><body></body></html>");
        assert!(!rt.has_pending_dynamic_scripts());
        assert!(!rt.has_pending_load_delaying_scripts());
        assert_eq!(rt.next_pending_timeout_delay_ms(), None);
        assert_eq!(
            rt.evaluate("typeof __dynScriptBusy").unwrap(),
            serde_json::json!("undefined")
        );
        assert_eq!(
            rt.evaluate(
                "Object.getOwnPropertyNames(globalThis).includes('__obscura_hasPendingDynamicScripts')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Reflect.ownKeys(globalThis).includes('__obscura_hasPendingDynamicScripts')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Reflect.ownKeys(globalThis).includes('__obscura_hasPendingLoadDelayingScripts')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Object.getOwnPropertyNames(globalThis).includes('__obscura_nextPendingTimeoutDelay')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Reflect.ownKeys(globalThis).includes('__obscura_nextPendingTimeoutDelay')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
    }

    /// Regression test for #147: a TypeError in one script must not poison
    /// the runtime so that subsequent scripts (or DOM queries) collapse to
    /// empty. The reporter saw `--dump text` return 1 byte after offside.js
    /// crashed; that cascade should never happen.
    #[test]
    fn script_typeerror_does_not_poison_subsequent_execution() {
        let mut rt = setup_runtime("<html><body><p id=hit>BODY_TEXT</p></body></html>");

        // 1. First script throws the same flavor of error offside.js produced
        //    (`Cannot read properties of undefined (reading 'classList')`).
        let err = rt
            .execute_script("buggy", "var x; x.classList.add('y');")
            .unwrap_err();
        assert!(
            err.contains("classList") || err.contains("undefined"),
            "expected classList/undefined error, got: {}",
            err
        );

        // 2. The runtime must still be usable: a follow-up script runs.
        rt.execute_script("ok", "globalThis.__after_error = 'still alive';")
            .unwrap();
        let result = rt.evaluate("globalThis.__after_error").unwrap();
        assert_eq!(result, serde_json::json!("still alive"));

        // 3. DOM queries still work after the script error.
        let text = rt
            .evaluate("document.querySelector('#hit').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("BODY_TEXT"));
    }

    /// Regression test for #355: an explicit `throw` in one inline <script> must
    /// not stop later independent <script>s from running. Each <script> executes
    /// as its own `execute_script` call, mirroring how page.rs runs them, so a
    /// thrown error is reported but the next script still runs.
    #[test]
    fn thrown_error_in_one_script_does_not_stop_later_scripts() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script("s1", "globalThis.__ran1 = true;")
            .unwrap();
        let err = rt
            .execute_script(
                "s2",
                "throw new Error('only one instance of babel-polyfill is allowed');",
            )
            .unwrap_err();
        assert!(
            err.contains("babel-polyfill"),
            "expected the thrown message, got: {}",
            err
        );
        rt.execute_script("s3", "globalThis.__ran3 = true;")
            .unwrap();
        let ran = rt
            .evaluate("JSON.stringify([globalThis.__ran1 === true, globalThis.__ran3 === true])")
            .unwrap();
        assert_eq!(ran, serde_json::json!("[true,true]"));
    }

    /// Regression test for #356: the `in` operator and `Object.keys` must work on
    /// `el.style` (CSSStyleDeclaration) and `el.dataset` (DOMStringMap), `_props`
    /// must not leak, and cssText must serialize dashed names with a trailing
    /// semicolon.
    #[test]
    fn style_and_dataset_support_in_operator_and_keys() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const el = document.createElement('div');
                    el.style.color = 'red';
                    el.style.fontSize = '14px';
                    el.dataset.foo = 'bar';
                    const keys = Object.keys(el.style);
                    return JSON.stringify({
                        colorInStyle: 'color' in el.style,
                        objectFitInStyle: 'object-fit' in el.style,
                        keysHasSet: keys.includes('color') && keys.includes('fontSize'),
                        noPropsLeak: !keys.includes('_props'),
                        fooInDataset: 'foo' in el.dataset,
                        datasetKeys: Object.keys(el.dataset),
                        cssText: el.style.cssText,
                        length: el.style.length,
                        getByDash: el.style.getPropertyValue('font-size'),
                        reflectedAttribute: el.getAttribute('style')
                    });
                })()"#,
            )
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(p["colorInStyle"], true);
        assert_eq!(p["objectFitInStyle"], true);
        assert_eq!(p["keysHasSet"], true);
        assert_eq!(p["noPropsLeak"], true);
        assert_eq!(p["fooInDataset"], true);
        assert_eq!(p["datasetKeys"], serde_json::json!(["foo"]));
        assert_eq!(p["cssText"], "color: red; font-size: 14px;");
        assert_eq!(p["length"], 2);
        assert_eq!(p["getByDash"], "14px");
        assert_eq!(p["reflectedAttribute"], "color: red; font-size: 14px;");
    }

    #[test]
    fn dom_string_map_is_exposed_and_backs_dataset() {
        let mut rt = setup_runtime(r#"<div id="x" data-foo="bar"></div>"#);
        let result = rt
            .evaluate(
                r#"(() => {
                    const dataset = document.getElementById("x").dataset;
                    const interface = window.DOMStringMap;
                    const descriptor = Object.getOwnPropertyDescriptor(window, "DOMStringMap");
                    let illegalConstructor = false;
                    if (interface) {
                        try { new interface(); }
                        catch (error) { illegalConstructor = error instanceof TypeError; }
                    }
                    return JSON.stringify({
                        type: typeof interface,
                        instance: !!interface && dataset instanceof interface,
                        prototype: !!interface && Object.getPrototypeOf(dataset) === interface.prototype,
                        constructor: !!interface && dataset.constructor === interface,
                        tag: Object.prototype.toString.call(dataset),
                        enumerable: descriptor ? descriptor.enumerable : "missing",
                        illegalConstructor,
                        value: dataset.foo,
                    });
                })()"#,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "function",
                "instance": true,
                "prototype": true,
                "constructor": true,
                "tag": "[object DOMStringMap]",
                "enumerable": false,
                "illegalConstructor": true,
                "value": "bar",
            })
        );
    }

    #[test]
    fn style_declaration_reflects_and_removes_parsed_attributes() {
        let mut rt = setup_runtime(
            "<html><body><div id='icon' style='font-size: 0px; color: red'></div></body></html>",
        );
        let result = rt
            .evaluate(
                r#"(() => {
                    const el = document.getElementById('icon');
                    const before = [el.style.fontSize, el.style.color, el.style.length];
                    const removed = el.style.removeProperty('font-size');
                    return JSON.stringify({
                        before,
                        removed,
                        after: el.style.cssText,
                        attribute: el.getAttribute('style')
                    });
                })()"#,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(value["before"], serde_json::json!(["0px", "red", 2]));
        assert_eq!(value["removed"], "0px");
        assert_eq!(value["after"], "color: red;");
        assert_eq!(value["attribute"], "color: red;");
    }

    #[test]
    fn select_add_and_option_text_update_the_live_dom() {
        let mut rt = setup_runtime("<html><body><select id='language'></select></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const select = document.getElementById('language');
                    const english = document.createElement('option');
                    english.value = 'en';
                    english.text = 'English';
                    english.selected = true;
                    select.add(english);
                    const greek = document.createElement('option');
                    greek.value = 'el';
                    greek.text = 'Greek';
                    select.add(greek, 0);
                    return JSON.stringify({
                        labels: [...select.options].map(option => option.textContent),
                        selectedIndex: select.selectedIndex,
                        value: select.value,
                        html: select.outerHTML
                    });
                })()"#,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(value["labels"], serde_json::json!(["Greek", "English"]));
        assert_eq!(value["selectedIndex"], 1);
        assert_eq!(value["value"], "en");
        assert!(value["html"]
            .as_str()
            .unwrap()
            .contains(r#"<option value="en" selected="">English</option>"#));
    }

    /// Regression for #105: `element.querySelector` and `querySelectorAll`
    /// must scope to the receiver's subtree, not the whole document.
    #[test]
    fn element_query_selector_is_scoped_to_subtree() {
        let mut rt = setup_runtime(
            r#"<div id="a"><span class="x">in a</span></div><div id="b"><span class="x">in b</span></div>"#,
        );
        let text = rt
            .evaluate("document.getElementById('a').querySelector('.x').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("in a"));

        let count_in_a = rt
            .evaluate("document.getElementById('a').querySelectorAll('.x').length")
            .unwrap();
        assert_eq!(count_in_a.as_f64().unwrap() as i64, 1);

        // Document-scoped query still sees both.
        let count_doc = rt
            .evaluate("document.querySelectorAll('.x').length")
            .unwrap();
        assert_eq!(count_doc.as_f64().unwrap() as i64, 2);
    }

    #[test]
    fn document_evaluate_exposes_basic_xpath_result() {
        let mut rt = setup_runtime("");

        let exposed = rt
            .evaluate("`${typeof XPathResult}:${typeof Document.prototype.evaluate}:${XPathResult.FIRST_ORDERED_NODE_TYPE}`")
            .unwrap();
        assert_eq!(exposed, serde_json::json!("function:function:9"));
    }

    /// Regression for #105: `document.forms` / `images` / `links` must be
    /// live, not hardcoded `[]`. jQuery 1.x's submit-event setup iterates
    /// `document.forms` and crashes when it's empty for pages that have forms.
    #[test]
    fn document_forms_images_links_are_live() {
        let mut rt =
            setup_runtime(r#"<form></form><form></form><img><a href="x">l</a><a>no-href</a>"#);
        assert_eq!(
            rt.evaluate("document.forms.length")
                .unwrap()
                .as_f64()
                .unwrap() as i64,
            2
        );
        assert_eq!(
            rt.evaluate("document.images.length")
                .unwrap()
                .as_f64()
                .unwrap() as i64,
            1
        );
        assert_eq!(
            rt.evaluate("document.links.length")
                .unwrap()
                .as_f64()
                .unwrap() as i64,
            1
        );
    }

    #[cfg(feature = "render")]
    fn parser_image_runtime(
        html: &str,
        loader: impl obscura_render::RenderResourceLoader + 'static,
    ) -> ObscuraJsRuntime {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(html));
        rt.set_url("http://example.com/page/index.html");
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(loader);
        rt.run_page_init();
        rt
    }

    #[cfg(feature = "render")]
    fn two_by_three_png() -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAYAAAC56t6B\
                 AAAAFklEQVR4nGP8z8Dwn4GBgYGJAQrgDAAxOwIE7x6DkQAAAABJRU5ErkJggg=="
                    .replace(char::is_whitespace, ""),
            )
            .unwrap()
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn parser_images_load_concurrently_without_blocking_the_event_loop() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_accepted = accepted.clone();
        let server_active = active.clone();
        let server_max_active = max_active.clone();
        let png = two_by_three_png();
        std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                server_accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let active = server_active.clone();
                let max_active = server_max_active.clone();
                let png = png.clone();
                std::thread::spawn(move || {
                    use std::io::{Read as _, Write as _};

                    let mut request = [0u8; 2048];
                    let _ = stream.read(&mut request);
                    let concurrent = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    max_active.fetch_max(concurrent, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        png.len()
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                    stream.write_all(&png).unwrap();
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
        });

        let base = format!("http://{address}");
        let html = format!(
            r#"<img src="{base}/one.png"><img src="{base}/two.png">
                <img src="{base}/three.png"><img src="{base}/shared.png">
                <img src="{base}/shared.png">"#
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(&html));
        rt.set_url(&format!("{base}/page.html"));
        rt.set_http_client(std::sync::Arc::new(
            obscura_net::ObscuraHttpClient::with_full_options(
                std::sync::Arc::new(obscura_net::CookieJar::new()),
                None,
                true,
            ),
        ));
        rt.run_page_init();

        let started = std::time::Instant::now();
        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    globalThis.__imageTimerRan = false;
                    setTimeout(() => { __imageTimerRan = true; }, 10);
                    const images = Array.from(document.images);
                    const events = [];
                    const finish = (type, image) => {
                        events.push([
                            type,
                            __imageTimerRan,
                            image.naturalWidth,
                            image.naturalHeight,
                        ]);
                        if (events.length === images.length) resolve(events);
                    };
                    for (const image of images) {
                        image.addEventListener("load", () => finish("load", image));
                        image.addEventListener("error", () => finish("error", image));
                        void image.complete;
                    }
                    setTimeout(() => resolve([["timed out"]]), 2000);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                ["load", true, 2, 3],
                ["load", true, 2, 3],
                ["load", true, 2, 3],
                ["load", true, 2, 3],
                ["load", true, 2, 3],
            ])
        );
        assert_eq!(
            accepted.load(std::sync::atomic::Ordering::SeqCst),
            4,
            "two elements selecting one URL must share a single request"
        );
        assert!(
            max_active.load(std::sync::atomic::Ordering::SeqCst) >= 3,
            "slow image requests did not overlap"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "four 150ms image requests serialized: {elapsed:?}"
        );
        assert_eq!(
            rt.state
                .borrow()
                .page_in_flight
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn image_lifecycle_cache_is_separated_by_cors_credentials_profile() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let server_requests = requests.clone();
        let png = two_by_three_png();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let mode = request
                    .lines()
                    .find_map(|line| line.strip_prefix("sec-fetch-mode: "))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                server_requests.lock().unwrap().push((path.clone(), mode));
                let cors_headers = if path == "/cors.png" {
                    // Anonymous accepts wildcard; use-credentials must reject
                    // it even when credentials permission is also present.
                    "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Credentials: true\r\n"
                } else {
                    ""
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n{cors_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    png.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(&png).unwrap();
            }
        });

        let base = format!("http://{address}");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(&format!(r#"<img id="image" src="{base}/plain.png">"#)));
        // Deliberately make the image cross-origin from the document.
        rt.set_url("http://127.0.0.1:1/page.html");
        rt.set_http_client(std::sync::Arc::new(
            obscura_net::ObscuraHttpClient::with_full_options(
                std::sync::Arc::new(obscura_net::CookieJar::new()),
                None,
                true,
            ),
        ));
        rt.run_page_init();
        rt.execute_script(
            "observe-profiled-image",
            r#"
                globalThis.image = document.getElementById("image");
                globalThis.__profileEvents = [];
                image.addEventListener("load", () => __profileEvents.push("load"));
                image.addEventListener("error", () => __profileEvents.push("error"));
                void image.complete;
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load"]])
        );

        rt.execute_script("require-anonymous-cors", r#"image.crossOrigin = "anonymous";"#)
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 0, ["load", "error"]]),
            "URL-keyed no-CORS bytes must not satisfy an anonymous CORS request"
        );

        rt.execute_script("restore-no-cors", "image.removeAttribute('crossorigin');")
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load", "error", "load"]]),
            "a CORS failure must not poison the earlier no-CORS success"
        );

        rt.execute_script(
            "load-anonymous-cors",
            &format!(r#"image.crossOrigin = "anonymous"; image.src = "{base}/cors.png";"#),
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load", "error", "load", "load"]])
        );

        rt.execute_script(
            "require-credentialed-cors",
            r#"image.crossOrigin = "use-credentials";"#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 0, ["load", "error", "load", "load", "error"]]),
            "anonymous CORS success must not satisfy use-credentials"
        );

        rt.execute_script(
            "restore-anonymous-cors",
            r#"image.crossOrigin = "anonymous";"#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load", "error", "load", "load", "error", "load"]]),
            "credentialed CORS failure must not poison anonymous success"
        );

        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                ("/plain.png".to_string(), "no-cors".to_string()),
                ("/plain.png".to_string(), "cors".to_string()),
                ("/cors.png".to_string(), "cors".to_string()),
                ("/cors.png".to_string(), "cors".to_string()),
            ]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn parser_image_data_src_mutation_does_not_restart_lifecycle() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="image" src="real.png" data-src="deferred.png">"#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        rt.execute_script(
            "observe-data-src-mutation",
            r#"
                globalThis.__dataSrcEvents = [];
                const image = document.getElementById("image");
                image.addEventListener("load", () => __dataSrcEvents.push(image.currentSrc));
                void image.complete;
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.currentSrc, __dataSrcEvents]")
                .unwrap(),
            serde_json::json!([
                true,
                "http://example.com/page/real.png",
                ["http://example.com/page/real.png"]
            ])
        );

        rt.execute_script(
            "mutate-non-source-data-attribute",
            r#"
                image.dataset.src = "ignored.png";
                globalThis.__afterDataSrcMutation = [image.complete, image.currentSrc];
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[__afterDataSrcMutation, __dataSrcEvents]")
                .unwrap(),
            serde_json::json!([
                [true, "http://example.com/page/real.png"],
                ["http://example.com/page/real.png"]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["http://example.com/page/real.png".to_string()]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn parser_image_data_src_is_inert_until_script_assigns_src() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="image" data-src="promoted.png">"#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    globalThis.image = document.getElementById("image");
                    globalThis.__promotedEvents = [];
                    image.addEventListener("load", () => __promotedEvents.push(image.currentSrc));
                    return [image.src, image.currentSrc, image.complete,
                            image.naturalWidth, image.naturalHeight];
                })()"#,
            )
            .unwrap(),
            serde_json::json!(["", "", true, 0, 0])
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert!(requests.lock().unwrap().is_empty());

        rt.execute_script(
            "promote-data-src-through-page-script",
            r#"
                image.src = image.dataset.src;
                globalThis.__afterSrcPromotion = [image.complete, image.currentSrc];
            "#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("__afterSrcPromotion").unwrap(),
            serde_json::json!([false, "http://example.com/page/promoted.png"])
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[image.complete, image.naturalWidth, image.naturalHeight, \
                  image.currentSrc, __promotedEvents]"
            )
            .unwrap(),
            serde_json::json!([
                true,
                2,
                3,
                "http://example.com/page/promoted.png",
                ["http://example.com/page/promoted.png"]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["http://example.com/page/promoted.png".to_string()]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn parser_image_lifecycle_uses_shared_render_resource() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="hero" src="../assets/hero.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(png.clone())
            },
        );
        rt.execute_script(
            "observe-parser-image",
            r#"
                globalThis.__imageEvents = [];
                globalThis.__decodeState = "pending";
                const image = document.getElementById("hero");
                globalThis.__imageInitial = [
                    image instanceof HTMLImageElement,
                    image.complete,
                    image.naturalWidth,
                    image.naturalHeight
                ];
                image.addEventListener("load", () => __imageEvents.push("load"));
                image.addEventListener("error", () => __imageEvents.push("error"));
                image.decode().then(
                    () => { __decodeState = "resolved"; },
                    error => { __decodeState = error.name; }
                );
            "#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("__imageInitial").unwrap(),
            serde_json::json!([true, false, 0, 0])
        );

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                r#"[
                    image.complete,
                    image.naturalWidth,
                    image.naturalHeight,
                    image.currentSrc,
                    __imageEvents,
                    __decodeState
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                true,
                2,
                3,
                "http://example.com/assets/hero.png",
                ["load"],
                "resolved"
            ])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // A subsequent request for the same URL is served by the retained
        // renderer bytes rather than calling the loader again.
        rt.execute_script("reload-image", r#"image.src = "../assets/hero.png";"#)
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn parser_image_failure_completes_and_rejects_decode() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let mut rt = parser_image_runtime(
            r#"<img id="broken" src="missing.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                None
            },
        );
        rt.execute_script(
            "observe-broken-image",
            r#"
                globalThis.__brokenEvents = [];
                globalThis.__brokenDecode = "pending";
                const broken = document.getElementById("broken");
                broken.onload = () => __brokenEvents.push("load");
                broken.onerror = () => __brokenEvents.push("error");
                broken.decode().then(
                    () => { __brokenDecode = "resolved"; },
                    error => { __brokenDecode = error.name; }
                );
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[broken.complete, broken.naturalWidth, broken.naturalHeight, \
                  __brokenEvents, __brokenDecode]"
            )
            .unwrap(),
            serde_json::json!([true, 0, 0, ["error"], "EncodingError"])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    fn parser_image_first_getter_observes_prepare_seeded_cache_synchronously() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="cached" src="cached.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(png.clone())
            },
        );
        {
            let mut state = rt.state.borrow_mut();
            assert!(ensure_prepared_render(&mut state).is_some());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Constructing the JS wrapper happens here, after prepare_dom loaded
        // the resource. The first complete getter must see that cache hit
        // immediately; it must not briefly regress to pending/zero.
        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const cached = document.getElementById("cached");
                    return [
                        cached.complete,
                        cached.naturalWidth,
                        cached.naturalHeight,
                        cached.currentSrc
                    ];
                })()"#,
            )
            .unwrap(),
            serde_json::json!([true, 2, 3, "http://example.com/page/cached.png"])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn parser_image_fallback_invalidates_only_new_intrinsic_geometry() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="late" src="late.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(png.clone())
            },
        );
        {
            let mut state = rt.state.borrow_mut();
            let previous = state.render_resources.set_sync_loading_enabled(false);
            assert!(ensure_prepared_render(&mut state).is_some());
            state
                .render_resources
                .set_sync_loading_enabled(previous);
            assert!(state.prepared_render.is_some());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        rt.execute_script(
            "load-image-after-layout",
            r#"
                globalThis.__lateEvents = [];
                const late = document.getElementById("late");
                late.addEventListener("load", () => __lateEvents.push("load"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[late.complete, late.naturalWidth, late.naturalHeight, __lateEvents]"
            )
            .unwrap(),
            serde_json::json!([true, 2, 3, ["load"]])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert_eq!(
                state.pending_style_mutations,
                vec![obscura_render::RetainedStyleMutation::Resource]
            );
        }

        // Once the successful dimensions are retained, another loading-form
        // metadata probe is only a cache hit and must preserve fresh layout.
        {
            let mut state = rt.state.borrow_mut();
            assert!(ensure_prepared_render(&mut state).is_some());
            assert!(state.prepared_render.is_some());
        }
        rt.execute_script(
            "reload-retained-image",
            r#"late.src = "late.png";"#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(rt.state.borrow().prepared_render.is_some());

        let missing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let missing_loader_calls = missing_calls.clone();
        let mut missing = parser_image_runtime(
            r#"<img id="missing" src="missing.png">"#,
            move |_url: &str| {
                missing_loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                None
            },
        );
        {
            let mut state = missing.state.borrow_mut();
            let previous = state.render_resources.set_sync_loading_enabled(false);
            assert!(ensure_prepared_render(&mut state).is_some());
            state
                .render_resources
                .set_sync_loading_enabled(previous);
            assert!(state.prepared_render.is_some());
        }
        missing
            .execute_script(
                "fail-image-after-layout",
                r#"
                    globalThis.__missingEvents = [];
                    const missing = document.getElementById("missing");
                    missing.addEventListener("error", () => __missingEvents.push("error"));
                "#,
            )
            .unwrap();
        missing.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            missing
                .evaluate(
                    "[missing.complete, missing.naturalWidth, missing.naturalHeight, __missingEvents]"
                )
                .unwrap(),
            serde_json::json!([true, 0, 0, ["error"]])
        );
        assert_eq!(
            missing_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(missing.state.borrow().prepared_render.is_some());
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn stable_cached_image_getters_do_not_queue_resize_geometry_work() {
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="cached" src="cached.png">
               <div id="probe" style="width:40px;height:20px"></div>"#,
            move |_url: &str| Some(png.clone()),
        );
        rt.execute_script(
            "settle-cached-image",
            r#"
                const cached = document.getElementById("cached");
                void cached.complete;
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[cached.complete, cached.naturalWidth, cached.naturalHeight]")
                .unwrap(),
            serde_json::json!([true, 2, 3])
        );

        rt.execute_script(
            "observe-unrelated-geometry",
            r#"
                globalThis.__stableGetterResizeRecords = 0;
                globalThis.__stableGetterObserver = new ResizeObserver(entries => {
                    __stableGetterResizeRecords += entries.length;
                });
                __stableGetterObserver.observe(document.getElementById("probe"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[__stableGetterResizeRecords, __obscura_nextPendingTimeoutDelay()]"
            )
            .unwrap(),
            serde_json::json!([1, -1])
        );

        rt.execute_script(
            "read-stable-image-cache",
            r#"
                for (let i = 0; i < 50; i++) {
                    void cached.complete;
                    void cached.currentSrc;
                    void cached.naturalWidth;
                    void cached.naturalHeight;
                }
            "#,
        )
        .unwrap();
        // Cached lifecycle reads do not change intrinsic dimensions, so they
        // must not enqueue a rendering checkpoint (and its geometry walk).
        assert_eq!(
            rt.evaluate(
                "[__stableGetterResizeRecords, __obscura_nextPendingTimeoutDelay()]"
            )
            .unwrap(),
            serde_json::json!([1, -1])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn parser_image_source_replacement_cancels_queued_completion() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let first = two_by_three_png();
        use base64::Engine as _;
        let second = base64::engine::general_purpose::STANDARD
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAFCAYAAABirU3b\
                 AAAAFUlEQVR4nGNk+M/wnwEJMDGgATIEAKVaAgg/Jbt7AAAAAElFTkSuQmCC"
                    .replace(char::is_whitespace, ""),
            )
            .unwrap();
        let mut rt = parser_image_runtime(r#"<img id="swap" src="old.png">"#, move |url: &str| {
            seen.lock().unwrap().push(url.to_string());
            if url.ends_with("/new.png") {
                Some(second.clone())
            } else {
                Some(first.clone())
            }
        });
        rt.execute_script(
            "replace-image-source",
            r#"
                globalThis.__swapEvents = [];
                const swap = document.getElementById("swap");
                swap.addEventListener("load", () => __swapEvents.push(swap.currentSrc));
                swap.src = "new.png";
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[swap.complete, swap.naturalWidth, swap.naturalHeight, \
                  swap.currentSrc, __swapEvents]"
            )
            .unwrap(),
            serde_json::json!([
                true,
                4,
                5,
                "http://example.com/page/new.png",
                ["http://example.com/page/new.png"]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["http://example.com/page/new.png".to_string()]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn responsive_picture_lifecycle_tracks_viewport_density_and_source_media() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"
                <picture>
                    <source type="image/avif" srcset="unsupported.avif">
                    <source id="wide-source" media="(min-width: 800px)"
                            srcset="wide.png 2x">
                    <source media="(max-width: 799px)" srcset="narrow.png">
                    <img id="responsive-picture" src="fallback.png">
                </picture>
            "#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        rt.set_viewport(1000.0, 600.0);
        rt.execute_script(
            "observe-responsive-picture",
            r#"
                globalThis.__pictureLoads = [];
                const pictureImage = document.getElementById("responsive-picture");
                pictureImage.addEventListener("load", () => {
                    __pictureLoads.push(pictureImage.currentSrc);
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[pictureImage.currentSrc, pictureImage.naturalWidth, \
                  pictureImage.naturalHeight, __pictureLoads]"
            )
            .unwrap(),
            serde_json::json!([
                "http://example.com/page/wide.png",
                1,
                2,
                ["http://example.com/page/wide.png"]
            ])
        );

        // A live viewport change re-runs the renderer's media/source
        // selection. The cache-only complete getter must report pending but
        // must not perform the load itself.
        rt.set_viewport(600.0, 600.0);
        assert_eq!(
            rt.evaluate("pictureImage.complete").unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[pictureImage.currentSrc, pictureImage.naturalWidth, \
                  pictureImage.naturalHeight, __pictureLoads]"
            )
            .unwrap(),
            serde_json::json!([
                "http://example.com/page/narrow.png",
                2,
                3,
                [
                    "http://example.com/page/wide.png",
                    "http://example.com/page/narrow.png"
                ]
            ])
        );

        // Mutating a <source> selection input invalidates its associated img.
        // The wide bytes are already shared in the render cache, but lifecycle
        // completion remains task-queued and emits one new load event.
        rt.execute_script(
            "mutate-picture-source",
            r#"
                document.getElementById("wide-source").setAttribute("media", "all");
                globalThis.__pictureCompleteAfterSourceMutation = pictureImage.complete;
            "#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("__pictureCompleteAfterSourceMutation").unwrap(),
            serde_json::json!(false)
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[pictureImage.currentSrc, __pictureLoads]")
                .unwrap(),
            serde_json::json!([
                "http://example.com/page/wide.png",
                [
                    "http://example.com/page/wide.png",
                    "http://example.com/page/narrow.png",
                    "http://example.com/page/wide.png"
                ]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                "http://example.com/page/wide.png".to_string(),
                "http://example.com/page/narrow.png".to_string(),
            ]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn responsive_srcset_sizes_uses_renderer_selected_current_src() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="responsive-srcset" src="fallback.png"
                     srcset="small.png 400w, large.png 800w" sizes="400px">"#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        rt.execute_script(
            "observe-responsive-srcset",
            r#"
                globalThis.__srcsetLoads = [];
                const srcsetImage = document.getElementById("responsive-srcset");
                srcsetImage.addEventListener("load", () => {
                    __srcsetLoads.push(srcsetImage.currentSrc);
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[srcsetImage.currentSrc, __srcsetLoads]")
                .unwrap(),
            serde_json::json!([
                "http://example.com/page/small.png",
                ["http://example.com/page/small.png"]
            ])
        );

        change_srcset_image_sizes(&mut rt);
        assert_eq!(
            rt.evaluate("srcsetImage.complete").unwrap(),
            serde_json::json!(false)
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[srcsetImage.currentSrc, __srcsetLoads]")
                .unwrap(),
            serde_json::json!([
                "http://example.com/page/large.png",
                [
                    "http://example.com/page/small.png",
                    "http://example.com/page/large.png"
                ]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                "http://example.com/page/small.png".to_string(),
                "http://example.com/page/large.png".to_string(),
            ]
        );
    }

    #[cfg(feature = "render")]
    fn change_srcset_image_sizes(rt: &mut ObscuraJsRuntime) {
        rt.execute_script("change-responsive-sizes", r#"srcsetImage.sizes = "800px";"#)
            .unwrap();
    }

    /// Regression for #105: `HTMLFormElement` must expose `.elements` so
    /// frameworks that probe form field collections work.
    #[test]
    fn html_form_element_exposes_elements_collection() {
        let mut rt = setup_runtime(
            r#"<form id="f"><input name=a><input name=b><textarea></textarea></form>"#,
        );
        let n = rt
            .evaluate("document.getElementById('f').elements.length")
            .unwrap();
        assert_eq!(n.as_f64().unwrap() as i64, 3);
        let is_form = rt
            .evaluate("document.getElementById('f') instanceof HTMLFormElement")
            .unwrap();
        assert_eq!(is_form, serde_json::json!(true));
    }

    /// Regression for #105: `Element.prepend` must actually insert at the
    /// start, not silently no-op.
    #[test]
    fn element_prepend_inserts_at_start() {
        let mut rt = setup_runtime(r#"<div id="c"><span>existing</span></div>"#);
        rt.evaluate(
            r#"
            const c = document.getElementById('c');
            const n = document.createElement('span');
            n.id = 'first';
            c.prepend(n);
            "#,
        )
        .unwrap();
        let first_id = rt
            .evaluate("document.getElementById('c').firstChild.id")
            .unwrap();
        assert_eq!(first_id, serde_json::json!("first"));
        let count = rt
            .evaluate("document.getElementById('c').childNodes.length")
            .unwrap();
        assert_eq!(count.as_f64().unwrap() as i64, 2);
    }

    /// Regression for #105: `isEqualNode` compares structure, not identity.
    /// Framework diff algorithms rely on this.
    #[test]
    fn is_equal_node_does_structural_compare() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const a = document.createElement('div'); a.setAttribute('class', 'x'); a.innerHTML = '<span>hi</span>';
                const b = document.createElement('div'); b.setAttribute('class', 'x'); b.innerHTML = '<span>hi</span>';
                const c = document.createElement('div'); c.innerHTML = '<span>bye</span>';
                return [a.isEqualNode(b), a.isEqualNode(c), a.isSameNode(b)];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, false, false]));
    }

    /// Regression for the long-standing insert_before arg-order bug noted
    /// in CLAUDE.md: bootstrap.js was passing (parent, new, ref) but `_dom`
    /// forwards only two args, silently dropping `ref`. With the fix,
    /// `insertBefore` actually inserts.
    #[test]
    fn insert_before_inserts_node_at_correct_position() {
        let mut rt =
            setup_runtime(r#"<div id="p"><span id="b">b</span><span id="c">c</span></div>"#);
        let order = rt
            .evaluate(
                r#"
                const p = document.getElementById('p');
                const a = document.createElement('span');
                a.id = 'a';
                p.insertBefore(a, document.getElementById('b'));
                return Array.from(p.children).map(e => e.id).join(',');
                "#,
            )
            .unwrap();
        assert_eq!(order, serde_json::json!("a,b,c"));
    }

    #[test]
    fn test_console_log() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script("test", "console.log('Hello from V8!')")
            .unwrap();
    }

    #[test]
    fn test_location() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let href = rt.evaluate("location.href").unwrap();
        assert_eq!(href, serde_json::json!("http://example.com/test"));
    }

    #[test]
    fn test_button_click_dispatches_listener() {
        let mut rt = setup_runtime(r#"<button id="go">Go</button>"#);
        let result = rt
            .evaluate(
                r#"
            const button = document.getElementById('go');
            button.addEventListener('click', () => { button.dataset.clicked = 'yes'; });
            button.click();
            return button.dataset.clicked;
        "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("yes"));
    }

    #[test]
    fn test_dispatch_mouse_event_runs_listener() {
        let mut rt = setup_runtime(r#"<button id="go">Go</button>"#);
        let result = rt
            .evaluate(
                r#"
            const button = document.getElementById('go');
            let count = 0;
            button.addEventListener('click', () => { count += 1; });
            button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
            return count;
        "#,
            )
            .unwrap();
        assert_eq!(result.as_f64().unwrap() as i64, 1);
    }

    #[test]
    fn test_location_href_assignment_updates_navigation_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let href = rt
            .evaluate("const next = '/next'; location.href = next; return location.href;")
            .unwrap();
        assert_eq!(href, serde_json::json!("http://example.com/next"));
        assert_eq!(
            rt.take_pending_navigation(),
            Some((
                "http://example.com/next".to_string(),
                "GET".to_string(),
                "".to_string()
            ))
        );
    }

    #[test]
    fn test_location_navigation_coerces_url_objects() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let hrefs = rt
            .evaluate(
                r#"(() => {
                    location.href = new URL('/from-href', location.href);
                    const href = location.href;
                    location.assign(new URL('/from-assign', location.href));
                    const assigned = location.href;
                    location.replace(new URL('/from-replace', location.href));
                    return [href, assigned, location.href];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            hrefs,
            serde_json::json!([
                "http://example.com/from-href",
                "http://example.com/from-assign",
                "http://example.com/from-replace"
            ])
        );
        assert_eq!(
            rt.take_pending_navigation(),
            Some((
                "http://example.com/from-replace".to_string(),
                "GET".to_string(),
                "".to_string()
            ))
        );
    }

    #[test]
    fn test_submit_button_click_handler_can_prevent_default_and_navigate() {
        let mut rt =
            setup_runtime(r#"<form><button type="submit" id="submit">Submit</button></form>"#);
        let href = rt
            .evaluate(
                r#"
            const form = document.querySelector('form');
            form.addEventListener('submit', (event) => {
                event.preventDefault();
                location.href = '/submitted';
            });
            document.getElementById('submit').click();
            return location.href;
        "#,
            )
            .unwrap();
        assert_eq!(href, serde_json::json!("http://example.com/submitted"));
        assert_eq!(
            rt.take_pending_navigation(),
            Some((
                "http://example.com/submitted".to_string(),
                "GET".to_string(),
                "".to_string()
            ))
        );
    }

    #[test]
    fn test_navigator() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let ua = rt.evaluate("navigator.userAgent").unwrap();
        assert!(
            ua.as_str().unwrap().contains("Chrome"),
            "UA should contain Chrome: {}",
            ua
        );
        let wd = rt.evaluate("navigator.webdriver").unwrap();
        assert_eq!(wd, serde_json::json!(false));
        let plugins = rt.evaluate("navigator.plugins.length").unwrap();
        assert!(plugins.as_f64().unwrap() > 0.0, "Should have plugins");
        let chrome = rt.evaluate("typeof window.chrome").unwrap();
        assert_eq!(chrome, serde_json::json!("object"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_no_args() {
        let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
        let result = rt
            .call_function_on("() => document.title", None, &[], true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("Test"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_with_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![
            serde_json::json!({"value": 10}),
            serde_json::json!({"value": 20}),
        ];
        let result = rt
            .call_function_on("(a, b) => a + b", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 30);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_with_string_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![
            serde_json::json!({"value": "hello"}),
            serde_json::json!({"value": " world"}),
        ];
        let result = rt
            .call_function_on("(a, b) => a + b", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("hello world"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_with_object_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![serde_json::json!({"value": {"name": "test", "count": 5}})];
        let result = rt
            .call_function_on("(obj) => obj.name + ':' + obj.count", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("test:5"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_return_object() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on("() => ({a: 1, b: 2})", None, &[], true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!({"a": 1, "b": 2}));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_object_ref_preserves_methods() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on(
                "() => ({ items: [1,2,3], getLen: function() { return this.items.length; } })",
                None,
                &[],
                false,
            )
            .await
            .unwrap();
        let oid = result.object_id.unwrap();

        let result2 = rt
            .call_function_on(
                "function() { return this.getLen(); }",
                Some(&oid),
                &[],
                true,
            )
            .await
            .unwrap();
        assert_eq!(result2.value.unwrap().as_f64().unwrap() as i64, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_evaluate_for_cdp_detects_node() {
        let mut rt = setup_runtime("<html><body><h1>Hello</h1></body></html>");
        let result = rt
            .evaluate_for_cdp("document.querySelector('h1')", false, false)
            .await
            .unwrap();
        assert_eq!(result.subtype.as_deref(), Some("node"));
        assert_eq!(result.js_type, "object");
        assert!(result.object_id.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_evaluate_for_cdp_detects_document() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate_for_cdp("document", false, false).await.unwrap();
        assert_eq!(result.subtype.as_deref(), Some("node"));
        assert_eq!(result.class_name, "HTMLDocument");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_evaluate_for_cdp_awaits_resolved_promise() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate_for_cdp("Promise.resolve(42)", true, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_evaluate_for_cdp_awaits_timer_promise() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate_for_cdp(
                "new Promise(resolve => setTimeout(() => resolve('done'), 1))",
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_str().unwrap(), "done");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_evaluate_for_cdp_can_await_beyond_legacy_five_second_cap() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let started = std::time::Instant::now();
        let result = rt
            .evaluate_for_cdp_with_timeout(
                "new Promise(resolve => setTimeout(() => resolve('after-five'), 5100))",
                true,
                true,
                6000,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_str(), Some("after-five"));
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(5),
            "long promise resolved before its timer deadline"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_for_cdp_reports_unsettled_promise_timeout() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let error = rt
            .call_function_on_for_cdp_with_timeout(
                "() => new Promise(() => {})",
                None,
                &[],
                true,
                true,
                25,
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("did not settle within 25ms"),
            "unexpected timeout error: {error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_evaluate_for_cdp_awaits_async_function() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate_for_cdp("(async () => 'async-ok')()", true, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_str().unwrap(), "async-ok");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_evaluate_for_cdp_reports_promise_rejection() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let err = rt
            .evaluate_for_cdp("Promise.reject(new Error('boom'))", true, true)
            .await
            .unwrap_err();
        assert!(err.contains("boom"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_call_function_on_dom_interaction() {
        let mut rt = setup_runtime(r#"<div id="items"><span>A</span><span>B</span></div>"#);
        let args = vec![serde_json::json!({"value": "span"})];
        let result = rt
            .call_function_on(
                "(sel) => document.querySelectorAll(sel).length",
                None,
                &args,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 2);
    }

    #[test]
    fn test_inner_html_setter() {
        let mut rt = setup_runtime(r#"<div id="target"><p>Old</p></div>"#);
        rt.execute_script(
            "test",
            r#"
            var el = document.getElementById('target');
            el.innerHTML = '<strong>Bold</strong><em>Italic</em>';
        "#,
        )
        .unwrap();
        let result = rt
            .evaluate("document.getElementById('target').innerHTML")
            .unwrap();
        let html = result.as_str().unwrap();
        assert!(
            html.contains("<strong>"),
            "innerHTML should contain <strong>, got: {}",
            html
        );
        assert!(
            html.contains("<em>"),
            "innerHTML should contain <em>, got: {}",
            html
        );
        assert!(
            !html.contains("Old"),
            "innerHTML should not contain old content, got: {}",
            html
        );
    }

    #[test]
    fn test_inner_html_with_nested() {
        let mut rt = setup_runtime(r#"<div id="root"></div>"#);
        rt.execute_script(
            "test",
            r#"
            var el = document.getElementById('root');
            el.innerHTML = '<ul><li>A</li><li>B</li><li>C</li></ul>';
        "#,
        )
        .unwrap();
        let count = rt
            .evaluate("document.querySelectorAll('li').length")
            .unwrap();
        assert_eq!(
            count.as_f64().unwrap() as i64,
            3,
            "Should find 3 li elements after innerHTML set"
        );

        let text = rt
            .evaluate("document.querySelector('li').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("A"));
    }

    #[test]
    fn test_input_value() {
        let mut rt = setup_runtime(
            r#"<form><input id="name" type="text" value="initial"><textarea id="bio">old text</textarea></form>"#,
        );
        let val = rt
            .evaluate("document.getElementById('name').value")
            .unwrap();
        assert_eq!(val, serde_json::json!("initial"));
        rt.execute_script(
            "test",
            "document.getElementById('name').value = 'new value';",
        )
        .unwrap();
        let val2 = rt
            .evaluate("document.getElementById('name').value")
            .unwrap();
        assert_eq!(val2, serde_json::json!("new value"));
        let bio = rt.evaluate("document.getElementById('bio').value").unwrap();
        assert_eq!(bio, serde_json::json!("old text"));
    }

    #[test]
    fn test_sequential_runtime_swap() {
        let mut rt1 = setup_runtime("<html><body><h1>Page1</h1></body></html>");
        let title1 = rt1
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(title1, serde_json::json!("Page1"));

        let dom1 = rt1.take_dom();
        drop(rt1);

        let mut rt2 = setup_runtime("<html><body><h1>Page2</h1></body></html>");
        let title2 = rt2
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(title2, serde_json::json!("Page2"));
        drop(rt2);

        if let Some(dom) = dom1 {
            let mut rt1b = ObscuraJsRuntime::new();
            rt1b.set_dom(dom);
            rt1b.set_url("http://example.com");
            rt1b.set_title("Page1");
            rt1b.run_page_init();
            let title1b = rt1b
                .evaluate("document.querySelector('h1').textContent")
                .unwrap();
            assert_eq!(title1b, serde_json::json!("Page1"));
        }
    }

    #[test]
    fn test_checkbox_checked() {
        let mut rt = setup_runtime(r#"<input id="cb" type="checkbox" checked>"#);
        let checked = rt
            .evaluate("document.getElementById('cb').checked")
            .unwrap();
        assert_eq!(checked, serde_json::json!(true));
        rt.execute_script("test", "document.getElementById('cb').checked = false;")
            .unwrap();
        let checked2 = rt
            .evaluate("document.getElementById('cb').checked")
            .unwrap();
        assert_eq!(checked2, serde_json::json!(false));
    }

    // Issue #324: React/Preact/Vue install a value tracker by redefining `value`
    // on the element instance so they can tell a real edit from their own
    // controlled write. __obscura_setFieldValue must write through the prototype
    // setter, leaving that per-instance tracker stale, so the following input
    // event reads as a genuine change and onChange fires. A plain assignment
    // keeps the tracker in sync and suppresses onChange.
    #[test]
    fn set_field_value_bypasses_instance_value_wrapper() {
        let mut rt = setup_runtime(r#"<input id="i">"#);
        let result = rt
            .evaluate(
                r#"
                (function(){
                    var el = document.getElementById('i');
                    var d = Object.getOwnPropertyDescriptor(el.constructor.prototype, 'value');
                    var set = d.set, get = d.get, tracked = '' + el.value;
                    Object.defineProperty(el, 'value', {
                        configurable: true,
                        get: function(){ return get.call(this); },
                        set: function(v){ tracked = '' + v; set.call(this, v); },
                    });
                    el.value = 'wrapped';
                    var afterDirect = { value: el.value, tracked: tracked };
                    globalThis.__obscura_setFieldValue(el, 'value', 'native');
                    var afterHelper = { value: el.value, tracked: tracked };
                    return JSON.stringify({ afterDirect: afterDirect, afterHelper: afterHelper });
                })()
                "#,
            )
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        // Direct assignment keeps tracker == value (the change that suppresses onChange).
        assert_eq!(parsed["afterDirect"]["value"], "wrapped");
        assert_eq!(parsed["afterDirect"]["tracked"], "wrapped");
        // The helper updates the value but leaves the tracker stale, so onChange fires.
        assert_eq!(parsed["afterHelper"]["value"], "native");
        assert_eq!(parsed["afterHelper"]["tracked"], "wrapped");
    }

    // Issue #324: React feature-detects the modern input-event path with
    // `('oninput' in document)`. If the GlobalEventHandlers on* attributes are
    // only on window (not Document/Element), that check fails and React falls
    // back to a legacy change-detection path, so controlled-input onChange never
    // fires. These must be present on document and Element.prototype too.
    #[test]
    fn global_event_handlers_present_on_document_and_element() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"JSON.stringify({
                    docInput: ('oninput' in document),
                    docChange: ('onchange' in document),
                    docClick: ('onclick' in document),
                    elProtoInput: ('oninput' in Element.prototype),
                    winInput: ('oninput' in window)
                })"#,
            )
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(p["docInput"], true);
        assert_eq!(p["docChange"], true);
        assert_eq!(p["docClick"], true);
        assert_eq!(p["elProtoInput"], true);
        assert_eq!(p["winInput"], true);
    }

    #[test]
    fn test_matches_and_closest() {
        let mut rt = setup_runtime(
            r#"<div class="outer"><div class="inner"><span id="target">Hi</span></div></div>"#,
        );
        let matches = rt
            .evaluate("document.getElementById('target').matches('span')")
            .unwrap();
        assert_eq!(matches, serde_json::json!(true));
        let closest = rt
            .evaluate("document.getElementById('target').closest('.outer').className")
            .unwrap();
        assert_eq!(closest, serde_json::json!("outer"));
        let no_match = rt
            .evaluate("document.getElementById('target').closest('.nonexistent')")
            .unwrap();
        assert_eq!(no_match, serde_json::Value::Null);
    }

    #[test]
    fn shallow_element_clone_preserves_interface_attributes_and_isolation() {
        let mut rt = setup_runtime(
            r#"<section id="src" class="source" data-token="original"><span>child</span></section>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const source = document.getElementById('src');
                const clone = source.cloneNode(false);
                clone.className = 'clone';
                source.setAttribute('data-token', 'changed');
                return [
                    clone instanceof Node,
                    clone instanceof Element,
                    clone instanceof HTMLElement,
                    typeof clone.outerHTML,
                    typeof clone.querySelectorAll,
                    clone.tagName,
                    clone.id,
                    clone.className,
                    clone.getAttribute('data-token'),
                    clone.childNodes.length,
                    clone.ownerDocument === document,
                    clone.parentNode === null,
                    clone !== source,
                    source.className,
                    source.getAttribute('data-token'),
                    source.childNodes.length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true, true, true, "string", "function", "SECTION", "src", "clone", "original", 0,
                true, true, true, "source", "changed", 1
            ])
        );
    }

    #[test]
    fn deep_document_element_clone_stays_an_independent_html_element() {
        let mut rt = setup_runtime(
            r#"<html lang="en" data-root="original"><head><title>Clone</title></head><body><main id="app" data-state="source"><p class="item">original text</p></main></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const source = document.documentElement;
                const clone = source.cloneNode(true);
                const cloneItem = clone.querySelector('.item');
                const sourceItem = source.querySelector('.item');
                cloneItem.textContent = 'clone text';
                source.querySelector('#app').setAttribute('data-state', 'changed');
                clone.setAttribute('lang', 'fr');
                return [
                    clone instanceof Element,
                    clone instanceof HTMLElement,
                    clone.tagName,
                    typeof clone.outerHTML,
                    typeof clone.querySelectorAll,
                    clone.querySelectorAll('head, body, main, p').length,
                    clone.ownerDocument === document,
                    clone.parentNode === null,
                    clone !== source,
                    clone.querySelector('body') !== document.body,
                    clone.getAttribute('data-root'),
                    clone.getAttribute('lang'),
                    source.getAttribute('lang'),
                    cloneItem.textContent,
                    sourceItem.textContent,
                    clone.querySelector('#app').getAttribute('data-state'),
                    source.querySelector('#app').getAttribute('data-state'),
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                true,
                "HTML",
                "string",
                "function",
                4,
                true,
                true,
                true,
                true,
                "original",
                "fr",
                "en",
                "clone text",
                "original text",
                "source",
                "changed"
            ])
        );
    }

    #[test]
    fn test_evaluate_multistatement() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("var x = 5; var y = 10; return x + y;").unwrap();
        assert_eq!(result.as_f64().unwrap() as i64, 15);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_object_ref_as_argument() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let obj = rt
            .call_function_on("() => ({ x: 42 })", None, &[], false)
            .await
            .unwrap();
        let oid = obj.object_id.unwrap();

        let args = vec![serde_json::json!({"objectId": oid})];
        let result = rt
            .call_function_on("(obj) => obj.x * 2", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 84);
    }

    fn setup_runtime_with_cookies(
        html: &str,
    ) -> (ObscuraJsRuntime, std::sync::Arc<obscura_net::CookieJar>) {
        let dom = obscura_dom::parse_html(html);
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.com/test");
        rt.set_title("Test Page");
        rt.set_cookie_jar(jar.clone());
        rt.run_page_init();
        (rt, jar)
    }

    #[test]
    fn test_document_cookie_reads_http_cookies() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("session=abc123; Path=/", &url);
        jar.set_cookie("theme=dark; Path=/", &url);
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(
            cookie_str.contains("session=abc123"),
            "expected session cookie, got: {}",
            cookie_str
        );
        assert!(
            cookie_str.contains("theme=dark"),
            "expected theme cookie, got: {}",
            cookie_str
        );
    }

    #[test]
    fn test_document_cookie_excludes_httponly() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("visible=yes; Path=/", &url);
        jar.set_cookie("secret=token; Path=/; HttpOnly", &url);
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(
            cookie_str.contains("visible=yes"),
            "expected visible cookie, got: {}",
            cookie_str
        );
        assert!(
            !cookie_str.contains("secret"),
            "httpOnly cookie should not be visible to JS, got: {}",
            cookie_str
        );
    }

    #[test]
    fn test_document_cookie_setter_stores_in_jar() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        rt.evaluate("document.cookie = 'foo=bar; Path=/'").unwrap();
        let url = url::Url::parse("http://example.com/test").unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        assert!(result.as_str().unwrap().contains("foo=bar"));
        let header = jar.get_cookie_header(&url);
        assert!(
            header.contains("foo=bar"),
            "cookie should be in jar, got: {}",
            header
        );
    }

    #[test]
    fn test_document_cookie_delete_via_max_age() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        rt.evaluate("document.cookie = 'temp=val; Path=/'").unwrap();
        assert!(rt
            .evaluate("document.cookie")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("temp=val"));
        rt.evaluate("document.cookie = 'temp=; Max-Age=0'").unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        assert!(
            !result.as_str().unwrap().contains("temp="),
            "cookie should be deleted, got: {}",
            result
        );
        assert!(!jar.get_cookie_header(&url).contains("temp="));
    }

    #[test]
    fn test_document_cookie_js_and_http_merge() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("server_sid=xyz; Path=/", &url);
        rt.evaluate("document.cookie = 'client_pref=light'")
            .unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(
            cookie_str.contains("server_sid=xyz"),
            "expected server cookie, got: {}",
            cookie_str
        );
        assert!(
            cookie_str.contains("client_pref=light"),
            "expected client cookie, got: {}",
            cookie_str
        );
    }

    #[test]
    fn test_document_cookie_empty_when_no_cookies() {
        let (mut rt, _jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let result = rt.evaluate("document.cookie").unwrap();
        assert_eq!(result.as_str().unwrap(), "");
    }

    #[test]
    fn test_document_cookie_no_jar_returns_empty() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("document.cookie").unwrap();
        assert_eq!(result.as_str().unwrap(), "");
    }

    #[test]
    fn test_document_write_appends_to_body() {
        let mut rt = setup_runtime("<html><body><p>Existing</p></body></html>");
        rt.evaluate("document.write('<div>Added</div>')").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        let body = html.as_str().unwrap();
        assert!(
            body.contains("Existing"),
            "existing content should remain, got: {}",
            body
        );
        assert!(
            body.contains("Added"),
            "written content should appear, got: {}",
            body
        );
    }

    #[test]
    fn test_document_writeln() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("document.writeln('Hello')").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        assert!(html.as_str().unwrap().contains("Hello"));
    }

    #[test]
    fn test_document_write_multiple_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("document.write('Hello', ' ', 'World')")
            .unwrap();
        let text = rt.evaluate("document.body.textContent").unwrap();
        assert_eq!(text.as_str().unwrap().trim(), "Hello World");
    }

    #[test]
    fn test_document_open_clears_body() {
        let mut rt = setup_runtime("<html><body><p>Old content</p></body></html>");
        rt.evaluate("document.open()").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        assert_eq!(html.as_str().unwrap(), "");
    }

    #[test]
    fn test_document_write_html_elements() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate(r#"document.write('<h1 id="title">Test</h1><p>Para</p>')"#)
            .unwrap();
        let h1 = rt
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(h1.as_str().unwrap(), "Test");
        let p = rt
            .evaluate("document.querySelector('p').textContent")
            .unwrap();
        assert_eq!(p.as_str().unwrap(), "Para");
    }

    #[test]
    fn test_url_relative_resolution() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate("new URL('data.json', 'http://example.com/path/page.html').href")
            .unwrap();
        assert_eq!(
            result.as_str().unwrap(),
            "http://example.com/path/data.json"
        );

        let result = rt
            .evaluate("new URL('/api/data', 'http://example.com/path/page.html').href")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "http://example.com/api/data");

        let result = rt
            .evaluate("new URL('https://other.com/foo', 'http://example.com/bar').href")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "https://other.com/foo");

        let result = rt
            .evaluate("new URL('sub/file.js', 'http://example.com/a/b/c.html').href")
            .unwrap();
        assert_eq!(
            result.as_str().unwrap(),
            "http://example.com/a/b/sub/file.js"
        );

        let result = rt
            .evaluate("new URL('api.json', 'http://localhost:8080/dir/index.html').href")
            .unwrap();
        assert_eq!(
            result.as_str().unwrap(),
            "http://localhost:8080/dir/api.json"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_fetch_url_input_decodes_binary_body_base64() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                const originalFetchOp = Deno.core.ops.op_fetch_url;
                try {
                    Deno.core.ops.op_fetch_url = (url) => {
                        globalThis.__capturedFetchUrl = url;
                        return JSON.stringify({
                            status: 200,
                            headers: { "content-type": "application/wasm" },
                            bodyBase64: "AGFzbQEAAAA=",
                            url,
                        });
                    };
                    const response = await fetch(new URL("/pkg/app_bg.wasm", document.URL));
                    const bytes = Array.from(new Uint8Array(await response.arrayBuffer()));
                    return { url: globalThis.__capturedFetchUrl, bytes };
                } finally {
                    Deno.core.ops.op_fetch_url = originalFetchOp;
                }
            }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "url": "http://example.com/pkg/app_bg.wasm",
                "bytes": [0, 97, 115, 109, 1, 0, 0, 0],
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_and_xhr_forward_browser_credentials_modes() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    const calls = [];
                    try {
                        Deno.core.ops.op_fetch_url =
                            (url, method, headers, body, origin, mode, credentials) => {
                                calls.push({ url, credentials });
                                return JSON.stringify({
                                    status: 200,
                                    headers: {},
                                    body: "ok",
                                    url,
                                });
                            };

                        await fetch("/default");
                        await fetch("/omit", { credentials: "omit" });
                        const request = new Request("/included", { credentials: "include" });
                        await fetch(request);
                        await fetch(request.clone());
                        await fetch(request, { credentials: "same-origin" });

                        const sendXhr = (path, withCredentials) => new Promise((resolve, reject) => {
                            const xhr = new XMLHttpRequest();
                            xhr.open("GET", path);
                            xhr.withCredentials = withCredentials;
                            xhr.onload = resolve;
                            xhr.onerror = reject;
                            xhr.send();
                        });
                        await sendXhr("/xhr-default", false);
                        await sendXhr("/xhr-credentialed", true);

                        let invalidFetchRejected = false;
                        try {
                            await fetch("/bad", { credentials: "invalid" });
                        } catch (error) {
                            invalidFetchRejected = error instanceof TypeError;
                        }

                        return { calls, invalidFetchRejected };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "calls": [
                    { "url": "http://example.com/default", "credentials": "same-origin" },
                    { "url": "http://example.com/omit", "credentials": "omit" },
                    { "url": "http://example.com/included", "credentials": "include" },
                    { "url": "http://example.com/included", "credentials": "include" },
                    { "url": "http://example.com/included", "credentials": "same-origin" },
                    { "url": "http://example.com/xhr-default", "credentials": "same-origin" },
                    { "url": "http://example.com/xhr-credentialed", "credentials": "include" },
                ],
                "invalidFetchRejected": true,
            })
        );
    }
    /// Serves a redirect chain across `connections` consecutive
    /// requests: `/hop/N` replies with 302 to `/hop/N-1`, `/hop/0` is the
    /// target. To a path the fixture cannot read it replies with 400
    /// instead of the target. A broken fixture thereby fails the test
    /// instead of letting it pass.
    fn redirect_chain_runtime(connections: usize) -> ObscuraJsRuntime {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..connections {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let hop = String::from_utf8_lossy(&buffer[..read])
                    .split_whitespace()
                    .nth(1)
                    .and_then(|path| path.strip_prefix("/hop/"))
                    .and_then(|hop| hop.parse::<usize>().ok());
                let response = match hop {
                    Some(0) => {
                        let body = "arrived";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                    Some(hop) => format!(
                        "HTTP/1.1 302 Found\r\nLocation: /hop/{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        hop - 1,
                    ),
                    None => {
                        let body = "unparsed";
                        format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let origin = format!("http://{address}");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html("<html><body></body></html>"));
        rt.set_url(&format!("{origin}/page"));
        rt.set_http_client(std::sync::Arc::new(
            obscura_net::ObscuraHttpClient::with_full_options(
                std::sync::Arc::new(obscura_net::CookieJar::new()),
                None,
                true,
            ),
        ));
        rt.run_page_init();
        rt
    }

    /// HTTP-redirect fetch returns a network error as soon as the
    /// redirect count *reaches* 20, and only increments it afterwards. So
    /// the twentieth hop must still succeed:
    /// https://fetch.spec.whatwg.org/#http-redirect-fetch
    /// WPT covers the same pair in `fetch/api/redirect/redirect-count.any.js`.
    #[tokio::test(flavor = "current_thread")]
    async fn fetch_follows_the_twentieth_redirect() {
        let mut rt = redirect_chain_runtime(21);
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => (await fetch("/hop/20")).text()"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!("arrived"));
    }

    /// The other end of the same pair: the twenty-first redirect must
    /// fail. `fetch` reports a rejected result as a `TypeError`.
    #[tokio::test(flavor = "current_thread")]
    async fn fetch_rejects_the_twenty_first_redirect() {
        let mut rt = redirect_chain_runtime(21);
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    try {
                        const response = await fetch("/hop/21");
                        return "resolved " + (await response.text());
                    } catch (error) {
                        return error instanceof TypeError ? "rejected" : "other";
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!("rejected"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_linked_stylesheet_enters_the_live_dom_with_imports_rebased() {
        let mut rt =
            setup_runtime("<html><head></head><body><div class=\"card\"></div></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    try {
                        Deno.core.ops.op_fetch_url = (url) => JSON.stringify({
                            status: 200,
                            headers: { "content-type": "text/css" },
                            body: url.endsWith("/assets/route.css")
                                ? '@import "./theme/base.css"; .card { display:grid; background-image:url("../img/card.png") }'
                                : '.card { color:red; background-image:url("./grain.png") }',
                            url,
                        });
                        const link = document.createElement("link");
                        link.setAttribute("rel", "stylesheet");
                        link.setAttribute("href", "/assets/route.css");
                        const loaded = new Promise(resolve => {
                            link.onload = () => resolve();
                        });
                        document.head.appendChild(link);
                        await loaded;
                        const style = document.querySelector("style[data-obscura-linked]");
                        const css = style.textContent;
                        const afterLink = link.nextSibling === style;
                        const list = document.styleSheets;
                        const sheet = link.sheet;
                        const rules = sheet.cssRules;
                        const cssom = {
                            listed: list.length === 1 && list[0] === sheet,
                            stable: link.sheet === sheet && sheet.cssRules === rules,
                            owner: sheet.ownerNode === link,
                            href: sheet.href,
                            selectors: Array.from(rules, rule => rule.selectorText),
                        };
                        link.remove();
                        return {
                            afterLink,
                            importedBeforeRoute:
                                css.indexOf("color:red") < css.indexOf("display:grid"),
                            importedUrl:
                                css.includes("http://example.com/assets/theme/grain.png"),
                            routeUrl:
                                css.includes("http://example.com/img/card.png"),
                            removedWithLink:
                                !document.querySelector("style[data-obscura-linked]"),
                            cssom,
                            detachedCssom: sheet.ownerNode === null
                                && link.sheet === null
                                && list.length === 0,
                        };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "afterLink": true,
                "importedBeforeRoute": true,
                "importedUrl": true,
                "routeUrl": true,
                "removedWithLink": true,
                "cssom": {
                    "listed": true,
                    "stable": true,
                    "owner": true,
                    "href": "http://example.com/assets/route.css",
                    "selectors": [".card", ".card"],
                },
                "detachedCssom": true,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unsuccessful_dynamic_script_response_fires_error_without_evaluating_body() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    try {
                        Deno.core.ops.op_fetch_url = (url) => JSON.stringify({
                            status: 401,
                            headers: { "content-type": "application/json" },
                            body: "globalThis.__executedFailedScript = true",
                            url,
                        });
                        const script = document.createElement("script");
                        script.src = "/unauthorized.js";
                        const outcome = await new Promise(resolve => {
                            script.onload = () => resolve("load");
                            script.onerror = () => resolve("error");
                            document.head.appendChild(script);
                        });
                        return {
                            outcome,
                            executed: globalThis.__executedFailedScript === true,
                        };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "outcome": "error",
                "executed": false,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_classic_scripts_are_async_by_default_but_honor_async_false_order() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    const runPair = async (explicitlyInOrder) => {
                        globalThis.__dynamicOrder = [];
                        Deno.core.ops.op_fetch_url = (url) => new Promise(resolve => {
                            const slow = url.includes("slow");
                            setTimeout(() => resolve(JSON.stringify({
                                status: 200,
                                headers: {"content-type": "text/javascript"},
                                body: `globalThis.__dynamicOrder.push("${slow ? "slow" : "fast"}")`,
                                url,
                            })), slow ? 30 : 1);
                        });
                        const load = name => new Promise(resolve => {
                            const script = document.createElement("script");
                            if (explicitlyInOrder) script.async = false;
                            script.src = `/${name}.js`;
                            script.onload = resolve;
                            document.head.appendChild(script);
                        });
                        await Promise.all([load("slow"), load("fast")]);
                        return globalThis.__dynamicOrder.slice();
                    };
                    try {
                        const asyncOrder = await runPair(false);
                        const inOrder = await runPair(true);
                        return {
                            asyncOrder,
                            inOrder,
                            pending: globalThis.__obscura_hasPendingDynamicScripts(),
                        };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "asyncOrder": ["fast", "slow"],
                "inOrder": ["slow", "fast"],
                "pending": false,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_response_array_buffer_preserves_typed_array_view() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                const bytes = new Uint8Array([9, 0, 97, 115, 109, 1, 8]);
                const response = new Response(bytes.subarray(1, 6));
                return Array.from(new Uint8Array(await response.arrayBuffer()));
            }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([0, 97, 115, 109, 1])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_wasm_instantiate_streaming_uses_response_array_buffer() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                const bytes = new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]);
                const result = await WebAssembly.instantiateStreaming(
                    Promise.resolve(new Response(bytes)),
                    {},
                );
                return result.instance instanceof WebAssembly.Instance;
            }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!(true));
    }

    #[test]
    fn test_text_decoder_respects_typed_array_view() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate("new TextDecoder().decode(new Uint8Array([65, 66, 67]).subarray(1, 2))")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "B");
    }

    #[test]
    fn test_document_doctype() {
        let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
        let result = rt.evaluate("document.doctype !== null").unwrap();
        assert_eq!(result, serde_json::json!(true));

        let name = rt.evaluate("document.doctype.name").unwrap();
        assert_eq!(name, serde_json::json!("html"));

        let node_type = rt.evaluate("document.doctype.nodeType").unwrap();
        assert_eq!(node_type.as_f64().unwrap() as i64, 10);
    }

    #[test]
    fn test_document_doctype_null_when_missing() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("document.doctype === null").unwrap();
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    fn test_xml_serializer_doctype() {
        let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
        let result = rt
            .evaluate("new XMLSerializer().serializeToString(document.doctype)")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "<!DOCTYPE html>");
    }

    #[test]
    fn test_xml_serializer_element() {
        let mut rt = setup_runtime(r#"<html><body><div id="x">Hello</div></body></html>"#);
        let result = rt
            .evaluate("new XMLSerializer().serializeToString(document.getElementById('x'))")
            .unwrap();
        let html = result.as_str().unwrap();
        assert!(html.contains("<div"));
        assert!(html.contains("Hello"));
    }

    #[test]
    fn test_create_event_custom_event_has_init_method() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let kind = rt
            .evaluate("typeof document.createEvent('CustomEvent').initCustomEvent")
            .unwrap();
        assert_eq!(kind, serde_json::json!("function"));
    }

    #[test]
    fn test_init_custom_event_sets_fields() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "test",
            r#"
            globalThis.__e = document.createEvent('CustomEvent');
            globalThis.__e.initCustomEvent('myevent', true, false, {hello: 'world'});
        "#,
        )
        .unwrap();
        let t = rt.evaluate("globalThis.__e.type").unwrap();
        assert_eq!(t, serde_json::json!("myevent"));
        let b = rt.evaluate("globalThis.__e.bubbles").unwrap();
        assert_eq!(b, serde_json::json!(true));
        let c = rt.evaluate("globalThis.__e.cancelable").unwrap();
        assert_eq!(c, serde_json::json!(false));
        let d = rt.evaluate("globalThis.__e.detail.hello").unwrap();
        assert_eq!(d, serde_json::json!("world"));
    }

    #[test]
    fn test_create_event_returns_correct_class() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let cust = rt
            .evaluate("document.createEvent('CustomEvent') instanceof CustomEvent")
            .unwrap();
        assert_eq!(cust, serde_json::json!(true));
        let mouse = rt
            .evaluate("document.createEvent('MouseEvent') instanceof MouseEvent")
            .unwrap();
        assert_eq!(mouse, serde_json::json!(true));
        let mouses = rt
            .evaluate("document.createEvent('MouseEvents') instanceof MouseEvent")
            .unwrap();
        assert_eq!(mouses, serde_json::json!(true));
        let kb = rt
            .evaluate("document.createEvent('KeyboardEvent') instanceof KeyboardEvent")
            .unwrap();
        assert_eq!(kb, serde_json::json!(true));
        let hash_change = rt
            .evaluate("document.createEvent('HashChangeEvent') instanceof HashChangeEvent")
            .unwrap();
        assert_eq!(hash_change, serde_json::json!(true));
        let message = rt
            .evaluate("document.createEvent('MessageEvent') instanceof MessageEvent")
            .unwrap();
        assert_eq!(message, serde_json::json!(true));
    }

    #[test]
    fn cssstyledeclaration_is_a_usable_global_interface() {
        // CSSStyleDeclaration was pre-declared non-enumerable but never assigned
        // a value (the only WebIDL interface missing its globalThis.X = X line),
        // so it was `undefined` while `'CSSStyleDeclaration' in window` was true,
        // and `el.style instanceof CSSStyleDeclaration` threw. It must be a real
        // constructor, non-enumerable like a browser, and the type of .style.
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var d=Object.getOwnPropertyDescriptor(window,'CSSStyleDeclaration');return (typeof window.CSSStyleDeclaration)+'|'+(document.body.style instanceof CSSStyleDeclaration)+'|'+(d?d.enumerable:'missing');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("function|true|false"));
    }

    #[test]
    fn test_create_event_rejects_unknown_interface() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    try {
                        document.createEvent('NotAnEventInterface');
                        return null;
                    } catch (error) {
                        return [error.name, error instanceof DOMException];
                    }
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["NotSupportedError", true]));
    }

    #[test]
    fn event_constructor_matches_webidl_conformance() {
        // new Event()/new CustomEvent() must throw (type is a required arg),
        // the type argument must be coerced to a string, CustomEvent.detail must
        // default to null (not undefined), createEvent must still build a
        // type-"" event, and an explicit detail must be preserved.
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){\
                 var out=[];\
                 try{new Event();out.push('no-throw')}catch(e){out.push(e.name)}\
                 try{new CustomEvent();out.push('no-throw')}catch(e){out.push(e.name)}\
                 out.push(new Event(123).type+':'+typeof new Event(123).type);\
                 out.push(String(new CustomEvent('x').detail));\
                 out.push(String(new CustomEvent('x',{detail:7}).detail));\
                 out.push(new Event('click').type);\
                 out.push(JSON.stringify(document.createEvent('Event').type));\
                 return out.join('|');\
                 })()",
            )
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!("TypeError|TypeError|123:string|null|7|click|\"\"")
        );
    }

    #[test]
    fn test_promise_rejection_event_requires_promise() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const promise = Promise.resolve(1);
                    const event = new PromiseRejectionEvent('unhandledrejection', {
                        promise,
                        reason: 'failed'
                    });
                    let missingPromiseThrows = false;
                    try {
                        new PromiseRejectionEvent('unhandledrejection');
                    } catch (error) {
                        missingPromiseThrows = error instanceof TypeError;
                    }
                    return [
                        event instanceof Event,
                        event.promise === promise,
                        event.reason === 'failed',
                        missingPromiseThrows
                    ];
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true, true, true]));
    }

    #[test]
    fn test_create_event_rejects_promise_rejection_event() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    try {
                        document.createEvent('PromiseRejectionEvent');
                        return null;
                    } catch (error) {
                        return [error.name, error instanceof DOMException];
                    }
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["NotSupportedError", true]));
    }

    #[test]
    fn test_create_event_supports_legacy_event_aliases() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"['Event', 'Events', 'HTMLEvents', 'SVGEvents'].map(name => {
                    const event = document.createEvent(name);
                    return [event instanceof Event, event.constructor === Event, event.type];
                })"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, true, ""],
                [true, true, ""],
                [true, true, ""],
                [true, true, ""]
            ])
        );
    }

    #[test]
    fn test_storage_event_constructor_and_legacy_factory() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const event = new StorageEvent('storage', {
                        key: 'theme',
                        oldValue: 'light',
                        newValue: 'dark',
                        url: 'https://example.test/'
                    });
                    const legacy = document.createEvent('StorageEvent');
                    legacy.initStorageEvent(
                        'storage', false, false, 'count', '1', '2',
                        'https://example.test/', null
                    );
                    return [
                        event instanceof Event,
                        event.key,
                        event.oldValue,
                        event.newValue,
                        event.url,
                        legacy instanceof StorageEvent,
                        legacy.key,
                        legacy.newValue
                    ];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                "theme",
                "light",
                "dark",
                "https://example.test/",
                true,
                "count",
                "2"
            ])
        );
    }

    #[test]
    fn test_html_to_markdown_headings() {
        let mut rt =
            setup_runtime("<html><body><h1>Title</h1><h2>Sub</h2><p>Body</p></body></html>");
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("# Title"), "missing H1: {}", md);
        assert!(md.contains("## Sub"), "missing H2: {}", md);
        assert!(md.contains("Body"), "missing paragraph text: {}", md);
    }

    #[test]
    fn test_html_to_markdown_links_and_inline() {
        let mut rt = setup_runtime(
            r#"<html><body><p>Hello <strong>world</strong> <a href="https://x.test/">link</a> <em>em</em></p></body></html>"#,
        );
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("**world**"), "missing strong: {}", md);
        assert!(md.contains("*em*"), "missing em: {}", md);
        assert!(
            md.contains("[link](https://x.test/)"),
            "missing link: {}",
            md
        );
    }

    #[test]
    fn test_html_to_markdown_lists() {
        let mut rt = setup_runtime(
            "<html><body><ul><li>A</li><li>B</li></ul><ol><li>X</li><li>Y</li></ol></body></html>",
        );
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("- A"), "missing unordered A: {}", md);
        assert!(md.contains("- B"), "missing unordered B: {}", md);
        assert!(md.contains("1. X"), "missing ordered X: {}", md);
    }

    #[test]
    fn test_html_to_markdown_skips_script_and_style() {
        let mut rt = setup_runtime(
            "<html><body><p>Text</p><script>alert(1)</script><style>body{color:red}</style></body></html>",
        );
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("Text"), "missing visible text: {}", md);
        assert!(!md.contains("alert"), "leaked script content: {}", md);
        assert!(!md.contains("color:red"), "leaked style content: {}", md);
    }

    #[test]
    fn test_page_content_puppeteer_pattern() {
        let mut rt =
            setup_runtime("<!DOCTYPE html><html><head></head><body><p>Test</p></body></html>");
        let result = rt.evaluate(
            "(function() { let retVal = ''; if (document.doctype) retVal = new XMLSerializer().serializeToString(document.doctype); if (document.documentElement) retVal += document.documentElement.outerHTML; return retVal; })()"
        ).unwrap();
        let html = result.as_str().unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html>"));
        assert!(html.contains("<p>Test</p>"));
    }

    #[test]
    fn test_element_from_point_is_function() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let kind = rt.evaluate("typeof document.elementFromPoint").unwrap();
        assert_eq!(kind, serde_json::json!("function"));
        let kind2 = rt.evaluate("typeof document.elementsFromPoint").unwrap();
        assert_eq!(kind2, serde_json::json!("function"));
    }

    #[test]
    fn test_element_from_point_in_viewport_returns_body() {
        let mut rt = setup_runtime("<html><body><h1>Hi</h1></body></html>");
        let tag = rt
            .evaluate("document.elementFromPoint(10, 10)?.tagName")
            .unwrap();
        assert_eq!(tag, serde_json::json!("BODY"));
    }

    #[test]
    fn test_element_from_point_out_of_viewport_returns_null() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let neg_x = rt.evaluate("document.elementFromPoint(-1, 10)").unwrap();
        assert_eq!(neg_x, serde_json::Value::Null);
        let neg_y = rt.evaluate("document.elementFromPoint(10, -1)").unwrap();
        assert_eq!(neg_y, serde_json::Value::Null);
        let huge = rt
            .evaluate("document.elementFromPoint(99999, 99999)")
            .unwrap();
        assert_eq!(huge, serde_json::Value::Null);
    }

    #[test]
    fn test_elements_from_point_returns_array() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let len_in = rt
            .evaluate("document.elementsFromPoint(10, 10).length")
            .unwrap();
        assert_eq!(len_in.as_f64().unwrap() as i64, 1);
        let len_out = rt
            .evaluate("document.elementsFromPoint(-1, -1).length")
            .unwrap();
        assert_eq!(len_out.as_f64().unwrap() as i64, 0);
    }

    #[test]
    fn test_element_from_point_non_numeric_returns_null() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let nan = rt.evaluate("document.elementFromPoint(NaN, 10)").unwrap();
        assert_eq!(nan, serde_json::Value::Null);
        let inf = rt
            .evaluate("document.elementFromPoint(Infinity, 10)")
            .unwrap();
        assert_eq!(inf, serde_json::Value::Null);
    }

    fn spawn_one_response_server(status: &str, body: &str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{}", address)
    }

    fn spawn_duplicate_module_graph_server() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/");
                let body = match path {
                    "/entry.js" => {
                        "import './shared.js'; globalThis.__module_entry_ran = true;"
                    }
                    "/shared.js" => {
                        "globalThis.__shared_module_runs = \
                         (globalThis.__shared_module_runs || 0) + 1;"
                    }
                    _ => "throw new Error('unexpected module path');",
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/javascript\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        format!("http://{}", address)
    }

    #[derive(Clone, Copy)]
    enum ModuleGraphFixture {
        CookieProtected,
        RedirectedChild,
    }

    fn spawn_module_graph_server(
        fixture: ModuleGraphFixture,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        let request_count = match fixture {
            ModuleGraphFixture::CookieProtected => 2,
            ModuleGraphFixture::RedirectedChild => 3,
        };
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = vec![0u8; 8192];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]).to_string();
                let lower_request = request.to_ascii_lowercase();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                requests_tx.send(request.clone()).unwrap();

                let (status, extra_headers, body) = match (fixture, path.as_str()) {
                    (ModuleGraphFixture::CookieProtected, "/entry.js") => (
                        "200 OK",
                        "",
                        "import { value } from './child.js'; \
                         globalThis.__module_graph_value = value;",
                    ),
                    (ModuleGraphFixture::CookieProtected, "/child.js")
                        if lower_request.contains("\r\ncookie: session=ok\r\n")
                            && lower_request
                                .contains("\r\nuser-agent: modulegraphtest/1.0\r\n")
                            && lower_request.contains("\r\nx-module-test: shared\r\n") =>
                    {
                        ("200 OK", "", "export const value = 'cookie-child';")
                    }
                    (ModuleGraphFixture::CookieProtected, "/child.js") => (
                        "401 Unauthorized",
                        "",
                        "throw new Error('page request context missing');",
                    ),
                    (ModuleGraphFixture::RedirectedChild, "/entry.js") => (
                        "200 OK",
                        "",
                        "import { value } from './redirect.js'; \
                         globalThis.__module_graph_value = value;",
                    ),
                    (ModuleGraphFixture::RedirectedChild, "/redirect.js") => {
                        ("302 Found", "Location: /child.js\r\n", "")
                    }
                    (ModuleGraphFixture::RedirectedChild, "/child.js") => {
                        ("200 OK", "", "export const value = 'redirect-child';")
                    }
                    _ => ("404 Not Found", "", "not found"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n\
                     Content-Type: application/javascript\r\n\
                     {extra_headers}Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{}", address), requests_rx)
    }

    fn spawn_import_map_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                requests_tx.send(path.clone()).unwrap();
                let (status, body) = match path.as_str() {
                    "/vendor/pkg/feature.js" => ("200 OK", "export const value = 'prefix-static';"),
                    "/vendor/dynamic.js" => ("200 OK", "export const value = 'exact-dynamic';"),
                    _ => ("404 Not Found", "not found"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n\
                     Content-Type: application/javascript\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{}", address), requests_rx)
    }

    fn spawn_root_module_import_map_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            requests_tx.send(path.clone()).unwrap();
            let body = if path == "/entry.js" {
                "globalThis.__root_module_identity = 'entry';"
            } else {
                "globalThis.__root_module_identity = 'remapped';"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/javascript\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{}", address), requests_rx)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn entry_module_http_failure_is_not_evaluated_as_empty_source() {
        let base = spawn_one_response_server("404 Not Found", "not found");
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_http_client(client);

        let error = rt
            .load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap_err();
        assert!(
            error.contains("HTTP 404"),
            "expected entry fetch status in error, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dependency_prepared_as_root_is_evaluated_only_once() {
        let base = spawn_duplicate_module_graph_server();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_http_client(client);

        // The HTML scheduler prepares all module graphs before evaluating any
        // of them. The shared URL is both a dependency and a later root, which
        // used to reach deno_core::mod_evaluate twice and panic (#591).
        let entry = rt
            .prepare_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();
        let shared = rt
            .prepare_module(&format!("{}/shared.js", base), 1_000)
            .await
            .unwrap();

        rt.evaluate_prepared_module(entry, 1_000).await.unwrap();
        rt.evaluate_prepared_module(shared, 1_000).await.unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__module_entry_ran === true").unwrap(),
            serde_json::json!(true),
        );
        assert_eq!(
            rt.evaluate("globalThis.__shared_module_runs").unwrap(),
            serde_json::json!(1.0),
        );
    }

    #[test]
    fn heap_limit_terminates_script_and_runtime_recovers() {
        crate::v8_flags::set_v8_flags("--max-old-space-size=32 --max-semi-space-size=1");
        let mut rt = ObscuraJsRuntime::new();

        for _ in 0..2 {
            let error = rt
                .evaluate(
                    "(() => { const chunks = []; for (;;) { \
                     chunks.push(new Array(262144).fill(1.25)); } })()",
                )
                .unwrap_err();
            assert!(
                error.contains("heap limit exceeded"),
                "unexpected heap failure: {error}",
            );
            assert_eq!(
                rt.evaluate("globalThis.__runtime_survived_oom = true").unwrap(),
                serde_json::json!(true),
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn descendant_module_uses_page_cookie_identity_and_headers() {
        let (base, requests) = spawn_module_graph_server(ModuleGraphFixture::CookieProtected);
        let page_url = url::Url::parse(&format!("{}/", base)).unwrap();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        jar.set_cookie("session=ok; Path=/", &page_url);
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar.clone(),
            None,
            true,
        ));
        client.set_user_agent("ModuleGraphTest/1.0").await;
        client
            .set_extra_headers(std::collections::HashMap::from([(
                "x-module-test".to_string(),
                "shared".to_string(),
            )]))
            .await;
        let callback_urls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let callbacks = std::sync::Arc::new(obscura_net::CallbackRegistry::new());
        let callback_urls_capture = callback_urls.clone();
        callbacks.add_request(std::sync::Arc::new(move |request| {
            callback_urls_capture
                .lock()
                .unwrap()
                .push(request.url.path().to_string());
        }));

        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_cookie_jar(jar);
        rt.set_http_client(client);
        rt.set_callbacks(callbacks);
        rt.load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__module_graph_value").unwrap(),
            serde_json::json!("cookie-child"),
        );
        let requests = (0..2)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(requests
            .iter()
            .any(|request| request.starts_with("GET /entry.js ")));
        let child = requests
            .iter()
            .find(|request| request.starts_with("GET /child.js "))
            .expect("descendant request");
        let child_lower = child.to_ascii_lowercase();
        assert!(
            child_lower.contains("\r\ncookie: session=ok\r\n"),
            "{child}"
        );
        assert!(
            child_lower.contains("\r\nuser-agent: modulegraphtest/1.0\r\n"),
            "{child}"
        );
        assert!(
            child_lower.contains("\r\nx-module-test: shared\r\n"),
            "{child}"
        );
        assert_eq!(
            *callback_urls.lock().unwrap(),
            vec!["/entry.js", "/child.js"],
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cross_origin_module_descendant_does_not_gain_module_origin_cookies() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let module_base = format!("http://{address}");
        let document_url = "http://127.0.0.1:1/page";
        let document_origin = "http://127.0.0.1:1";
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]).to_string();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/");
                let body = match path {
                    "/entry.js" => {
                        "import { value } from './child.js'; globalThis.__cors_value = value;"
                    }
                    "/child.js" => "export const value = 'safe';",
                    _ => "throw new Error('unexpected module path');",
                };
                requests_tx.send(request).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/javascript\r\n\
                     Access-Control-Allow-Origin: {document_origin}\r\n\
                     Cache-Control: public, max-age=3600\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let module_origin = url::Url::parse(&module_base).unwrap();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        jar.set_cookie("cdn_session=secret; Path=/", &module_origin);
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(document_url);
        rt.set_http_client(client);
        rt.load_module(&format!("{module_base}/entry.js"), 1_000)
            .await
            .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__cors_value").unwrap(),
            serde_json::json!("safe"),
        );
        let requests = (0..2)
            .map(|_| {
                requests_rx
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for request in &requests {
            let lower = request.to_ascii_lowercase();
            assert!(lower.contains("\r\norigin: http://127.0.0.1:1\r\n"), "{request}");
            assert!(!lower.contains("\r\ncookie:"), "{request}");
        }
        let child = requests
            .iter()
            .find(|request| request.starts_with("GET /child.js "))
            .expect("child module request")
            .to_ascii_lowercase();
        assert!(
            child.contains(&format!("\r\nreferer: {module_base}/entry.js\r\n")),
            "{child}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn descendant_module_follows_page_client_redirects() {
        let (base, requests) = spawn_module_graph_server(ModuleGraphFixture::RedirectedChild);
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_http_client(client);
        rt.load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__module_graph_value").unwrap(),
            serde_json::json!("redirect-child"),
        );
        let paths = (0..3)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "GET /entry.js HTTP/1.1",
                "GET /redirect.js HTTP/1.1",
                "GET /child.js HTTP/1.1",
            ],
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn import_map_resolves_prefix_static_and_exact_dynamic_imports() {
        let (base, requests) = spawn_import_map_server();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/app/index.html", base));
        rt.set_http_client(client);
        rt.add_import_map(
            r#"{
                "imports": {
                    "pkg/": "../vendor/pkg/",
                    "dynamic-pkg": "../vendor/dynamic.js"
                }
            }"#,
            &format!("{}/config/import-map.json", base),
        )
        .unwrap();

        rt.load_inline_module(
            "import { value as prefix } from 'pkg/feature.js'; \
             const dynamic = (await import('dynamic-pkg')).value; \
             globalThis.__import_map_values = [prefix, dynamic];",
            &format!("{}/app/index.html", base),
            1_000,
        )
        .await
        .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__import_map_values").unwrap(),
            serde_json::json!(["prefix-static", "exact-dynamic"]),
        );
        let paths = (0..2)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            paths.contains(&"/vendor/pkg/feature.js".to_string()),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"/vendor/dynamic.js".to_string()),
            "{paths:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn import_map_does_not_remap_external_root_module_url() {
        let (base, requests) = spawn_root_module_import_map_server();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/index.html", base));
        rt.set_http_client(client);
        rt.add_import_map(
            &format!(r#"{{"imports":{{"{base}/entry.js":"{base}/remapped.js"}}}}"#),
            &format!("{}/index.html", base),
        )
        .unwrap();

        rt.load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__root_module_identity").unwrap(),
            serde_json::json!("entry"),
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/entry.js",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_modules_expose_document_base_as_import_meta_url() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/page/index.html");
        rt.load_inline_module(
            "globalThis.__first_inline_url = import.meta.url;",
            "https://example.com/base/",
            1_000,
        )
        .await
        .unwrap();
        rt.load_inline_module(
            "globalThis.__second_inline_url = import.meta.url;",
            "https://example.com/base/",
            1_000,
        )
        .await
        .unwrap();

        assert_eq!(
            rt.evaluate("[globalThis.__first_inline_url, globalThis.__second_inline_url]")
                .unwrap(),
            serde_json::json!(["https://example.com/base/", "https://example.com/base/"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn classic_script_url_is_dynamic_import_referrer() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let request_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let path = String::from_utf8_lossy(&request[..length])
                .lines()
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let body = "export const value = 'scoped-classic';";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
            path
        });
        let base = format!("http://{address}");
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{base}/page/index.html"));
        rt.set_http_client(client);
        rt.set_dom(parse_html("<html><body></body></html>"));
        rt.run_page_init();
        rt.add_import_map(
            &format!(r#"{{"scopes":{{"{base}/classic/":{{"pkg":"{base}/scoped.js"}}}}}}"#),
            &format!("{base}/page/index.html"),
        )
        .unwrap();

        rt.execute_script(
            &format!("{base}/classic/entry.js"),
            "document.documentElement.setAttribute('data-classic-op', 'ran'); \
             import('pkg').then(module => { globalThis.__classic_import = module.value; });",
        )
        .unwrap();
        rt.run_event_loop().await.unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__classic_import").unwrap(),
            serde_json::json!("scoped-classic")
        );
        assert_eq!(
            rt.evaluate("document.documentElement.getAttribute('data-classic-op')")
                .unwrap(),
            serde_json::json!("ran")
        );
        assert_eq!(request_thread.join().unwrap(), "/scoped.js");
    }

    #[test]
    fn timed_out_classic_script_leaves_runtime_reusable() {
        let mut rt = ObscuraJsRuntime::new();
        rt.execute_script_with_timeout(
            "https://example.test/hang.js",
            "while (true) {}",
            std::time::Duration::from_millis(20),
        )
        .unwrap();
        rt.execute_script(
            "https://example.test/after-timeout.js",
            "globalThis.__after_timeout = true;",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__after_timeout").unwrap(),
            serde_json::json!(true)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_module_graph_error_propagates() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        let error = rt
            .load_inline_module("import 'bare-specifier';", "https://example.com/", 1_000)
            .await
            .unwrap_err();
        assert!(
            error.contains("Inline module load error"),
            "expected graph load error, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_module_evaluation_error_propagates() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        let error = rt
            .load_inline_module(
                "throw new Error('module-evaluation-boom');",
                "https://example.com/",
                1_000,
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("Inline module eval error") && error.contains("module-evaluation-boom"),
            "expected evaluation error, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_module_evaluation_timeout_propagates() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        let error = rt
            .load_inline_module(
                "await new Promise(resolve => setTimeout(resolve, 10000));",
                "https://example.com/",
                20,
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("Inline module evaluation timed out after"),
            "expected evaluation timeout, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_inline_module_does_not_wait_for_interval_idle() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        rt.load_inline_module(
            "globalThis.__module_loaded = true; setInterval(() => {}, 10000);",
            "https://example.com/",
            500,
        )
        .await
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__module_loaded").unwrap(),
            serde_json::json!(true)
        );
    }

    // Issue #139 — proxy_url must thread through to both the ES-module
    // loader (module_loader.rs) and op_fetch_url's reqwest client
    // (ops.rs::build_request_client). Pre-fix both built clients with
    // `Client::builder().build()` — no proxy — so JS fetch/XHR and
    // dynamic imports silently bypassed BrowserContext.proxy_url.
    //
    // Phase 5.5 RED check: each test references a symbol that does NOT
    // exist on main (proxy_url() accessor, with_proxy ctor,
    // with_base_url_and_proxy ctor), so the tests fail to compile without
    // the prod fix.
    #[test]
    fn http_client_round_trips_proxy_url() {
        use obscura_net::{CookieJar, ObscuraHttpClient};
        let jar = std::sync::Arc::new(CookieJar::new());
        let configured =
            ObscuraHttpClient::with_options(jar.clone(), Some("http://proxy.test:8080"));
        assert_eq!(
            configured.proxy_url(),
            Some("http://proxy.test:8080"),
            "proxy_url() must expose the value passed to with_options"
        );

        let direct = ObscuraHttpClient::with_options(jar, None);
        assert_eq!(
            direct.proxy_url(),
            None,
            "proxy_url() must return None when no proxy was configured"
        );
    }

    #[test]
    fn module_loader_stores_proxy_for_dynamic_imports() {
        use crate::module_loader::ObscuraModuleLoader;
        let loader = ObscuraModuleLoader::with_proxy(
            "https://example.com/",
            Some("http://proxy.test:8080".to_string()),
        );
        assert_eq!(loader.proxy_url.as_deref(), Some("http://proxy.test:8080"));
        assert_eq!(loader.base_url, "https://example.com/");

        // Default constructor must keep the historical "no proxy" behaviour.
        let direct = ObscuraModuleLoader::new("https://example.com/");
        assert_eq!(direct.proxy_url, None);
    }

    #[test]
    fn runtime_with_base_url_and_proxy_constructs_successfully() {
        // Sanity-check the public ctor that page.rs uses to thread proxy
        // through to the module loader. Direct (None) and proxied paths
        // must both initialise the JS environment.
        let _direct = ObscuraJsRuntime::with_base_url_and_proxy("https://example.com/", None);
        let _proxied = ObscuraJsRuntime::with_base_url_and_proxy(
            "https://example.com/",
            Some("http://proxy.test:8080".to_string()),
        );
    }

    // ── Issue #45 (Playwright actionability) regression tests ────────────────
    // Kept at the end of the module so they don't share textual context with
    // unrelated test additions in other branches (avoids spurious merge
    // conflicts when both this branch and an unrelated bootstrap.js change
    // add tests near the start of `mod tests`).

    /// Playwright >= 1.25 calls `element.checkVisibility(...)` before every
    /// input event. If the method isn't defined Playwright retries until its
    /// action timeout fires. Without a layout engine we can't compute it
    /// properly, so the stub always returns true — still strictly better
    /// than the undefined path.
    #[test]
    fn element_check_visibility_is_callable() {
        let mut rt = setup_runtime(r#"<div id="x">x</div>"#);
        let result = rt
            .evaluate("document.getElementById('x').checkVisibility({checkOpacity: true})")
            .unwrap();
        assert_eq!(result, serde_json::json!(true));

        let typeof_method = rt
            .evaluate("typeof document.getElementById('x').checkVisibility")
            .unwrap();
        assert_eq!(typeof_method, serde_json::json!("function"));
    }

    /// Playwright's `getByRole` / `getByLabel` locators resolve via ARIA
    /// reflection properties. Without the getters those locators always
    /// fail. Reflect the underlying aria-* attributes.
    #[test]
    fn element_aria_reflection_properties_read_aria_attrs() {
        let mut rt = setup_runtime(
            r#"<button id="b" role="tab" aria-label="Settings" aria-selected="true">x</button>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const el = document.getElementById('b');
                return [el.role, el.ariaLabel, el.ariaSelected];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["tab", "Settings", "true"]));
    }

    /// Setting an ARIA reflection property must write through to the
    /// underlying attribute so frameworks that toggle state via
    /// `el.ariaExpanded = 'true'` actually update the DOM.
    /// Regression: React 18 / mobile SPAs (e.g. goofish.com) call
    /// addEventListener on navigator.connection (NetworkInformation) and
    /// navigator.serviceWorker (ServiceWorkerContainer). Both are EventTargets
    /// in real browsers; missing the method crashed the app bundle with
    /// "addEventListener is not a function".
    #[test]
    fn navigator_eventtarget_stubs_expose_add_event_listener() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
                const connection = navigator.connection;
                let calls = 0;
                let receiverMatches = false;
                function listener(event) {
                    calls += 1;
                    receiverMatches = this === connection && event.type === 'change';
                }
                connection.addEventListener('change', listener);
                const dispatchResult = connection.dispatchEvent(new Event('change'));
                connection.removeEventListener('change', listener);
                connection.dispatchEvent(new Event('change'));
                return [
                    typeof connection.addEventListener,
                    typeof connection.removeEventListener,
                    typeof connection.dispatchEvent,
                    typeof navigator.serviceWorker.addEventListener,
                    dispatchResult,
                    calls,
                    receiverMatches,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["function", "function", "function", "function", true, 1, true])
        );
    }

    #[test]
    fn text_codec_streams_expose_browser_shape() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
                const encoder = new TextEncoderStream();
                const decoder = new TextDecoderStream();
                return {
                    encoder: encoder.encoding,
                    encoderReadable: typeof encoder.readable.getReader,
                    encoderWritable: typeof encoder.writable.getWriter,
                    decoder: decoder.encoding,
                    decoderReadable: typeof decoder.readable.getReader,
                    decoderWritable: typeof decoder.writable.getWriter,
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "encoder": "utf-8",
                "encoderReadable": "function",
                "encoderWritable": "function",
                "decoder": "utf-8",
                "decoderReadable": "function",
                "decoderWritable": "function",
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_encoder_stream_pipe_through_delivers_hydration_data() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate_for_cdp(
                r#"
                (async () => {
                    let sourceController;
                    const source = new ReadableStream({
                        start(controller) { sourceController = controller; },
                    });
                    const encoded = source.pipeThrough(new TextEncoderStream());
                    sourceController.enqueue('["server",{"hydrated":true}]\n');
                    sourceController.close();

                    const decoder = new TextDecoder();
                    let tail = "";
                    const lines = encoded.pipeThrough(new TransformStream({
                        transform(chunk, controller) {
                            const complete = (tail + decoder.decode(chunk, {stream: true})).split("\n");
                            tail = complete.pop() || "";
                            for (const line of complete) controller.enqueue(line);
                        },
                        flush(controller) { if (tail) controller.enqueue(tail); },
                    }));
                    const first = await lines.getReader().read();
                    return JSON.parse(first.value);
                })()
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!(["server", {"hydrated": true}])
        );
    }

    /// Regression test for #285: DDoS-Guard's challenge calls
    /// `t.insertAdjacentText(...)` and dies with `TypeError: ... is not a
    /// function` because `Element.prototype.insertAdjacentText` was missing.
    /// Verify all four positions place a Text node (NOT parsed HTML) at the
    /// right spot. Tests `insertAdjacentText` exists, is callable, and that
    /// inserted content remains literal text — angle brackets must not be
    /// parsed as markup, which is the whole point of the API.
    #[test]
    fn element_insert_adjacent_text_polyfill() {
        let mut rt = setup_runtime(r#"<div id="p"><span id="t">X</span></div>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                t.insertAdjacentText('afterbegin', 'AB');
                t.insertAdjacentText('beforeend', 'BE');
                t.insertAdjacentText('beforebegin', 'BB');
                t.insertAdjacentText('afterend', 'AE');
                t.insertAdjacentText('beforeend', '<b>raw</b>');
                return [
                    typeof Element.prototype.insertAdjacentText,
                    document.getElementById('p').textContent,
                    t.getElementsByTagName('b').length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["function", "BBABXBE<b>raw</b>AE", 0])
        );
    }

    /// Regression test for #285: `Element.prototype.insertAdjacentElement`
    /// was missing alongside `insertAdjacentText`. Verify all four positions
    /// place the given element correctly and that the inserted element is
    /// returned (per spec — that's the contract callers rely on for chaining).
    #[test]
    fn element_insert_adjacent_element_polyfill() {
        let mut rt = setup_runtime(r#"<div id="p"><span id="t">X</span></div>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                const before = document.createElement('b');  before.id = 'before';
                const after  = document.createElement('i');  after.id  = 'after';
                const inside = document.createElement('em'); inside.id = 'inside';
                const last   = document.createElement('u');  last.id   = 'last';
                const r1 = t.insertAdjacentElement('beforebegin', before);
                const r2 = t.insertAdjacentElement('afterend',    after);
                const r3 = t.insertAdjacentElement('afterbegin',  inside);
                const r4 = t.insertAdjacentElement('beforeend',   last);
                const siblings = Array.from(document.getElementById('p').children).map(c => c.id);
                const inT = Array.from(t.children).map(c => c.id);
                return [
                    typeof Element.prototype.insertAdjacentElement,
                    r1 === before && r2 === after && r3 === inside && r4 === last,
                    siblings,
                    inT,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "function",
                true,
                ["before", "t", "after"],
                ["inside", "last"]
            ])
        );
    }

    #[test]
    fn console_log_error_does_not_trigger_prepare_stack_trace() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
            let called = false;
            const saved = Error.prepareStackTrace;
            Error.prepareStackTrace = function() { called = true; return saved; };
            const e = new Error("test");
            console.log(e);
            Error.prepareStackTrace = saved;
            return called;
        "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(false));
    }

    #[test]
    fn element_aria_reflection_setters_write_through() {
        let mut rt = setup_runtime(r#"<div id="d"></div>"#);
        let result = rt
            .evaluate(
                r#"
                const el = document.getElementById('d');
                el.role = 'menu';
                el.ariaExpanded = 'true';
                return [el.getAttribute('role'), el.getAttribute('aria-expanded')];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["menu", "true"]));
    }

    /// Framework schedulers commonly subclass EventTarget for their own
    /// lifecycle events. These targets have no backing DOM node, but must
    /// still deliver callbacks (including object, once, and signal listeners).
    #[test]
    fn standalone_event_target_delivers_framework_lifecycle_events() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
                const TypedEventTarget = class extends EventTarget {};
                const target = new TypedEventTarget();
                const calls = [];
                const removed = () => calls.push("removed");
                const controller = new AbortController();
                const node = { id: "canvas-ref" };

                target.addEventListener("insert", (event) => calls.push(event.node.id));
                target.addEventListener("insert", { handleEvent() { calls.push("object"); } });
                target.addEventListener("insert", () => calls.push("once"), { once: true });
                target.addEventListener("insert", removed);
                target.removeEventListener("insert", removed);
                target.addEventListener("insert", () => calls.push("aborted"), {
                    signal: controller.signal,
                });
                controller.abort();

                const first = new Event("insert", { cancelable: true });
                first.node = node;
                const firstResult = target.dispatchEvent(first);
                const second = new Event("insert");
                second.node = node;
                const secondResult = target.dispatchEvent(second);
                return [
                    target instanceof EventTarget,
                    calls,
                    firstResult,
                    secondResult,
                    first.target === target,
                    first.currentTarget === null,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                ["canvas-ref", "object", "once", "canvas-ref", "object"],
                true,
                true,
                true,
                true
            ])
        );
    }

    #[test]
    fn media_text_tracks_expose_loaded_webvtt_cues() {
        let mut rt = setup_runtime(
            r#"<video><track id="captions" kind="captions" srclang="en" default
                src="data:text/vtt,WEBVTT%0A%0A00%3A00%3A01.000%20--%3E%2000%3A00%3A03.000%0AHello"></video>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const element = document.getElementById("captions");
                const video = document.querySelector("video");
                const cue = element.track.cues[0];
                const added = video.addTextTrack("metadata", "Data", "en");
                cue.line = -2;
                cue.size = 80;
                return [
                    element instanceof HTMLTrackElement,
                    element.readyState === HTMLTrackElement.LOADED,
                    element.track instanceof TextTrack,
                    element.track.cues.length,
                    cue.startTime,
                    cue.endTime,
                    cue.text,
                    cue.line,
                    cue.size,
                    video.textTracks.length,
                    video.textTracks.getTrackById("captions") === element.track,
                    added.kind,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, true, true, 1, 1, 3, "Hello", -2, 80, 1, true, "metadata"])
        );
    }

    #[test]
    fn unsupported_media_capabilities_and_readiness_are_honest() {
        let mut rt = setup_runtime(
            r#"<video id="media" src="https://example.test/movie.mp4"
                poster="https://example.test/poster.png"></video>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const media = document.getElementById("media");
                return [
                    media.canPlayType("video/mp4"),
                    media.canPlayType('video/webm; codecs="vp9"'),
                    media.readyState,
                    media.currentTime,
                    media.videoWidth,
                    media.videoHeight,
                    media.paused,
                    media.currentSrc,
                    media.poster,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "",
                "",
                0,
                0,
                0,
                0,
                true,
                "",
                "https://example.test/poster.png"
            ])
        );
    }

    #[test]
    fn html_string_scripts_remain_inert_when_connected() {
        let mut rt = setup_runtime("<html><head></head><body><div id=target></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__fragmentScriptRuns = 0;

                const direct = document.createElement("div");
                direct.innerHTML = "<script>globalThis.__fragmentScriptRuns++<\/script>";
                document.body.appendChild(direct.firstChild);

                const nested = document.createElement("div");
                nested.innerHTML = "<section><script>globalThis.__fragmentScriptRuns++<\/script></section>";
                document.body.appendChild(nested.firstChild);

                const template = document.createElement("template");
                template.innerHTML = "<script>globalThis.__fragmentScriptRuns++<\/script>";
                document.body.appendChild(template.content.firstChild);

                const nestedTemplateHolder = document.createElement("div");
                nestedTemplateHolder.innerHTML =
                    "<template><script>globalThis.__fragmentScriptRuns++<\/script></template>";
                document.body.appendChild(
                    nestedTemplateHolder.firstChild.content.firstChild
                );

                document.getElementById("target").insertAdjacentHTML(
                    "beforeend",
                    "<script>globalThis.__fragmentScriptRuns++<\/script>"
                );

                const parsed = new DOMParser().parseFromString(
                    "<body><script>globalThis.__fragmentScriptRuns++<\/script></body>",
                    "text/html"
                );
                document.body.appendChild(parsed.querySelector("script"));

                let externalFetches = 0;
                const originalFetchOp = Deno.core.ops.op_fetch_url;
                try {
                    Deno.core.ops.op_fetch_url = () => {
                        externalFetches++;
                        return JSON.stringify({
                            status: 200,
                            headers: {"content-type": "text/javascript"},
                            body: "globalThis.__fragmentScriptRuns++",
                            url: "http://example.com/inert.js"
                        });
                    };
                    const external = document.createElement("div");
                    external.innerHTML = "<script src=/inert.js><\/script>";
                    document.head.appendChild(external.firstChild);
                } finally {
                    Deno.core.ops.op_fetch_url = originalFetchOp;
                }
                return [globalThis.__fragmentScriptRuns, externalFetches];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([0, 0]));
    }

    #[test]
    fn connected_insertion_prepares_dynamic_script_subtrees_once() {
        let mut rt = setup_runtime("<html><head></head><body><i id=anchor></i></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__dynamicScriptRuns = [];

                const detached = document.createElement("div");
                const delayed = document.createElement("script");
                delayed.textContent = "globalThis.__dynamicScriptRuns.push('delayed')";
                detached.appendChild(delayed);
                const beforeConnection = globalThis.__dynamicScriptRuns.length;
                document.body.appendChild(detached);
                document.head.appendChild(delayed);

                const before = document.createElement("script");
                before.textContent = "globalThis.__dynamicScriptRuns.push('before')";
                document.body.insertBefore(before, document.getElementById("anchor"));

                const replacement = document.createElement("script");
                replacement.textContent = "globalThis.__dynamicScriptRuns.push('replace')";
                document.body.replaceChild(replacement, document.getElementById("anchor"));

                const subtree = document.createElement("section");
                const nested = document.createElement("script");
                nested.textContent = "globalThis.__dynamicScriptRuns.push('nested')";
                subtree.appendChild(nested);
                document.body.appendChild(subtree);

                return [beforeConnection, globalThis.__dynamicScriptRuns];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([0, ["delayed", "before", "replace", "nested"]])
        );
    }

    #[test]
    fn script_clone_preserves_started_state() {
        let mut rt = setup_runtime(
            "<html><head></head><body><script id=parser>globalThis.__cloneScriptRuns++</script></body></html>",
        );
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__cloneScriptRuns = 0;
                const parser = document.getElementById("parser");
                globalThis.__markParserScripts([parser._nid]);
                document.head.appendChild(parser);
                document.body.appendChild(parser.cloneNode(true));

                const dynamic = document.createElement("script");
                dynamic.textContent = "globalThis.__cloneScriptRuns++";
                document.body.appendChild(dynamic);
                document.body.appendChild(dynamic.cloneNode(true));

                const holder = document.createElement("div");
                holder.innerHTML = "<script>globalThis.__cloneScriptRuns++<\/script>";
                document.body.appendChild(holder.firstChild.cloneNode(true));

                const fragment = document.createDocumentFragment();
                fragment.appendChild(dynamic.cloneNode(true));
                document.body.appendChild(fragment.cloneNode(true));
                return globalThis.__cloneScriptRuns;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(1.0));
    }

    #[test]
    fn contextual_fragment_and_document_write_keep_executable_script_policy() {
        let mut rt = setup_runtime("<html><head></head><body><div id=context></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__executableFragmentRuns = [];
                const range = document.createRange();
                range.selectNode(document.getElementById("context"));
                const fragment = range.createContextualFragment(
                    "<template><script>globalThis.__executableFragmentRuns.push('template')<\/script></template>" +
                    "<script>globalThis.__executableFragmentRuns.push('range')<\/script>"
                );
                document.body.appendChild(fragment);
                document.write(
                    "<script>globalThis.__executableFragmentRuns.push('write')<\/script>"
                );
                return globalThis.__executableFragmentRuns;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["range", "write"]));
    }

    // One stream per document. The tokenizer carries its state across the calls.
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-write
    #[test]
    fn document_write_joins_an_element_split_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<di');
                document.write('v id="split">');
                document.write('content</div>');
                const el = document.getElementById('split');
                return el ? el.textContent : null;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("content"));
    }

    #[test]
    fn document_write_joins_a_tag_name_split_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<spa');
                document.write('n id="half">x</span>');
                const el = document.getElementById('half');
                return el ? el.tagName : null;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("SPAN"));
    }

    // The shape the UI5 cachebuster writes: "<script", one per attribute, then ">".
    #[test]
    fn document_write_runs_a_script_split_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__splitScriptRan = false;
                document.write('<scr' + 'ipt');
                document.write(' id="split-script"');
                document.write('>');
                document.write('globalThis.__splitScriptRan = true;');
                document.write('<\/scr' + 'ipt>');
                return [!!document.getElementById('split-script'), globalThis.__splitScriptRan];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true]));
    }

    // A script in the <head> inserts behind itself, so that what it writes runs before what
    // the parser saw after it.
    #[test]
    fn document_write_inserts_at_the_writing_scripts_position() {
        let mut rt = setup_runtime(
            r#"<html><head><script id="writer"></script></head><body><p id="existing">x</p></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                // What the production path sets while a script runs; bootstrap.js
                // assigns __currentScriptNid around every script it prepares.
                globalThis.__currentScriptNid = document.getElementById('writer')._nid;
                document.write('<span id="written"></span>');
                return JSON.stringify({
                  head: Array.from(document.head.children).map(e => e.id || e.tagName),
                  body: Array.from(document.body.children).map(e => e.id || e.tagName),
                });
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(r#"{"head":["writer","written"],"body":["existing"]}"#)
        );
    }

    // Holding back until the close would lose everything written after it. It belongs inside.
    #[test]
    fn document_write_shows_an_element_that_is_never_closed() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<div id="unclosed">hello');
                const el = document.getElementById('unclosed');
                return el ? el.textContent : null;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("hello"));
    }

    #[test]
    fn document_write_grows_an_open_element_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<div id="wrap">');
                document.write('<span id="inner">y</span>');
                const inner = document.getElementById('inner');
                return JSON.stringify({
                  wrap: !!document.getElementById('wrap'),
                  inner: !!inner,
                  nested: !!(inner && inner.parentElement && inner.parentElement.id === 'wrap'),
                });
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(r#"{"wrap":true,"inner":true,"nested":true}"#)
        );
    }

    // Writing goes through the same insertion steps as any other insertion.
    #[test]
    fn document_write_reports_to_mutation_observers() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__seen = [];
                const observer = new MutationObserver((records) => {
                  for (const record of records) {
                    for (const node of record.addedNodes) globalThis.__seen.push(node.nodeName);
                  }
                });
                observer.observe(document.body, { childList: true });
                document.write('<span id="watched">z</span>');
                observer.takeRecords().forEach((record) => {
                  for (const node of record.addedNodes) globalThis.__seen.push(node.nodeName);
                });
                return globalThis.__seen.join(',');
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("SPAN"));
    }

    // before(), after() and replaceWith() all go through parent.insertBefore. replaceChild
    // also goes there in the fragment branch. AGENTS.md requires whoever touches insertBefore
    // to check them: the order of reference node versus parent nid is easy to break. The test
    // also pins that every insertion is reported exactly once, not twice.
    #[test]
    fn child_node_methods_place_nodes_and_report_once() {
        let mut rt = setup_runtime(r#"<html><body><p id="a"></p><p id="b"></p></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                const ids = () => Array.from(document.body.children).map((e) => e.id).join(',');
                const make = (id) => { const e = document.createElement('span'); e.id = id; return e; };
                const observer = new MutationObserver(() => {});
                observer.observe(document.body, { childList: true });
                const steps = {};

                document.getElementById('b').before(make('x'));
                steps.before = ids();
                document.getElementById('b').after(make('y'));
                steps.after = ids();
                document.getElementById('y').replaceWith(make('z'));
                steps.replaceWith = ids();
                document.body.replaceChild(make('w'), document.getElementById('z'));
                steps.replaceChild = ids();

                const added = observer.takeRecords()
                  .flatMap((record) => Array.from(record.addedNodes).map((n) => n.id));
                observer.disconnect();
                steps.added = added.join(',');
                return JSON.stringify(steps);
                "#,
            )
            .unwrap();
        let steps: serde_json::Value =
            serde_json::from_str(result.as_str().unwrap()).expect("steps json");
        assert_eq!(steps["before"], "a,x,b");
        assert_eq!(steps["after"], "a,x,b,y");
        assert_eq!(steps["replaceWith"], "a,x,b,z");
        assert_eq!(steps["replaceChild"], "a,x,b,w");
        // Every inserted node exactly once, in the order of insertion.
        assert_eq!(steps["added"], "x,y,z,w");
    }

    // insertBefore reported no mutation at all, appendChild did.
    #[test]
    fn insert_before_reports_to_mutation_observers() {
        let mut rt = setup_runtime("<html><body><p id=\"ref\"></p></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                const observer = new MutationObserver(() => {});
                observer.observe(document.body, { childList: true });
                document.body.insertBefore(
                  document.createElement('span'),
                  document.getElementById('ref'),
                );
                const seen = observer.takeRecords()
                  .flatMap((record) => Array.from(record.addedNodes).map((n) => n.nodeName));
                observer.disconnect();
                return seen.join(',');
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("SPAN"));
    }

    #[test]
    fn document_write_registers_window_named_access() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<img name="namedImage" src="x.png">');
                return typeof window.namedImage;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("object"));
    }

    #[test]
    fn document_write_keeps_call_order_at_the_insertion_point() {
        let mut rt = setup_runtime(
            r#"<html><head><script id="writer"></script></head><body></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__currentScriptNid = document.getElementById('writer')._nid;
                document.write('<span id="one"></span>');
                document.write('<span id="two"></span>');
                return Array.from(document.head.children).map(e => e.id).join(',');
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("writer,one,two"));
    }
}
