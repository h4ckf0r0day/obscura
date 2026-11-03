//! Independent runtime worker and session lifecycle model for Obscura Next.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

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
        let isolation = IsolationPolicy::for_profile(
            config.profile.unwrap_or(IsolationProfile::Standard),
        );

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

/// Thread-safe manager for all active sessions in this worker.
#[derive(Clone, Default)]
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, ManagedSession>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn create_session(
        &self,
        id: String,
        config: SessionConfig,
    ) -> Result<(), WorkerError> {
        let mut guard = self.sessions.write();
        if guard.contains_key(&id) {
            return Err(WorkerError::SessionAlreadyExists(id));
        }

        let session = ManagedSession::new(id.clone(), config);
        guard.insert(id, session);

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

    pub fn active_count(&self) -> usize {
        self.sessions.read().len()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.sessions.read().contains_key(id)
    }

    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.read().keys().cloned().collect()
    }

    pub fn evict_idle(&self, max_idle: Duration) -> Vec<String> {
        let now = Instant::now();
        let mut guard = self.sessions.write();
        let mut evicted = Vec::new();

        guard.retain(|id, session| {
            if now.duration_since(session.last_active_at) > max_idle {
                evicted.push(id.clone());
                false
            } else {
                true
            }
        });

        if !evicted.is_empty() {
            let metrics = global_metrics();
            if let Some(gauge) = metrics.gauge("obscura_sessions_active") {
                gauge.set(guard.len() as i64);
            }
        }

        evicted
    }

    pub async fn execute_on_session<F, R>(&self, id: &str, f: F) -> Result<R, WorkerError>
    where
        F: FnOnce(&mut ManagedSession) -> Result<R, WorkerError>,
    {
        let mut guard = self.sessions.write();
        let session = guard
            .get_mut(id)
            .ok_or_else(|| WorkerError::SessionNotFound(id.to_string()))?;

        if !session.state.can_execute() {
            return Err(WorkerError::InvalidState(id.to_string(), session.state));
        }

        session.state = SessionState::Busy;
        session.touch();

        let result = f(session);

        session.state = SessionState::Ready;
        result
    }
}

// ---------------------------------------------------------------------------
// Worker IPC Protocol Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct WorkerRequest {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(flatten)]
    pub command: WorkerCommand,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "cmd")]
pub enum WorkerCommand {
    #[serde(rename = "ping")]
    Ping,

    #[serde(rename = "session_create")]
    SessionCreate {
        session_id: String,
        #[serde(default)]
        config: SessionConfig,
    },

    #[serde(rename = "session_destroy")]
    SessionDestroy { session_id: String },

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

    #[serde(rename = "title")]
    Title { session_id: String },

    #[serde(rename = "dump")]
    Dump {
        session_id: String,
        #[serde(default)]
        format: DumpFormat,
    },

    #[serde(rename = "health")]
    Health,

    #[serde(rename = "shutdown")]
    Shutdown,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Default, Deserialize, Serialize)]
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

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DumpFormat {
    #[default]
    Html,
    Text,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl WorkerResponse {
    pub fn success(id: Option<String>, result: serde_json::Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<String>, msg: String) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(msg),
        }
    }
}

/// Dispatches a protocol command against the session manager.
pub async fn dispatch_worker_command(
    manager: &SessionManager,
    request: WorkerRequest,
) -> WorkerResponse {
    let req_id = request.id;

    match request.command {
        WorkerCommand::Ping => WorkerResponse::success(req_id, serde_json::json!("pong")),

        WorkerCommand::Health => {
            let active = manager.active_count();
            WorkerResponse::success(
                req_id,
                serde_json::json!({
                    "status": "healthy",
                    "active_sessions": active,
                    "pid": std::process::id(),
                }),
            )
        }

        WorkerCommand::SessionCreate { session_id, config } => {
            match manager.create_session(session_id.clone(), config) {
                Ok(()) => WorkerResponse::success(
                    req_id,
                    serde_json::json!({ "session_id": session_id, "status": "created" }),
                ),
                Err(e) => WorkerResponse::error(req_id, e.to_string()),
            }
        }

        WorkerCommand::SessionDestroy { session_id } => {
            match manager.destroy_session(&session_id) {
                Ok(()) => WorkerResponse::success(
                    req_id,
                    serde_json::json!({ "session_id": session_id, "status": "destroyed" }),
                ),
                Err(e) => WorkerResponse::error(req_id, e.to_string()),
            }
        }

        WorkerCommand::Navigate {
            session_id,
            url,
            wait_until,
            timeout_ms: _,
        } => {
            let mut guard = manager.sessions.write();
            let session = match guard.get_mut(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            // Enforce isolation policy before navigation
            if let Err(err) = session.isolation.validate_url(&url) {
                let metrics = global_metrics();
                if let Some(counter) = metrics.counter("obscura_security_blocks_total") {
                    counter.inc();
                }
                return WorkerResponse::error(req_id, format!("Security block: {err}"));
            }

            session.state = SessionState::Busy;
            session.touch();

            let nav_res = session
                .page
                .navigate_with_wait(&url, wait_until.into())
                .await;

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
                            "status": 200
                        }),
                    )
                }
                Err(e) => WorkerResponse::error(req_id, e.to_string()),
            }
        }

        WorkerCommand::Evaluate {
            session_id,
            expression,
            await_promise,
            timeout_ms,
        } => {
            let mut guard = manager.sessions.write();
            let session = match guard.get_mut(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

            session.state = SessionState::Busy;
            session.touch();

            let timeout = timeout_ms.unwrap_or(30_000);
            let eval_res = session
                .page
                .evaluate_for_cdp_with_timeout(&expression, await_promise, true, timeout)
                .await;

            session.state = SessionState::Ready;

            match eval_res {
                Ok(info) => {
                    let val = match info.value {
                        Some(v) => v,
                        None => serde_json::Value::String(info.description),
                    };
                    WorkerResponse::success(req_id, val)
                }
                Err(e) => WorkerResponse::error(req_id, e.to_string()),
            }
        }

        WorkerCommand::Title { session_id } => {
            let guard = manager.sessions.read();
            match guard.get(&session_id) {
                Some(s) => {
                    WorkerResponse::success(req_id, serde_json::json!(s.page.title.clone()))
                }
                None => WorkerResponse::error(
                    req_id,
                    format!("Session '{session_id}' not found"),
                ),
            }
        }

        WorkerCommand::Dump { session_id, format } => {
            let guard = manager.sessions.read();
            let session = match guard.get(&session_id) {
                Some(s) => s,
                None => {
                    return WorkerResponse::error(
                        req_id,
                        format!("Session '{session_id}' not found"),
                    )
                }
            };

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

        WorkerCommand::Shutdown => {
            WorkerResponse::success(req_id, serde_json::json!("shutting down"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_lifecycle_management() {
        let manager = SessionManager::new();
        assert_eq!(manager.active_count(), 0);

        // Create session
        let res = manager.create_session("sess-1".to_string(), SessionConfig::default());
        assert!(res.is_ok());
        assert_eq!(manager.active_count(), 1);
        assert!(manager.contains("sess-1"));

        // Reject duplicate ID
        let dup = manager.create_session("sess-1".to_string(), SessionConfig::default());
        assert!(dup.is_err());

        // Destroy session
        assert!(manager.destroy_session("sess-1").is_ok());
        assert_eq!(manager.active_count(), 0);
        assert!(!manager.contains("sess-1"));
    }

    #[tokio::test]
    async fn test_protocol_ping_and_health() {
        let manager = SessionManager::new();

        let ping_req = WorkerRequest {
            id: Some("1".to_string()),
            command: WorkerCommand::Ping,
        };
        let ping_resp = dispatch_worker_command(&manager, ping_req).await;
        assert!(ping_resp.ok);
        assert_eq!(ping_resp.result.unwrap(), "pong");

        let health_req = WorkerRequest {
            id: Some("2".to_string()),
            command: WorkerCommand::Health,
        };
        let health_resp = dispatch_worker_command(&manager, health_req).await;
        assert!(health_resp.ok);
        assert_eq!(health_resp.result.unwrap()["status"], "healthy");
    }

    #[tokio::test]
    async fn test_protocol_security_ssrf_block() {
        let manager = SessionManager::new();
        manager
            .create_session("sess-sec".to_string(), SessionConfig::default())
            .unwrap();

        let nav_req = WorkerRequest {
            id: Some("ssrf-test".to_string()),
            command: WorkerCommand::Navigate {
                session_id: "sess-sec".to_string(),
                url: "http://169.254.169.254/latest/meta-data".to_string(),
                wait_until: NavigationWaitUntil::Load,
                timeout_ms: Some(1000),
            },
        };

        let nav_resp = dispatch_worker_command(&manager, nav_req).await;
        assert!(!nav_resp.ok);
        assert!(nav_resp.error.unwrap().contains("Security block"));
    }
}
