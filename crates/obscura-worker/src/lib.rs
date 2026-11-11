//! Independent runtime worker and session lifecycle model for Obscura Next.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::Mutex as TokioMutex;

use obscura_browser::{BrowserContext, Page, WaitUntil};
use obscura_isolation::{IsolationPolicy, IsolationProfile};
use obscura_telemetry::global_metrics;

#[derive(Error, Debug)]
pub enum WorkerError {
    #[error("Session '{0}' not found")]
    SessionNotFound(String),

    #[error("Session '{0}' already exists")]
    SessionAlreadyExists(String),

    #[error("Session '{0}' is in invalid state '{1:?}' for this operation")]
    InvalidState(String, SessionState),

    #[error("Isolation error: {0}")]
    Isolation(#[from] obscura_isolation::IsolationError),

    #[error("Browser execution error: {0}")]
    Browser(String),

    #[error("Operation timed out after {0:?}")]
    Timeout(Duration),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),
}

/// Lifecycle states for an isolated browser session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Allocated,
    Initializing,
    Ready,
    Busy,
    Suspended,
    Draining,
    Terminated,
}

impl SessionState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Ready | Self::Busy)
    }

    pub fn can_execute(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Configuration parameters for an isolated session.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SessionConfig {
    pub proxy_url: Option<String>,
    pub stealth: bool,
    pub user_agent: Option<String>,
    pub obey_robots: bool,
    pub allow_file_access: bool,
    pub allow_private_network: bool,
    pub profile: Option<IsolationProfile>,
}

/// Managed session holding an independent browser page, context, and isolate.
pub struct ManagedSession {
    pub id: String,
    pub state: SessionState,
    pub config: SessionConfig,
    pub isolation: IsolationPolicy,
    pub context: Arc<BrowserContext>,
    pub page: Page,
    pub created_at: Instant,
    pub last_active_at: Instant,
    pub requests_executed: usize,
}

impl ManagedSession {
    pub fn new(id: String, config: SessionConfig) -> Self {
        let isolation =
            IsolationPolicy::for_profile(config.profile.unwrap_or(IsolationProfile::Standard));

        let mut context = BrowserContext::with_full_options(
            format!("ctx-{id}"),
            config.proxy_url.clone(),
            config.stealth,
            config.user_agent.clone(),
        );
        context.obey_robots = config.obey_robots;
        context.set_allow_file_access(config.allow_file_access);
        context.allow_private_network = config.allow_private_network;

        let context = Arc::new(context);
        let page = Page::new(format!("page-{id}"), context.clone());

        Self {
            id,
            state: SessionState::Ready,
            config,
            isolation,
            context,
            page,
            created_at: Instant::now(),
            last_active_at: Instant::now(),
            requests_executed: 0,
        }
    }

    pub fn touch(&mut self) {
        self.last_active_at = Instant::now();
        self.requests_executed += 1;
    }
}

/// Thread-safe manager for all active sessions in this worker with fine-grained per-session locking.
#[derive(Clone)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Arc<TokioMutex<ManagedSession>>>>>,
    started_at: Instant,
    active_requests: Arc<AtomicUsize>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            started_at: Instant::now(),
            active_requests: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn create_session(&self, id: String, config: SessionConfig) -> Result<(), WorkerError> {
        let mut guard = self.sessions.write();
        if guard.contains_key(&id) {
            return Err(WorkerError::SessionAlreadyExists(id));
        }

        let session = ManagedSession::new(id.clone(), config);
        guard.insert(id, Arc::new(TokioMutex::new(session)));

        // Update telemetry
        let metrics = global_metrics();
        if let Some(gauge) = metrics.gauge("obscura_sessions_active") {
            gauge.set(guard.len() as i64);
        }
        if let Some(counter) = metrics.counter("obscura_sessions_total") {
            counter.inc();
        }

        Ok(())
    }

    pub fn destroy_session(&self, id: &str) -> Result<(), WorkerError> {
        let mut guard = self.sessions.write();
        if guard.remove(id).is_some() {
            let metrics = global_metrics();
            if let Some(gauge) = metrics.gauge("obscura_sessions_active") {
                gauge.set(guard.len() as i64);
            }
            Ok(())
        } else {
            Err(WorkerError::SessionNotFound(id.to_string()))
        }
    }

    /// Retrieves an individual session handle without holding the global session map lock.
    pub fn get_session(&self, id: &str) -> Option<Arc<TokioMutex<ManagedSession>>> {
        self.sessions.read().get(id).cloned()
    }

    pub fn active_count(&self) -> usize {
        self.sessions.read().len()
    }

    pub fn inc_active_requests(&self) {
        let n = self.active_requests.fetch_add(1, Ordering::Relaxed) + 1;
        let metrics = global_metrics();
        if let Some(g) = metrics.gauge("obscura_worker_active_requests") {
            g.set(n as i64);
        }
    }

    pub fn dec_active_requests(&self) {
        let prev = self.active_requests.load(Ordering::Relaxed);
        let n = if prev > 0 { prev - 1 } else { 0 };
        self.active_requests.store(n, Ordering::Relaxed);
        let metrics = global_metrics();
        if let Some(g) = metrics.gauge("obscura_worker_active_requests") {
            g.set(n as i64);
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.sessions.read().contains_key(id)
    }

    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.read().keys().cloned().collect()
    }

    pub async fn evict_idle(&self, max_idle: Duration) -> Vec<String> {
        let session_arcs: Vec<(String, Arc<TokioMutex<ManagedSession>>)> = {
            let guard = self.sessions.read();
            guard
                .iter()
                .map(|(id, arc)| (id.clone(), arc.clone()))
                .collect()
        };

        let now = Instant::now();
        let mut to_evict = Vec::new();

        for (id, arc) in session_arcs {
            let session = arc.lock().await;
            if now.duration_since(session.last_active_at) > max_idle {
                to_evict.push(id);
            }
        }

        if !to_evict.is_empty() {
            let mut guard = self.sessions.write();
            for id in &to_evict {
                guard.remove(id);
            }
            let metrics = global_metrics();
            if let Some(gauge) = metrics.gauge("obscura_sessions_active") {
                gauge.set(guard.len() as i64);
            }
        }

        to_evict
    }
}

// ---------------------------------------------------------------------------
// Worker IPC Protocol Types (Canonical v0.2.0)
// ---------------------------------------------------------------------------

pub fn default_protocol_version() -> String {
    "1.0".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkerRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "default_protocol_version")]
    pub version: String,
    #[serde(flatten)]
    pub command: WorkerCommand,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "cmd")]
pub enum WorkerCommand {
    #[serde(rename = "ping")]
    Ping,

    #[serde(rename = "health")]
    Health,

    #[serde(rename = "shutdown")]
    Shutdown,

    #[serde(rename = "session_create", alias = "create_session")]
    SessionCreate {
        session_id: String,
        #[serde(default)]
        config: SessionConfig,
    },

    #[serde(rename = "session_destroy", alias = "destroy_session")]
    SessionDestroy { session_id: String },

    #[serde(rename = "session_status")]
    SessionStatus { session_id: String },

    #[serde(rename = "navigate")]
    Navigate {
        session_id: String,
        url: String,
        #[serde(default, rename = "waitUntil")]
        wait_until: NavigationWaitUntil,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },

    #[serde(rename = "evaluate")]
    Evaluate {
        session_id: String,
        expression: String,
        #[serde(default = "default_true")]
        await_promise: bool,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },

    #[serde(rename = "snapshot")]
    Snapshot { session_id: String },

    #[serde(rename = "click")]
    Click {
        session_id: String,
        selector: String,
    },

    #[serde(rename = "fill")]
    Fill {
        session_id: String,
        selector: String,
        value: String,
    },

    #[serde(rename = "type")]
    Type {
        session_id: String,
        selector: String,
        text: String,
    },

    #[serde(rename = "screenshot")]
    Screenshot { session_id: String },

    #[serde(rename = "title")]
    Title { session_id: String },

    #[serde(rename = "dump")]
    Dump {
        session_id: String,
        #[serde(default)]
        format: DumpFormat,
    },
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NavigationWaitUntil {
    #[default]
    Load,
    DomContentLoaded,
    NetworkIdle0,
    NetworkIdle2,
}

impl From<NavigationWaitUntil> for WaitUntil {
    fn from(value: NavigationWaitUntil) -> Self {
        match value {
            NavigationWaitUntil::Load => Self::Load,
            NavigationWaitUntil::DomContentLoaded => Self::DomContentLoaded,
            NavigationWaitUntil::NetworkIdle0 => Self::NetworkIdle0,
            NavigationWaitUntil::NetworkIdle2 => Self::NetworkIdle2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DumpFormat {
    #[default]
    Html,
    Text,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ProtocolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(
        code: impl Into<String>,
        message: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Some(details),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default = "default_protocol_version")]
    pub version: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

impl WorkerResponse {
    pub fn success(id: Option<String>, result: serde_json::Value) -> Self {
        Self {
            id,
            version: "1.0".to_string(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<String>, code: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            id,
            version: "1.0".to_string(),
            ok: false,
            result: None,
            error: Some(ProtocolError::new(code, msg)),
        }
    }

    pub fn error_with_details(
        id: Option<String>,
        code: impl Into<String>,
        msg: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        Self {
            id,
            version: "1.0".to_string(),
            ok: false,
            result: None,
            error: Some(ProtocolError::with_details(code, msg, details)),
        }
    }
}

/// Dispatches a protocol command against the session manager with fine-grained per-session concurrency.
pub async fn dispatch_worker_command(
    manager: &SessionManager,
    request: WorkerRequest,
) -> WorkerResponse {
    let req_id = request.id;

    // Track request metric
    let metrics = global_metrics();
    if let Some(counter) = metrics.counter("obscura_worker_requests_total") {
        counter.inc();
    }

    let _start = Instant::now();
    let result = match request.command {
        WorkerCommand::Ping => WorkerResponse::success(req_id, serde_json::json!("pong")),

        WorkerCommand::Health => {
            let active = manager.active_count();
            let uptime = manager.uptime().as_secs_f64();
            WorkerResponse::success(
                req_id,
                serde_json::json!({
                    "status": "healthy",
                    "active_sessions": active,
                    "pid": std::process::id(),
                    "uptime_seconds": uptime,
                }),
            )
        }

        WorkerCommand::Shutdown => {
            WorkerResponse::success(req_id, serde_json::json!({ "status": "shutting_down" }))
        }

        WorkerCommand::SessionCreate { session_id, config } => {
            match manager.create_session(session_id.clone(), config) {
                Ok(()) => WorkerResponse::success(
                    req_id,
                    serde_json::json!({ "session_id": session_id, "status": "created" }),
                ),
                Err(WorkerError::SessionAlreadyExists(id)) => WorkerResponse::error(
                    req_id,
                    "SESSION_ALREADY_EXISTS",
                    format!("Session '{id}' already exists"),
                ),
                Err(e) => {
                    if let Some(counter) = metrics.counter("obscura_worker_request_errors_total") {
                        counter.inc();
                    }
                    WorkerResponse::error(req_id, "EXECUTION_ERROR", e.to_string())
                }
            }
        }

        WorkerCommand::SessionDestroy { session_id } => {
            match manager.destroy_session(&session_id) {
                Ok(()) => WorkerResponse::success(
                    req_id,
                    serde_json::json!({ "session_id": session_id, "status": "destroyed" }),
                ),
                Err(WorkerError::SessionNotFound(id)) => WorkerResponse::error(
                    req_id,
                    "SESSION_NOT_FOUND",
                    format!("Session '{id}' not found"),
                ),
                Err(e) => {
                    if let Some(counter) = metrics.counter("obscura_worker_request_errors_total") {
                        counter.inc();
                    }
                    WorkerResponse::error(req_id, "EXECUTION_ERROR", e.to_string())
                }
            }
        }

        WorkerCommand::SessionStatus { session_id } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };
            let session = session_arc.lock().await;
            WorkerResponse::success(
                req_id,
                serde_json::json!({
                    "session_id": session_id,
                    "state": session.state,
                    "requests_executed": session.requests_executed,
                    "age_seconds": session.created_at.elapsed().as_secs_f64(),
                    "idle_seconds": session.last_active_at.elapsed().as_secs_f64(),
                }),
            )
        }

        WorkerCommand::Navigate {
            session_id,
            url,
            wait_until,
            timeout_ms: _,
        } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            // Acquire per-session lock (does not block other sessions!)
            let mut session = session_arc.lock().await;

            // Enforce isolation policy before navigation
            if let Err(err) = session.isolation.validate_url(&url) {
                if let Some(counter) = metrics.counter("obscura_security_blocks_total") {
                    counter.inc();
                }
                return WorkerResponse::error(
                    req_id,
                    "SECURITY_BLOCK",
                    format!("Security block: {err}"),
                );
            }

            session.state = SessionState::Busy;
            session.touch();

            let t0 = Instant::now();
            let nav_res = session
                .page
                .navigate_with_wait(&url, wait_until.into())
                .await;

            let elapsed = t0.elapsed();
            if let Some(hist) = metrics.histogram("obscura_navigation_duration_seconds") {
                hist.observe_duration(elapsed);
            }

            session.state = SessionState::Ready;

            match nav_res {
                Ok(()) => {
                    let title = session.page.title.clone();
                    let current_url = session.page.url_string();
                    WorkerResponse::success(
                        req_id,
                        serde_json::json!({
                            "title": title,
                            "url": current_url,
                            "status": 200,
                            "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
                        }),
                    )
                }
                Err(e) => {
                    if let Some(counter) = metrics.counter("obscura_worker_request_errors_total") {
                        counter.inc();
                    }
                    WorkerResponse::error(req_id, "BROWSER_ERROR", e.to_string())
                }
            }
        }

        WorkerCommand::Evaluate {
            session_id,
            expression,
            await_promise,
            timeout_ms,
        } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            let mut session = session_arc.lock().await;

            session.state = SessionState::Busy;
            session.touch();

            let timeout = timeout_ms.unwrap_or(30_000);
            let t0 = Instant::now();
            let eval_res = session
                .page
                .evaluate_for_cdp_with_timeout(&expression, await_promise, true, timeout)
                .await;

            let elapsed = t0.elapsed();
            if let Some(hist) = metrics.histogram("obscura_eval_duration_seconds") {
                hist.observe_duration(elapsed);
            }

            session.state = SessionState::Ready;

            match eval_res {
                Ok(info) => {
                    let val = match info.value {
                        Some(v) => v,
                        None => serde_json::Value::String(info.description),
                    };
                    WorkerResponse::success(req_id, val)
                }
                Err(e) => {
                    if let Some(counter) = metrics.counter("obscura_worker_request_errors_total") {
                        counter.inc();
                    }
                    WorkerResponse::error(req_id, "EVAL_ERROR", e.to_string())
                }
            }
        }

        WorkerCommand::Snapshot { session_id } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            let mut session = session_arc.lock().await;
            session.touch();

            if let Some(ref dom) = session.page.dom {
                let distilled = obscura_agent::SemanticDistiller::distill(dom);
                WorkerResponse::success(
                    req_id,
                    serde_json::json!({
                        "title": session.page.title.clone(),
                        "url": session.page.url_string(),
                        "tree": distilled,
                    }),
                )
            } else {
                WorkerResponse::success(
                    req_id,
                    serde_json::json!({
                        "title": session.page.title.clone(),
                        "url": session.page.url_string(),
                        "tree": null,
                    }),
                )
            }
        }

        WorkerCommand::Click {
            session_id,
            selector,
        } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            let mut session = session_arc.lock().await;
            session.state = SessionState::Busy;
            session.touch();

            let script = format!(
                r#"(function() {{
                    const el = document.querySelector({});
                    if (!el) throw new Error("Element not found: " + {});
                    el.scrollIntoView({{ block: 'center', inline: 'center' }});
                    el.dispatchEvent(new MouseEvent('mousedown', {{ bubbles: true, cancelable: true }}));
                    el.dispatchEvent(new MouseEvent('mouseup', {{ bubbles: true, cancelable: true }}));
                    el.click();
                    return true;
                }})()"#,
                serde_json::to_string(&selector).unwrap_or_default(),
                serde_json::to_string(&selector).unwrap_or_default()
            );

            let res = session
                .page
                .evaluate_for_cdp_with_timeout(&script, true, true, 10_000)
                .await;

            let _ = session.page.run_autonomous_event_loop_turn().await;
            session.state = SessionState::Ready;

            match res {
                Ok(_) => WorkerResponse::success(
                    req_id,
                    serde_json::json!({ "clicked": true, "selector": selector }),
                ),
                Err(e) => WorkerResponse::error(req_id, "CLICK_ERROR", e.to_string()),
            }
        }

        WorkerCommand::Fill {
            session_id,
            selector,
            value,
        } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            let mut session = session_arc.lock().await;
            session.state = SessionState::Busy;
            session.touch();

            let script = format!(
                r#"(function() {{
                    const el = document.querySelector({});
                    if (!el) throw new Error("Element not found: " + {});
                    el.focus();
                    el.value = {};
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return true;
                }})()"#,
                serde_json::to_string(&selector).unwrap_or_default(),
                serde_json::to_string(&selector).unwrap_or_default(),
                serde_json::to_string(&value).unwrap_or_default()
            );

            let res = session
                .page
                .evaluate_for_cdp_with_timeout(&script, true, true, 10_000)
                .await;

            let _ = session.page.run_autonomous_event_loop_turn().await;
            session.state = SessionState::Ready;

            match res {
                Ok(_) => WorkerResponse::success(
                    req_id,
                    serde_json::json!({ "filled": true, "selector": selector }),
                ),
                Err(e) => WorkerResponse::error(req_id, "FILL_ERROR", e.to_string()),
            }
        }

        WorkerCommand::Type {
            session_id,
            selector,
            text,
        } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            let mut session = session_arc.lock().await;
            session.state = SessionState::Busy;
            session.touch();

            let script = format!(
                r#"(function() {{
                    const el = document.querySelector({});
                    if (!el) throw new Error("Element not found: " + {});
                    el.focus();
                    const text = {};
                    for (const char of text) {{
                        el.dispatchEvent(new KeyboardEvent('keydown', {{ key: char, bubbles: true }}));
                        el.dispatchEvent(new KeyboardEvent('keypress', {{ key: char, bubbles: true }}));
                        el.value = (el.value || '') + char;
                        el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                        el.dispatchEvent(new KeyboardEvent('keyup', {{ key: char, bubbles: true }}));
                    }}
                    return true;
                }})()"#,
                serde_json::to_string(&selector).unwrap_or_default(),
                serde_json::to_string(&selector).unwrap_or_default(),
                serde_json::to_string(&text).unwrap_or_default()
            );

            let res = session
                .page
                .evaluate_for_cdp_with_timeout(&script, true, true, 10_000)
                .await;

            let _ = session.page.run_autonomous_event_loop_turn().await;
            session.state = SessionState::Ready;

            match res {
                Ok(_) => WorkerResponse::success(
                    req_id,
                    serde_json::json!({ "typed": true, "selector": selector, "length": text.len() }),
                ),
                Err(e) => WorkerResponse::error(req_id, "TYPE_ERROR", e.to_string()),
            }
        }

        WorkerCommand::Screenshot { session_id } => {
            let _ = session_id;
            // Feature gate for screenshot support
            WorkerResponse::error(
                req_id,
                "UNSUPPORTED_FEATURE",
                "Screenshot requires a render-enabled build",
            )
        }

        WorkerCommand::Title { session_id } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };
            let session = session_arc.lock().await;
            WorkerResponse::success(req_id, serde_json::json!(session.page.title.clone()))
        }

        WorkerCommand::Dump { session_id, format } => {
            let session_arc = match manager.get_session(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        "SESSION_NOT_FOUND",
                        format!("Session '{session_id}' not found"),
                    )
                }
            };
            let session = session_arc.lock().await;

            let dumped = match format {
                DumpFormat::Html => session
                    .page
                    .with_dom(|dom| {
                        if let Ok(Some(html_node)) = dom.query_selector("html") {
                            dom.outer_html(html_node)
                        } else {
                            dom.inner_html(dom.document())
                        }
                    })
                    .unwrap_or_default(),
                DumpFormat::Text => session
                    .page
                    .with_dom(|dom| {
                        if let Ok(Some(body)) = dom.query_selector("body") {
                            dom.text_content(body)
                        } else {
                            String::new()
                        }
                    })
                    .unwrap_or_default(),
            };
            WorkerResponse::success(req_id, serde_json::json!(dumped))
        }
    };

    let elapsed = _start.elapsed();
    if let Some(hist) = global_metrics().histogram("obscura_worker_request_duration_seconds") {
        hist.observe_duration(elapsed);
    }
    result
}

/// Runs the framed JSON stdin/stdout worker event loop on a Tokio LocalSet.
pub async fn run_worker_loop() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::mpsc;

    let local = tokio::task::LocalSet::new();

    local
        .run_until(async move {
            let manager = SessionManager::new();
            let stdin = tokio::io::stdin();
            let mut reader = BufReader::new(stdin);
            let mut line = String::new();

            tracing::info!("Obscura Next Isolated Runtime Worker starting up");

            let (resp_tx, mut resp_rx) = mpsc::channel::<WorkerResponse>(128);

            let writer_handle = tokio::task::spawn_local(async move {
                let mut stdout = tokio::io::stdout();
                while let Some(resp) = resp_rx.recv().await {
                    if let Ok(mut out) = serde_json::to_string(&resp) {
                        out.push('\n');
                        let _ = stdout.write_all(out.as_bytes()).await;
                        let _ = stdout.flush().await;
                    }
                }
            });

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("Worker stdin read error: {e}");
                        break;
                    }
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let request: WorkerRequest = match serde_json::from_str(trimmed) {
                    Ok(req) => req,
                    Err(e) => {
                        let err_resp = WorkerResponse::error(
                            None,
                            "INVALID_JSON",
                            format!("Invalid JSON request: {e}"),
                        );
                        let _ = resp_tx.send(err_resp).await;
                        continue;
                    }
                };

                let is_shutdown = matches!(request.command, WorkerCommand::Shutdown);
                if is_shutdown {
                    let resp = dispatch_worker_command(&manager, request).await;
                    let _ = resp_tx.send(resp).await;
                    break;
                }

                manager.inc_active_requests();
                let manager_clone = manager.clone();
                let resp_tx_clone = resp_tx.clone();

                tokio::task::spawn_local(async move {
                    let resp = dispatch_worker_command(&manager_clone, request).await;
                    manager_clone.dec_active_requests();
                    let _ = resp_tx_clone.send(resp).await;
                });
            }

            drop(resp_tx);
            let _ = writer_handle.await;

            tracing::info!("Obscura Next Isolated Runtime Worker shutting down cleanly");
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_protocol_ping_and_health() {
        let manager = SessionManager::new();
        let ping_req = WorkerRequest {
            id: Some("1".to_string()),
            version: "1.0".to_string(),
            command: WorkerCommand::Ping,
        };
        let resp = dispatch_worker_command(&manager, ping_req).await;
        assert!(resp.ok);
        assert_eq!(resp.result, Some(serde_json::json!("pong")));
        assert_eq!(resp.id, Some("1".to_string()));

        let health_req = WorkerRequest {
            id: Some("2".to_string()),
            version: "1.0".to_string(),
            command: WorkerCommand::Health,
        };
        let health_resp = dispatch_worker_command(&manager, health_req).await;
        assert!(health_resp.ok);
        assert_eq!(health_resp.result.unwrap()["status"], "healthy");
    }

    #[tokio::test]
    async fn test_session_lifecycle_management() {
        let manager = SessionManager::new();
        let session_id = "test-session-1".to_string();

        let create_req = WorkerRequest {
            id: Some("3".to_string()),
            version: "1.0".to_string(),
            command: WorkerCommand::SessionCreate {
                session_id: session_id.clone(),
                config: SessionConfig::default(),
            },
        };
        let create_resp = dispatch_worker_command(&manager, create_req).await;
        assert!(create_resp.ok);
        assert_eq!(manager.active_count(), 1);

        // Duplicate creation returns SESSION_ALREADY_EXISTS
        let dup_req = WorkerRequest {
            id: Some("4".to_string()),
            version: "1.0".to_string(),
            command: WorkerCommand::SessionCreate {
                session_id: session_id.clone(),
                config: SessionConfig::default(),
            },
        };
        let dup_resp = dispatch_worker_command(&manager, dup_req).await;
        assert!(!dup_resp.ok);
        assert_eq!(dup_resp.error.unwrap().code, "SESSION_ALREADY_EXISTS");

        // Status check
        let status_req = WorkerRequest {
            id: Some("5".to_string()),
            version: "1.0".to_string(),
            command: WorkerCommand::SessionStatus {
                session_id: session_id.clone(),
            },
        };
        let status_resp = dispatch_worker_command(&manager, status_req).await;
        assert!(status_resp.ok);

        // Destroy
        let destroy_req = WorkerRequest {
            id: Some("6".to_string()),
            version: "1.0".to_string(),
            command: WorkerCommand::SessionDestroy {
                session_id: session_id.clone(),
            },
        };
        let destroy_resp = dispatch_worker_command(&manager, destroy_req).await;
        assert!(destroy_resp.ok);
        assert_eq!(manager.active_count(), 0);
    }

    #[tokio::test]
    async fn test_protocol_security_ssrf_block() {
        let manager = SessionManager::new();
        let session_id = "test-session-ssrf".to_string();

        manager
            .create_session(session_id.clone(), SessionConfig::default())
            .unwrap();

        // Target cloud metadata IP
        let nav_req = WorkerRequest {
            id: Some("7".to_string()),
            version: "1.0".to_string(),
            command: WorkerCommand::Navigate {
                session_id,
                url: "http://169.254.169.254/latest/meta-data".to_string(),
                wait_until: NavigationWaitUntil::Load,
                timeout_ms: Some(1000),
            },
        };

        let resp = dispatch_worker_command(&manager, nav_req).await;
        assert!(!resp.ok);
        let err = resp.error.unwrap();
        assert_eq!(err.code, "SECURITY_BLOCK");
        assert!(err.message.contains("Security block"));
    }

    #[tokio::test]
    async fn test_fine_grained_concurrency() {
        let manager = SessionManager::new();
        let s1 = "s1".to_string();
        let s2 = "s2".to_string();

        manager
            .create_session(s1.clone(), SessionConfig::default())
            .unwrap();
        manager
            .create_session(s2.clone(), SessionConfig::default())
            .unwrap();

        assert_eq!(manager.active_count(), 2);

        // Acquire both session handles
        let arc1 = manager.get_session(&s1).unwrap();
        let arc2 = manager.get_session(&s2).unwrap();

        // Lock session 1
        let mut guard1 = arc1.lock().await;
        guard1.state = SessionState::Busy;

        // Verify session 2 can still be locked and executed concurrently without blocking!
        let mut guard2 = arc2.lock().await;
        guard2.state = SessionState::Ready;
        guard2.touch();
        assert_eq!(guard2.requests_executed, 1);
        drop(guard2);

        assert_eq!(guard1.state, SessionState::Busy);
        drop(guard1);
    }
}
