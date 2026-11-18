use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use deno_core::error::ModuleLoaderError;
use deno_core::futures::future::{FutureExt, LocalBoxFuture, WeakShared};
use deno_core::ModuleLoadOptions;
use deno_core::ModuleLoadReferrer;
use deno_core::ModuleLoadResponse;
use deno_core::ModuleLoader;
use deno_core::ModuleSource;
use deno_core::ModuleSourceCode;
use deno_core::ModuleSpecifier;

use deno_error::JsErrorBox;

use crate::import_map::ImportMap;
use crate::ops::{ObscuraState, JsNetworkEvent, CachedNetworkResponseBody, StoredNetworkResponseBody, FetchNetworkGuard, record_network_completion};

/// Observable network activity for ES-module graphs.
///
/// deno_core keeps dynamic-import state inside its private module map. The
/// browser lifecycle still needs to distinguish a genuinely idle page from a
/// graph whose fetch future is being advanced in short event-loop slices. A
/// loader-owned counter provides that signal without treating unrelated
/// fetch/XHR analytics as render-blocking work.
#[derive(Debug, Default)]
pub(crate) struct ModuleLoadActivity {
    pending: std::sync::atomic::AtomicUsize,
    last_activity: std::sync::Mutex<Option<std::time::Instant>>,
}

impl ModuleLoadActivity {
    fn begin(self: &Arc<Self>) -> ModuleLoadGuard {
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::time::Instant::now());
        ModuleLoadGuard(self.clone())
    }

    pub(crate) fn is_pending_or_recent(&self, grace: std::time::Duration) -> bool {
        if self.pending.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            return true;
        }
        self.last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|last| last.elapsed() <= grace)
    }
}

struct ModuleLoadGuard(Arc<ModuleLoadActivity>);

impl Drop for ModuleLoadGuard {
    fn drop(&mut self) {
        let previous = self
            .0
            .pending
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        debug_assert!(previous > 0, "module load activity counter underflow");
        *self
            .0
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(std::time::Instant::now());
    }
}

pub struct ObscuraModuleLoader {
    pub base_url: String,
    /// Proxy URL threaded through to every dynamic ES-module fetch (#139).
    /// `None` keeps the pre-#139 direct-connection behaviour for callers
    /// that haven't been updated.
    pub proxy_url: Option<String>,
    /// The owning page's network context. Production runtimes always install
    /// this so every module in a graph uses the same cookie jar, configured
    /// identity, redirect/security policy, interception, and callbacks as the
    /// entry module. Directly-constructed standalone loaders remain supported.
    page_state: Option<Weak<RefCell<ObscuraState>>>,
    /// Directly-constructed loaders still use Obscura's network policy and
    /// connection pool; they simply have an isolated cookie jar.
    standalone_client: Option<Arc<obscura_net::ObscuraHttpClient>>,
    import_map: Rc<RefCell<ImportMap>>,
    activity: Arc<ModuleLoadActivity>,
    /// Canonical and requested specifiers fetched into deno_core's module map.
    /// The runtime uses a cursor into this append-only list to associate a
    /// prepared root with the dependencies that its successful evaluation also
    /// evaluates.
    loaded_specifiers: Rc<RefCell<Vec<String>>>,
    /// Module fetches that some graph load still awaits, keyed by request URL.
    /// deno_core runs each `import()` and module script as its own graph load
    /// and only dedups a module once it is registered, so concurrent graphs
    /// that share a dependency would each fetch it. A browser module map has a
    /// "fetching" state that makes later requests wait for the first one.
    /// Entries are weak: the waiting loads own the fetch, so a fetch whose
    /// loads were all cancelled is dropped (and reported as failed) as before.
    /// Every waiting load hands the source to deno_core, which registers the
    /// module before polling anything else, so once the last one has taken it
    /// later loads find the module in deno_core's map. A failed fetch is not
    /// cached: a later request fetches it again.
    inflight: RefCell<InflightModules>,
}

/// A fetched module body, cheap to clone into each waiting graph load.
#[derive(Clone)]
struct FetchedModule {
    found: ModuleSpecifier,
    code: Arc<str>,
}

type ModuleFetch = LocalBoxFuture<'static, Result<FetchedModule, String>>;

#[derive(Default)]
struct InflightModules {
    fetches: HashMap<String, WeakShared<ModuleFetch>>,
    cleanup_at: usize,
}

impl ObscuraModuleLoader {
    pub fn new(base_url: &str) -> Self {
        Self::with_proxy(base_url, None)
    }

    pub fn with_proxy(base_url: &str, proxy_url: Option<String>) -> Self {
        let import_map = Rc::new(RefCell::new(ImportMap::default()));
        Self::with_proxy_and_import_map(base_url, proxy_url, import_map)
    }

    fn with_proxy_and_import_map(
        base_url: &str,
        proxy_url: Option<String>,
        import_map: Rc<RefCell<ImportMap>>,
    ) -> Self {
        let standalone_client = Arc::new(obscura_net::ObscuraHttpClient::with_options(
            Arc::new(obscura_net::CookieJar::new()),
            proxy_url.as_deref(),
        ));
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            page_state: None,
            standalone_client: Some(standalone_client),
            import_map,
            activity: Arc::new(ModuleLoadActivity::default()),
            loaded_specifiers: Rc::new(RefCell::new(Vec::new())),
            inflight: RefCell::new(InflightModules::default()),
        }
    }

    pub(crate) fn with_page_state(
        base_url: &str,
        proxy_url: Option<String>,
        page_state: &Rc<RefCell<ObscuraState>>,
        import_map: Rc<RefCell<ImportMap>>,
    ) -> Self {
        ObscuraModuleLoader {
            base_url: base_url.to_string(),
            proxy_url,
            page_state: Some(Rc::downgrade(page_state)),
            standalone_client: None,
            import_map,
            activity: Arc::new(ModuleLoadActivity::default()),
            loaded_specifiers: Rc::new(RefCell::new(Vec::new())),
            inflight: RefCell::new(InflightModules::default()),
        }
    }

    pub(crate) fn activity(&self) -> Arc<ModuleLoadActivity> {
        self.activity.clone()
    }

    pub(crate) fn loaded_specifiers(&self) -> Rc<RefCell<Vec<String>>> {
        self.loaded_specifiers.clone()
    }

    /// The page's network context for a new module fetch.
    fn page_network(&self) -> PageNetwork {
        match self.page_state.as_ref() {
            Some(weak) => (|| {
                let state = weak
                    .upgrade()
                    .ok_or_else(|| "Module loader page state was dropped".to_string())?;
                let state = state
                    .try_borrow()
                    .map_err(|_| "Module loader page state is already borrowed".to_string())?;
                let client = state
                    .http_client
                    .clone()
                    .ok_or_else(|| "No http_client wired to module loader".to_string())?;
                // Fork: in stealth mode an ES module must be fetched over the
                // same transport as the document. Upstream sends it through the
                // plain reqwest client, so a `type="module"` script arrives with
                // a different TLS fingerprint and none of the browser identity
                // headers, while the HTML that referenced it came over wreq.
                // That cross-transport mismatch is trivially detectable.
                #[cfg(feature = "stealth")]
                let stealth = state.stealth_client.clone();
                #[cfg(not(feature = "stealth"))]
                let stealth: Option<std::sync::Arc<()>> = None;
                Ok((client, stealth, state.callbacks.clone()))
            })(),
            None => self
                .standalone_client
                .clone()
                .map(|client| {
                    #[cfg(feature = "stealth")]
                    let stealth = None;
                    #[cfg(not(feature = "stealth"))]
                    let stealth: Option<std::sync::Arc<()>> = None;
                    (client, stealth, None)
                })
                .ok_or_else(|| "No network context wired to module loader".to_string()),
        }
    }
}

fn io_err(msg: String) -> ModuleLoaderError {
    JsErrorBox::from_err(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

impl ModuleLoader for ObscuraModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: deno_core::ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        // deno_core represents the root passed to load_side_es_module with a
        // synthetic "." referrer. A browser resolves <script type=module src>
        // as a resource URL before it starts a graph; the document import map
        // must not remap that root URL.
        if referrer == "." {
            return deno_core::resolve_import(specifier, &self.base_url)
                .map_err(|error| io_err(error.to_string()));
        }

        let base = if referrer.is_empty()
            || referrer.starts_with('<')
            || referrer == "about:blank"
        {
            &self.base_url
        } else {
            referrer
        };

        let base = ModuleSpecifier::parse(base)
            .map_err(|e| io_err(format!("Invalid module referrer {}: {}", base, e)))?;
        self.import_map
            .try_borrow_mut()
            .map_err(|_| io_err("Import map is already borrowed".to_string()))?
            .resolve(specifier, &base)
            .map_err(io_err)
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
        options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let url = module_specifier.to_string();
        // Module-graph CORS and same-origin credentials are relative to the
        // owning document, not to the importing module. The importer remains
        // the HTTP referrer for a dependency; keeping these URLs distinct
        // prevents a cross-origin parent module from gaining CDN cookies when
        // it imports a sibling on that CDN.
        let document_url = ModuleSpecifier::parse(&self.base_url)
            .unwrap_or_else(|_| module_specifier.clone());
        let referrer = maybe_referrer
            .map(|referrer| referrer.specifier.clone())
            .unwrap_or_else(|| document_url.clone());
        // Capture the loader's proxy here so the async closure below owns a
        // plain Option<String> rather than borrowing &self across an `await`.
        let proxy_url = self.proxy_url.clone();
        let activity = self.activity.clone();
        let loaded_specifiers = self.loaded_specifiers.clone();
        loaded_specifiers.borrow_mut().push(url.clone());
        // Register before returning the future. The lifecycle can inspect the
        // runtime between deno_core accepting the load and first polling it.
        // Keeping the guard inside the future makes cancellation/navigation
        // decrement the count through Drop as well as success and failure.
        let is_dyn_import = options.is_dynamic_import;
        let activity_guard = is_dyn_import.then(|| activity.begin());
        let page_state = self.page_state.clone();

        let requested_specifier = module_specifier.clone();
        let fetch = {
            let mut inflight = self.inflight.borrow_mut();
            // Sweeping on every load makes wide, pending module graphs quadratic.
            if inflight.fetches.len() >= inflight.cleanup_at {
                inflight.fetches.retain(|_, fetch| fetch.upgrade().is_some());
                let cleanup_at = inflight.fetches.len().saturating_mul(2).max(32);
                inflight.fetches.shrink_to(cleanup_at);
                inflight.cleanup_at = cleanup_at;
            }
            match inflight.fetches.get(&url).and_then(WeakShared::upgrade) {
                Some(fetch) => fetch,
                None => {
                    let page_network = self.page_network();
                    let fetch = fetch_module(
                        url.clone(), proxy_url, document_url, referrer, page_state, page_network,
                    )
                    .map(|result| result.map_err(|error| error.to_string()))
                    .boxed_local()
                    .shared();
                    if let Some(weak) = fetch.downgrade() {
                        inflight.fetches.insert(url, weak);
                    }
                    fetch
                }
            }
        };

        ModuleLoadResponse::Async(Pin::from(Box::new(async move {
            // deno_core propagates `is_dynamic_import` to every dependency edge in
            // the recursive graph, so this excludes parser-discovered/static
            // graphs without losing descendant fetches of a lazy graph.
            let _activity_guard = activity_guard;
            let fetched = fetch.await.map_err(io_err)?;
            if fetched.found.as_str() != requested_specifier.as_str() {
                loaded_specifiers.borrow_mut().push(fetched.found.to_string());
            }
            Ok(ModuleSource::new_with_redirect(
                deno_core::ModuleType::JavaScript,
                ModuleSourceCode::String(fetched.code.into()),
                &requested_specifier,
                &fetched.found,
                None,
            ))
        })))
    }
}

#[cfg(feature = "stealth")]
type StealthClient = Option<Arc<obscura_net::StealthHttpClient>>;
#[cfg(not(feature = "stealth"))]
type StealthClient = Option<Arc<()>>;

type PageNetwork = Result<
    (
        Arc<obscura_net::ObscuraHttpClient>,
        StealthClient,
        Option<Arc<obscura_net::CallbackRegistry>>,
    ),
    String,
>;

async fn fetch_module(
    url: String,
    proxy_url: Option<String>,
    document_url: ModuleSpecifier,
    referrer: ModuleSpecifier,
    page_state: Option<Weak<RefCell<ObscuraState>>>,
    page_network: PageNetwork,
) -> Result<FetchedModule, ModuleLoaderError> {
    tracing::debug!(
        "Loading ES module: {} (proxy: {})",
        url,
        proxy_url.as_deref().unwrap_or("direct")
    );

    match page_network {
        Ok((client, stealth, callbacks)) => {
            let requested = ModuleSpecifier::parse(&url)
                .map_err(|e| io_err(format!("Invalid module URL {}: {}", url, e)))?;
            static NEXT_MODULE_REQUEST_ID: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(1);
            let request_id = format!("module-{}", NEXT_MODULE_REQUEST_ID.fetch_add(
                1, std::sync::atomic::Ordering::Relaxed,
            ));
            // Keep failed and cancelled graph fetches visible to the
            // same Network listeners as ordinary fetch/XHR failures.
            let mut network_guard = page_state.as_ref().map(|state| FetchNetworkGuard {
                state: state.clone(), cancel: None,
                event: Some(JsNetworkEvent {
                    request_id: request_id.clone(), intercepted: false,
                    url: url.clone(), method: "GET".to_string(),
                    resource_type: "Script".to_string(), status: 0,
                    response_headers: Default::default(), body_size: 0,
                    timestamp: 0.0, error_text: None,
                }),
            });
            let request =
                obscura_net::ResourceRequest::module_script(&document_url, &referrer);
            #[cfg(feature = "stealth")]
            let resp = match stealth {
                Some(stealth) => stealth
                    .fetch_resource_with_callbacks(
                        &requested,
                        request,
                        callbacks.as_deref(),
                    )
                    .await,
                None => {
                    client
                        .fetch_resource_with_callbacks(
                            &requested,
                            request,
                            callbacks.as_deref(),
                        )
                        .await
                }
            }
            .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?;
            #[cfg(not(feature = "stealth"))]
            let resp = {
                let _ = &stealth;
                client
                    .fetch_resource_with_callbacks(
                        &requested,
                        request,
                        callbacks.as_deref(),
                    )
                    .await
                    .map_err(|e| io_err(format!("Failed to fetch module {}: {}", url, e)))?
            };
            // A response (including the blocked-tracker sentinel) is
            // not a transport failure, even if evaluation later fails.
            if let Some(guard) = &mut network_guard { guard.event = None; }
            // A module response belongs to the owning page regardless
            // of whether graph preparation or evaluation later fails.
            // Status 0 is the transport's blocked-tracker sentinel.
            if resp.status != 0 {
                if let Some(state) = page_state.as_ref().and_then(Weak::upgrade) {
                    let mut state = state.try_borrow_mut().map_err(|_| {
                        io_err("Module loader page state is already borrowed".to_string())
                    })?;
                    record_network_completion(&mut state, JsNetworkEvent {
                        request_id, intercepted: false,
                        url: resp.url.to_string(), method: "GET".to_string(),
                        resource_type: "Script".to_string(), status: resp.status,
                        response_headers: resp.headers.clone(), body_size: resp.body.len(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default().as_secs_f64(),
                        error_text: None,
                    }, |_| CachedNetworkResponseBody::Encoded(match std::str::from_utf8(&resp.body) {
                        Ok(body) => StoredNetworkResponseBody {
                            body: body.to_string(), base64_encoded: false,
                        },
                        Err(_) => StoredNetworkResponseBody {
                            body: BASE64.encode(&resp.body), base64_encoded: true,
                        },
                    }));
                }
            }
            if !(200..=299).contains(&resp.status) {
                return Err(io_err(format!(
                    "Module {} returned HTTP {}",
                    url, resp.status
                )));
            }
            let found = ModuleSpecifier::parse(resp.url.as_str()).map_err(|e| {
                io_err(format!("Invalid final module URL {}: {}", resp.url, e))
            })?;
            let code = obscura_net::decode_non_html(&resp.body, resp.content_type());
            Ok(FetchedModule { found, code: Arc::from(code) })
        }
        Err(error) => Err(io_err(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflight_registry_stays_bounded_and_reuses_live_fetches() {
        let loader = ObscuraModuleLoader::new("https://module-cache.test/");
        let queue = |path: &str| {
            let url = ModuleSpecifier::parse(&format!("https://module-cache.test/{path}")).unwrap();
            loader.load(&url, None, ModuleLoadOptions {
                is_dynamic_import: true,
                is_synchronous: false,
                requested_module_type: deno_core::RequestedModuleType::None,
            })
        };
        // Keep the network futures unpolled, as happens when a wide graph
        // queues its dependencies before the event loop can advance them.
        let live = (0..128).map(|index| queue(&format!("live-{index}.js"))).collect::<Vec<_>>();
        let first_url = "https://module-cache.test/live-0.js";
        let first = loader.inflight.borrow().fetches[first_url].upgrade().unwrap();

        for index in 0..2048 {
            drop(queue(&format!("canceled-{index}.js")));
        }
        let repeated = queue("live-0.js");
        {
            let inflight = loader.inflight.borrow();
            let again = inflight.fetches[first_url].upgrade().unwrap();
            assert!(first.ptr_eq(&again), "cleanup must preserve a live shared fetch");
            assert!(inflight.fetches.len() <= 2 * live.len() + 32, "canceled loads must not grow the registry indefinitely");
        }
        assert_eq!(loader.activity.pending.load(std::sync::atomic::Ordering::Relaxed), live.len() + 1);

        drop(first);
        drop(repeated);
        drop(live);
        for index in 0..2048 {
            drop(queue(&format!("later-{index}.js")));
        }
        let inflight = loader.inflight.borrow();
        assert!(inflight.fetches.len() <= 64, "an idle registry must return to a small batch");
        assert!(inflight.fetches.capacity() <= 128, "later sweeps must not scan a large stale table");
        assert!(inflight.fetches.values().all(|fetch| fetch.upgrade().is_none()), "registry entries must not keep canceled fetches alive");
        assert_eq!(loader.activity.pending.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
