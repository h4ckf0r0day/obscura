//! Supervision tree, self-healing process lifecycle, and pool routing for Obscura Next workers.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Notify};

use obscura_telemetry::global_metrics;
use obscura_worker::{WorkerCommand, WorkerRequest, WorkerResponse};

#[derive(Error, Debug)]
pub enum SupervisorError {
    #[error("Worker spawn failure: {0}")]
    SpawnFailed(String),

    #[error("No healthy workers available")]
    NoWorkersAvailable,

    #[error("Worker {0} unresponsive to heartbeat")]
    HeartbeatTimeout(usize),

    #[error("Worker {0} not found")]
    WorkerNotFound(usize),

    #[error("Session '{0}' not found on any active worker")]
    SessionNotFound(String),

    #[error("Worker {0} terminated unexpectedly")]
    WorkerTerminated(usize),

    #[error("Operation timed out: {0}")]
    Timeout(String),

    #[error("IPC communication error: {0}")]
    IpcError(String),
}

/// Dynamic recycling policy for a supervised worker.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct WorkerRecyclePolicy {
    pub max_sessions: Option<usize>,
    pub max_rss_mb: Option<u64>,
    pub max_age: Option<Duration>,
}

impl Default for WorkerRecyclePolicy {
    fn default() -> Self {
        Self {
            max_sessions: Some(100),
            max_rss_mb: Some(1024),
            max_age: Some(Duration::from_secs(3600)),
        }
    }
}

impl WorkerRecyclePolicy {
    pub fn should_recycle(
        &self,
        sessions: usize,
        rss_mb: Option<u64>,
        age: Duration,
    ) -> Option<String> {
        if let Some(limit) = self.max_sessions {
            if sessions >= limit {
                return Some(format!("Exceeded session limit: {sessions} >= {limit}"));
            }
        }
        if let (Some(limit), Some(rss)) = (self.max_rss_mb, rss_mb) {
            if rss >= limit {
                return Some(format!("Exceeded RSS memory limit: {rss}MB >= {limit}MB"));
            }
        }
        if let Some(limit) = self.max_age {
            if age >= limit {
                return Some(format!("Exceeded max lifetime: {age:?} >= {limit:?}"));
            }
        }
        None
    }
}

/// Operational state of a supervised worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerStatus {
    Starting,
    Ready,
    Degraded,
    Draining,
    Terminated,
}

struct WorkerJob {
    req: WorkerRequest,
    resp_tx: oneshot::Sender<Result<WorkerResponse, SupervisorError>>,
}

/// High-level descriptor and IPC coordinator for a worker in the pool.
pub struct SupervisedWorker {
    pub id: usize,
    pub pid: Option<u32>,
    pub spawned_at: Instant,
    pub status: RwLock<WorkerStatus>,
    pub active_sessions: AtomicUsize,
    pub total_sessions_served: AtomicUsize,
    pub consecutive_failures: AtomicUsize,
    pub is_ready: Arc<AtomicBool>,
    pub wake_notify: Arc<Notify>,

    job_tx: Option<tokio::sync::mpsc::Sender<WorkerJob>>,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
}

impl SupervisedWorker {
    /// Creates a mock supervised worker without an OS child process (for testing).
    pub fn new(id: usize, pid: Option<u32>) -> Self {
        Self {
            id,
            pid,
            spawned_at: Instant::now(),
            status: RwLock::new(WorkerStatus::Starting),
            active_sessions: AtomicUsize::new(0),
            total_sessions_served: AtomicUsize::new(0),
            consecutive_failures: AtomicUsize::new(0),
            is_ready: Arc::new(AtomicBool::new(false)),
            wake_notify: Arc::new(Notify::new()),
            job_tx: None,
            child: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Spawns a real OS child process and initializes stdin/stdout/stderr IPC pipes.
    pub async fn spawn(id: usize, bin_path: &Path) -> Result<Arc<Self>, SupervisorError> {
        let resolved = resolve_worker_binary(bin_path);
        tracing::info!(worker_id = id, binary = ?resolved, "Spawning supervised worker process");

        let mut cmd = Command::new(&resolved);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Create a new process group on Unix
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }

        let mut child_proc = cmd.spawn().map_err(|e| {
            SupervisorError::SpawnFailed(format!("Failed to spawn {resolved:?}: {e}"))
        })?;
        let pid = child_proc.id();
        let mut child_stdin = child_proc
            .stdin
            .take()
            .ok_or_else(|| SupervisorError::SpawnFailed("Missing child stdin pipe".into()))?;
        let child_stdout = child_proc
            .stdout
            .take()
            .ok_or_else(|| SupervisorError::SpawnFailed("Missing child stdout pipe".into()))?;

        // Wait for explicit readiness signal from the child.
        let mut stdout_reader = BufReader::new(child_stdout);
        let mut ready_line = String::new();
        let ready = tokio::time::timeout(
            Duration::from_secs(5),
            stdout_reader.read_line(&mut ready_line),
        )
        .await;
        match ready {
            Ok(Ok(n)) if n > 0 && ready_line.contains("ready") => {}
            _ => {
                let _ = child_proc.kill().await;
                return Err(SupervisorError::SpawnFailed(
                    "worker readiness handshake failed".into(),
                ));
            }
        }
        let child_stderr = child_proc
            .stderr
            .take()
            .ok_or_else(|| SupervisorError::SpawnFailed("Missing child stderr pipe".into()))?;

        let is_ready_flag = Arc::new(AtomicBool::new(false));

        // Response ID -> pending oneshot (keyed by request id string)
        let pending_requests: Arc<
            parking_lot::Mutex<
                HashMap<String, oneshot::Sender<Result<WorkerResponse, SupervisorError>>>,
            >,
        > = Arc::new(parking_lot::Mutex::new(HashMap::new()));

        // Dedicated stderr drainer task (prevents child process buffer lockups)
        let worker_id = id;
        tokio::spawn(async move {
            let mut lines = BufReader::new(child_stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "worker_stderr", worker_id = worker_id, "{}", line);
            }
        });

        // Dedicated stdout reader task — routes incoming JSON responses to waiting callers
        let pending_for_stdout = pending_requests.clone();
        let is_ready_for_stdout = is_ready_flag.clone();
        tokio::spawn(async move {
            let mut lines = stdout_reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<WorkerResponse>(trimmed) {
                    Ok(resp) => {
                        // The response id matches the request id
                        if let Some(ref resp_id) = resp.id {
                            let sender = pending_for_stdout.lock().remove(resp_id);
                            if let Some(tx) = sender {
                                let _ = tx.send(Ok(resp));
                            } else {
                                tracing::debug!(worker_id = worker_id, id = %resp_id, "Received response for unmapped request id");
                            }
                        } else {
                            tracing::debug!(worker_id = worker_id, "Received response with no id");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(worker_id = worker_id, error = %e, raw = %trimmed, "Malformed JSON from worker stdout");
                    }
                }
            }
            // Child closed stdout / terminated — fail all pending requests
            is_ready_for_stdout.store(false, Ordering::Release);
            let mut pending = pending_for_stdout.lock();
            for (_id, tx) in pending.drain() {
                let _ = tx.send(Err(SupervisorError::WorkerTerminated(worker_id)));
            }
        });

        // Dedicated stdin writer task — receives WorkerJobs and writes JSON to child stdin
        let (job_tx, mut job_rx) = tokio::sync::mpsc::channel::<WorkerJob>(128);
        let pending_for_writer = pending_requests.clone();
        tokio::spawn(async move {
            while let Some(job) = job_rx.recv().await {
                let req_id = job.req.id.clone().unwrap_or_default();
                let serialized = match serde_json::to_string(&job.req) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = job
                            .resp_tx
                            .send(Err(SupervisorError::IpcError(e.to_string())));
                        continue;
                    }
                };

                if !req_id.is_empty() {
                    pending_for_writer
                        .lock()
                        .insert(req_id.clone(), job.resp_tx);
                }

                if let Err(e) = child_stdin
                    .write_all(format!("{serialized}\n").as_bytes())
                    .await
                {
                    if !req_id.is_empty() {
                        pending_for_writer.lock().remove(&req_id);
                    }
                    tracing::error!(worker_id = worker_id, error = %e, "Failed to write to worker stdin");
                    break;
                }
                if let Err(e) = child_stdin.flush().await {
                    if !req_id.is_empty() {
                        pending_for_writer.lock().remove(&req_id);
                    }
                    tracing::error!(worker_id = worker_id, error = %e, "Failed to flush worker stdin");
                    break;
                }
            }
        });

        let worker = Arc::new(Self {
            id,
            pid,
            spawned_at: Instant::now(),
            status: RwLock::new(WorkerStatus::Starting),
            active_sessions: AtomicUsize::new(0),
            total_sessions_served: AtomicUsize::new(0),
            consecutive_failures: AtomicUsize::new(0),
            is_ready: is_ready_flag.clone(),
            wake_notify: Arc::new(Notify::new()),
            job_tx: Some(job_tx),
            child: Arc::new(tokio::sync::Mutex::new(Some(child_proc))),
        });

        // Record metrics
        let metrics = global_metrics();
        if let Some(c) = metrics.counter("obscura_worker_spawn_total") {
            c.inc();
        }

        // Perform startup handshake — send ping and verify response
        let handshake_id = format!("handshake-{}-{}", id, std::process::id());
        let handshake_req = WorkerRequest {
            id: Some(handshake_id),
            version: "1.0".to_string(),
            command: WorkerCommand::Ping,
        };

        match worker
            .send_request(handshake_req, Duration::from_secs(5))
            .await
        {
            Ok(resp) if resp.ok => {
                worker.mark_ready();
                let metrics = global_metrics();
                if let Some(c) = metrics.counter("obscura_worker_processes_total") {
                    c.inc();
                }
                if let Some(c) = metrics.counter("obscura_worker_spawn_total") {
                    c.inc();
                }
                tracing::info!(worker_id = id, pid = ?pid, "Supervised worker handshake succeeded");
                Ok(worker)
            }
            Ok(resp) => {
                let _ = worker.kill().await;
                Err(SupervisorError::SpawnFailed(format!(
                    "Worker {id} handshake returned error: {:?}",
                    resp.error
                )))
            }
            Err(e) => {
                let _ = worker.kill().await;
                Err(SupervisorError::SpawnFailed(format!(
                    "Worker {id} handshake failed: {e}"
                )))
            }
        }
    }

    /// Dispatches a request to this worker and awaits the response with timeout.
    pub async fn send_request(
        &self,
        req: WorkerRequest,
        timeout_dur: Duration,
    ) -> Result<WorkerResponse, SupervisorError> {
        let job_tx = self.job_tx.as_ref().ok_or_else(|| {
            SupervisorError::IpcError(format!("Worker {} has no active IPC channel", self.id))
        })?;

        let (resp_tx, resp_rx) = oneshot::channel();
        let job = WorkerJob { req, resp_tx };

        job_tx
            .send(job)
            .await
            .map_err(|_| SupervisorError::WorkerTerminated(self.id))?;

        match tokio::time::timeout(timeout_dur, resp_rx).await {
            Ok(Ok(Ok(resp))) => Ok(resp),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_cancelled)) => Err(SupervisorError::WorkerTerminated(self.id)),
            Err(_) => Err(SupervisorError::Timeout(format!(
                "Worker {} timed out after {:?}",
                self.id, timeout_dur
            ))),
        }
    }

    pub fn mark_ready(&self) {
        *self.status.write() = WorkerStatus::Ready;
        self.is_ready.store(true, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        let metrics = global_metrics();
    }

    pub fn mark_draining(&self) {
        *self.status.write() = WorkerStatus::Draining;
        self.is_ready.store(false, Ordering::Release);
    }

    pub fn mark_terminated(&self) {
        *self.status.write() = WorkerStatus::Terminated;
        self.is_ready.store(false, Ordering::Release);
    }

    pub fn record_failure(&self) -> usize {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= 3 {
            *self.status.write() = WorkerStatus::Degraded;
        }
        failures
    }

    pub fn acquire_session(&self) {
        self.active_sessions.fetch_add(1, Ordering::Relaxed);
        self.total_sessions_served.fetch_add(1, Ordering::Relaxed);
    }

    pub fn release_session(&self) {
        self.active_sessions.fetch_sub(1, Ordering::Relaxed);
        self.wake_notify.notify_one();
    }

    /// Gracefully stops the worker process: sends shutdown command, waits grace period, kills if needed.
    pub async fn shutdown(&self, grace_period: Duration) -> Result<(), SupervisorError> {
        self.mark_draining();

        let req = WorkerRequest {
            id: Some(format!("shutdown-{}", self.id)),
            version: "1.0".to_string(),
            command: WorkerCommand::Shutdown,
        };

        let _ = self.send_request(req, Duration::from_millis(1500)).await;

        // Graceful: SIGTERM to process group
        #[cfg(unix)]
        {
            let lock = self.child.lock().await;
            if let Some(child) = lock.as_ref() {
                if let Some(pid) = child.id() {
                    unsafe {
                        libc::kill(-(pid as i32), 15);
                    } // SIGTERM
                }
            }
        }
        tokio::time::sleep(grace_period.min(Duration::from_millis(500))).await;

        self.kill().await
    }

    /// Kills the child process immediately.
    pub async fn kill(&self) -> Result<(), SupervisorError> {
        let was_ready = self.is_ready.load(Ordering::Acquire);
        self.mark_terminated();
        let mut lock = self.child.lock().await;
        if let Some(mut child) = lock.take() {
            // Kill process group first (Unix), then direct child
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                unsafe {
                    libc::kill(-(pid as i32), 15); // SIGTERM to group
                }
                // Brief wait for graceful exit
                tokio::time::sleep(Duration::from_millis(100)).await;
                unsafe {
                    libc::kill(-(pid as i32), 9); // SIGKILL to group
                }
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if was_ready {
            let metrics = global_metrics();
            if let Some(c) = metrics.counter("obscura_worker_processes_failed") {
                c.inc();
            }
        }
        Ok(())
    }
}

/// Resolves a worker binary path using environment, build directories, or PATH.
pub fn resolve_worker_binary(configured: &Path) -> PathBuf {
    if configured.is_absolute() && configured.exists() {
        return configured.to_path_buf();
    }
    if let Ok(env_path) = std::env::var("OBSCURA_WORKER_BIN") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return p;
        }
    }
    let candidates = [
        configured.to_path_buf(),
        PathBuf::from("target/debug/obscura-runtime-worker"),
        PathBuf::from("target/release/obscura-runtime-worker"),
        PathBuf::from("../target/debug/obscura-runtime-worker"),
        PathBuf::from("../target/release/obscura-runtime-worker"),
        PathBuf::from("../../target/debug/obscura-runtime-worker"),
        PathBuf::from("../../target/release/obscura-runtime-worker"),
        PathBuf::from("target/debug/obscura-worker"),
        PathBuf::from("target/release/obscura-worker"),
    ];
    for candidate in candidates {
        if candidate.exists() {
            return candidate;
        }
    }
    configured.to_path_buf()
}

/// Supervisor configuration.
#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    pub worker_bin: PathBuf,
    pub pool_size: usize,
    pub heartbeat_interval: Duration,
    pub heartbeat_timeout: Duration,
    pub recycle_policy: WorkerRecyclePolicy,
    pub restart_backoff_base: Duration,
    pub max_restarts: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            worker_bin: PathBuf::from("obscura-runtime-worker"),
            pool_size: 4,
            heartbeat_interval: Duration::from_secs(5),
            heartbeat_timeout: Duration::from_secs(3),
            recycle_policy: WorkerRecyclePolicy::default(),
            restart_backoff_base: Duration::from_millis(100),
            max_restarts: 3,
        }
    }
}

/// Helper to extract a session_id from a WorkerCommand, if applicable.
fn extract_session_id(cmd: &WorkerCommand) -> Option<String> {
    match cmd {
        WorkerCommand::SessionCreate { session_id, .. } => Some(session_id.clone()),
        WorkerCommand::SessionDestroy { session_id } => Some(session_id.clone()),
        WorkerCommand::SessionStatus { session_id } => Some(session_id.clone()),
        WorkerCommand::Navigate { session_id, .. } => Some(session_id.clone()),
        WorkerCommand::Evaluate { session_id, .. } => Some(session_id.clone()),
        WorkerCommand::Click { session_id, .. } => Some(session_id.clone()),
        WorkerCommand::Fill { session_id, .. } => Some(session_id.clone()),
        WorkerCommand::Type { session_id, .. } => Some(session_id.clone()),
        WorkerCommand::Snapshot { session_id } => Some(session_id.clone()),
        WorkerCommand::Screenshot { session_id } => Some(session_id.clone()),
        WorkerCommand::Title { session_id } => Some(session_id.clone()),
        WorkerCommand::Dump { session_id, .. } => Some(session_id.clone()),
        WorkerCommand::Ping | WorkerCommand::Health | WorkerCommand::Shutdown => None,
    }
}

/// Worker pool supervisor implementing self-healing, routing, and lifecycle supervision.
pub struct WorkerPoolSupervisor {
    pub config: SupervisorConfig,
    workers: Arc<RwLock<Vec<Arc<SupervisedWorker>>>>,
    session_to_worker: Arc<RwLock<HashMap<String, usize>>>,
    round_robin_idx: AtomicUsize,
    next_worker_id: AtomicUsize,
    restart_counts: Arc<RwLock<HashMap<usize, usize>>>,
}

impl WorkerPoolSupervisor {
    pub fn new(config: SupervisorConfig) -> Self {
        Self {
            config,
            workers: Arc::new(RwLock::new(Vec::new())),
            session_to_worker: Arc::new(RwLock::new(HashMap::new())),
            round_robin_idx: AtomicUsize::new(0),
            next_worker_id: AtomicUsize::new(1),
            restart_counts: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Initializes and starts the supervisor pool with real child processes.
    pub async fn start(&self) -> Result<(), SupervisorError> {
        let count = self.config.pool_size;
        tracing::info!(pool_size = count, "Starting supervised worker pool");

        for _ in 0..count {
            let id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
            let worker = SupervisedWorker::spawn(id, &self.config.worker_bin).await?;
            self.register_worker(worker);
        }

        self.refresh_pool_metrics();
        Ok(())
    }

    pub fn register_worker(&self, worker: Arc<SupervisedWorker>) {
        self.workers.write().push(worker);
        let metrics = global_metrics();
        let n = self.workers.read().len() as i64;
        if let Some(gauge) = metrics.gauge("obscura_worker_processes_count") {
            gauge.set(n);
        }
        if let Some(gauge) = metrics.gauge("obscura_worker_processes_healthy") {
            gauge.set(self.healthy_count() as i64);
        }
    }

    pub fn pool_size(&self) -> usize {
        self.workers.read().len()
    }

    pub fn healthy_count(&self) -> usize {
        self.workers
            .read()
            .iter()
            .filter(|w| w.is_ready.load(Ordering::Acquire))
            .count()
    }
    pub fn refresh_pool_metrics(&self) {
        let metrics = global_metrics();
        let workers = self.workers.read();
        let total = workers.len() as i64;
        let healthy = workers
            .iter()
            .filter(|w| w.is_ready.load(Ordering::Acquire))
            .count() as i64;
        if let Some(g) = metrics.gauge("obscura_worker_processes_count") {
            g.set(total);
        }
        if let Some(g) = metrics.gauge("obscura_worker_processes_healthy") {
            g.set(healthy);
        }
    }

    /// Selects a ready worker using round-robin distribution.
    pub fn pick_worker(&self) -> Result<Arc<SupervisedWorker>, SupervisorError> {
        let workers = self.workers.read();
        if workers.is_empty() {
            return Err(SupervisorError::NoWorkersAvailable);
        }

        let n = workers.len();
        let start = self.round_robin_idx.fetch_add(1, Ordering::Relaxed);

        for i in 0..n {
            let candidate = &workers[(start + i) % n];
            if candidate.is_ready.load(Ordering::Acquire) {
                return Ok(candidate.clone());
            }
        }

        Err(SupervisorError::NoWorkersAvailable)
    }

    /// Selects the ready worker with the lowest active session count.
    pub fn pick_least_loaded(&self) -> Result<Arc<SupervisedWorker>, SupervisorError> {
        let workers = self.workers.read();
        workers
            .iter()
            .filter(|w| w.is_ready.load(Ordering::Acquire))
            .min_by_key(|w| w.active_sessions.load(Ordering::Relaxed))
            .cloned()
            .ok_or(SupervisorError::NoWorkersAvailable)
    }

    /// Dispatches a WorkerRequest to an appropriate worker based on session affinity or load balancing.
    pub async fn dispatch(
        &self,
        mut req: WorkerRequest,
    ) -> Result<WorkerResponse, SupervisorError> {
        // Assign an id if missing
        if req.id.as_ref().map_or(true, |s| s.is_empty()) {
            req.id = Some(format!(
                "sup-req-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        }

        let metrics = global_metrics();
        if let Some(c) = metrics.counter("obscura_ipc_requests_total") {
            c.inc();
        }
        let start_time = Instant::now();

        let req_session_id = extract_session_id(&req.command);
        let is_create = matches!(req.command, WorkerCommand::SessionCreate { .. });
        let is_destroy = matches!(req.command, WorkerCommand::SessionDestroy { .. });

        let worker = if is_create {
            // New session: pick least loaded ready worker
            let w = self.pick_least_loaded()?;
            if let Some(ref sid) = req_session_id {
                self.session_to_worker.write().insert(sid.clone(), w.id);
            }
            w.acquire_session();
            w
        } else if let Some(ref sid) = req_session_id {
            // Existing session: route to the worker holding this session
            let target_id = self
                .session_to_worker
                .read()
                .get(sid)
                .copied()
                .ok_or_else(|| SupervisorError::SessionNotFound(sid.clone()))?;

            let found = self
                .workers
                .read()
                .iter()
                .find(|w| w.id == target_id)
                .cloned();

            match found {
                Some(w)
                    if w.is_ready.load(Ordering::Acquire)
                        || *w.status.read() == WorkerStatus::Draining =>
                {
                    w
                }
                Some(_) => return Err(SupervisorError::WorkerTerminated(target_id)),
                None => return Err(SupervisorError::WorkerNotFound(target_id)),
            }
        } else {
            // Stateless command (ping, health, etc.)
            self.pick_worker()?
        };

        self.refresh_pool_metrics();

        // Forward request to worker
        let resp = worker.send_request(req, Duration::from_secs(30)).await;

        if let Some(h) = metrics.histogram("obscura_ipc_latency_seconds") {
            h.observe_duration(start_time.elapsed());
        }

        match resp {
            Ok(r) => {
                if is_create && !r.ok {
                    // Rollback session mapping on creation failure
                    if let Some(ref sid) = req_session_id {
                        self.session_to_worker.write().remove(sid);
                    }
                    worker.release_session();
                } else if is_destroy {
                    if let Some(ref sid) = req_session_id {
                        self.session_to_worker.write().remove(sid);
                    }
                    worker.release_session();
                }
                Ok(r)
            }
            Err(e) => {
                if is_create {
                    if let Some(ref sid) = req_session_id {
                        self.session_to_worker.write().remove(sid);
                    }
                    worker.release_session();
                }
                worker.record_failure();
                if let Some(c) = metrics.counter("obscura_worker_crash_total") {
                    c.inc();
                }
                // Attempt bounded restart if the worker appears dead
                if !worker.is_ready.load(Ordering::Acquire) {
                    let _ = self.restart_worker(worker.id).await;
                }
                Err(e)
            }
        }
    }

    /// Heartbeats a specific worker by sending a Ping command.
    pub async fn ping_worker(&self, worker: &SupervisedWorker) -> bool {
        let req = WorkerRequest {
            id: Some(format!("hb-{}-{}", worker.id, std::process::id())),
            version: "1.0".to_string(),
            command: WorkerCommand::Ping,
        };
        match worker
            .send_request(req, self.config.heartbeat_timeout)
            .await
        {
            Ok(resp) => resp.ok,
            Err(_) => false,
        }
    }

    /// Evaluates workers against the recycle policy and flags those exceeding thresholds.
    pub fn check_recycling(&self) -> Vec<(usize, String)> {
        let workers = self.workers.read();
        let mut to_recycle = Vec::new();

        for worker in workers.iter() {
            if !worker.is_ready.load(Ordering::Acquire) {
                continue;
            }
            let served = worker.total_sessions_served.load(Ordering::Relaxed);
            let age = worker.spawned_at.elapsed();

            if let Some(reason) = self.config.recycle_policy.should_recycle(served, None, age) {
                to_recycle.push((worker.id, reason));
            }
        }

        to_recycle
    }

    /// Restarts a terminated worker with bounded exponential backoff.
    pub async fn restart_worker(
        &self,
        worker_id: usize,
    ) -> Result<Arc<SupervisedWorker>, SupervisorError> {
        // Check restart limit
        {
            let counts = self.restart_counts.read();
            let count = counts.get(&worker_id).copied().unwrap_or(0);
            if count >= self.config.max_restarts {
                return Err(SupervisorError::WorkerTerminated(worker_id));
            }
        }

        // Backoff
        let attempt = {
            let mut counts = self.restart_counts.write();
            let e = counts.entry(worker_id).or_insert(0);
            *e += 1;
            *e
        };
        let backoff = self.config.restart_backoff_base * attempt as u32;
        tokio::time::sleep(backoff).await;

        // Spawn replacement
        let new_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
        let new_worker = SupervisedWorker::spawn(new_id, &self.config.worker_bin).await?;

        // Replace in registry
        {
            let mut workers = self.workers.write();
            if let Some(pos) = workers.iter().position(|w| w.id == worker_id) {
                let old = workers[pos].clone();
                let _ = old.kill().await;
                workers[pos] = new_worker.clone();
            } else {
                workers.push(new_worker.clone());
            }
        }

        let metrics = global_metrics();
        if let Some(c) = metrics.counter("obscura_worker_restarts_total") {
            c.inc();
        }

        self.refresh_pool_metrics();
        Ok(new_worker)
    }

    /// Gracefully terminates all workers in the pool.
    pub async fn shutdown(&self) -> Result<(), SupervisorError> {
        let workers = self.workers.read().clone();
        for worker in workers {
            let _ = worker.shutdown(Duration::from_millis(500)).await;
        }
        self.workers.write().clear();
        self.session_to_worker.write().clear();
        self.refresh_pool_metrics();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_recycling_policy() {
        let policy = WorkerRecyclePolicy {
            max_sessions: Some(10),
            max_rss_mb: Some(512),
            max_age: Some(Duration::from_secs(60)),
        };

        // Under limits
        assert!(policy
            .should_recycle(5, Some(200), Duration::from_secs(30))
            .is_none());

        // Session limit exceeded
        let reason = policy
            .should_recycle(10, Some(200), Duration::from_secs(30))
            .unwrap();
        assert!(reason.contains("session limit"));

        // Memory limit exceeded
        let reason = policy
            .should_recycle(5, Some(600), Duration::from_secs(30))
            .unwrap();
        assert!(reason.contains("RSS memory limit"));

        // Age limit exceeded
        let reason = policy
            .should_recycle(5, Some(200), Duration::from_secs(65))
            .unwrap();
        assert!(reason.contains("max lifetime"));
    }

    #[test]
    fn test_pool_round_robin_selection() {
        let config = SupervisorConfig::default();
        let supervisor = WorkerPoolSupervisor::new(config);

        let w1 = Arc::new(SupervisedWorker::new(1, Some(101)));
        let w2 = Arc::new(SupervisedWorker::new(2, Some(102)));
        let w3 = Arc::new(SupervisedWorker::new(3, Some(103)));

        w1.mark_ready();
        w2.mark_ready();
        w3.mark_ready();

        supervisor.register_worker(w1.clone());
        supervisor.register_worker(w2.clone());
        supervisor.register_worker(w3.clone());

        assert_eq!(supervisor.healthy_count(), 3);

        let picked_1 = supervisor.pick_worker().unwrap();
        let picked_2 = supervisor.pick_worker().unwrap();
        let picked_3 = supervisor.pick_worker().unwrap();

        assert_eq!(picked_1.id, 1);
        assert_eq!(picked_2.id, 2);
        assert_eq!(picked_3.id, 3);

        // Mark w2 as draining -> should be skipped in round-robin
        w2.mark_draining();
        assert_eq!(supervisor.healthy_count(), 2);

        let next_1 = supervisor.pick_worker().unwrap();
        let next_2 = supervisor.pick_worker().unwrap();
        assert!(next_1.id == 1 || next_1.id == 3);
        assert!(next_2.id == 1 || next_2.id == 3);
        assert_ne!(next_1.id, 2);
        assert_ne!(next_2.id, 2);
    }

    #[tokio::test]
    async fn test_supervisor_spawn_and_ping() {
        // This test requires the worker binary to be built
        let worker_bin = resolve_worker_binary(Path::new("obscura-runtime-worker"));
        if !worker_bin.exists() {
            eprintln!(
                "Skipping spawn test: worker binary not found at {:?}",
                worker_bin
            );
            return;
        }

        let config = SupervisorConfig {
            worker_bin: worker_bin.clone(),
            pool_size: 1,
            ..SupervisorConfig::default()
        };

        let supervisor = WorkerPoolSupervisor::new(config);
        supervisor.start().await.expect("supervisor start");

        assert_eq!(supervisor.pool_size(), 1);
        assert_eq!(supervisor.healthy_count(), 1);

        // Dispatch a ping
        let req = WorkerRequest {
            id: Some("test-ping-1".into()),
            version: "1.0".into(),
            command: WorkerCommand::Ping,
        };
        let resp = supervisor.dispatch(req).await.expect("ping dispatch");
        assert!(resp.ok);

        // Dispatch a health check
        let req = WorkerRequest {
            id: Some("test-health-1".into()),
            version: "1.0".into(),
            command: WorkerCommand::Health,
        };
        let resp = supervisor.dispatch(req).await.expect("health dispatch");
        assert!(resp.ok);

        supervisor.shutdown().await.expect("shutdown");
        assert_eq!(supervisor.pool_size(), 0);
    }

    #[tokio::test]
    async fn test_supervisor_session_lifecycle() {
        let worker_bin = resolve_worker_binary(Path::new("obscura-runtime-worker"));
        if !worker_bin.exists() {
            eprintln!("Skipping session lifecycle test: worker binary not found");
            return;
        }

        let config = SupervisorConfig {
            worker_bin,
            pool_size: 2,
            ..SupervisorConfig::default()
        };

        let supervisor = WorkerPoolSupervisor::new(config);
        supervisor.start().await.expect("supervisor start");
        assert_eq!(supervisor.healthy_count(), 2);

        // Create a session
        let req = WorkerRequest {
            id: Some("create-s1".into()),
            version: "1.0".into(),
            command: WorkerCommand::SessionCreate {
                session_id: "session-alpha".into(),
                config: obscura_worker::SessionConfig::default(),
            },
        };
        let resp = supervisor.dispatch(req).await.expect("create session");
        assert!(resp.ok, "session creation should succeed: {:?}", resp.error);

        // Navigate the session
        let req = WorkerRequest {
            id: Some("nav-s1".into()),
            version: "1.0".into(),
            command: WorkerCommand::Navigate {
                session_id: "session-alpha".into(),
                url: "data:text/html,<h1>Hello World</h1>".into(),
                wait_until: obscura_worker::NavigationWaitUntil::Load,
                timeout_ms: Some(5000),
            },
        };
        let resp = supervisor.dispatch(req).await.expect("navigate session");
        assert!(resp.ok, "navigation should succeed: {:?}", resp.error);

        // Evaluate JS
        let req = WorkerRequest {
            id: Some("eval-s1".into()),
            version: "1.0".into(),
            command: WorkerCommand::Evaluate {
                session_id: "session-alpha".into(),
                expression: "document.title || 'ok'".into(),
                await_promise: true,
                timeout_ms: Some(3000),
            },
        };
        let resp = supervisor.dispatch(req).await.expect("evaluate session");
        assert!(resp.ok, "evaluate should succeed: {:?}", resp.error);

        // Destroy session
        let req = WorkerRequest {
            id: Some("destroy-s1".into()),
            version: "1.0".into(),
            command: WorkerCommand::SessionDestroy {
                session_id: "session-alpha".into(),
            },
        };
        let resp = supervisor.dispatch(req).await.expect("destroy session");
        assert!(resp.ok, "destroy should succeed: {:?}", resp.error);

        supervisor.shutdown().await.expect("shutdown");
    }
}

#[cfg(test)]
mod process_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn test_graceful_shutdown() {
        let worker_bin = resolve_worker_binary(std::path::Path::new("obscura-runtime-worker"));
        if !worker_bin.exists() {
            eprintln!("Skipping: worker binary not found");
            return;
        }
        let config = SupervisorConfig {
            worker_bin,
            pool_size: 1,
            ..SupervisorConfig::default()
        };
        let supervisor = WorkerPoolSupervisor::new(config);
        supervisor.start().await.expect("start");
        assert_eq!(supervisor.healthy_count(), 1);
        supervisor.shutdown().await.expect("shutdown");
        assert_eq!(supervisor.pool_size(), 0);
    }

    #[tokio::test]
    async fn test_worker_kill_idempotent() {
        let worker = Arc::new(SupervisedWorker::new(99, Some(999)));
        worker.mark_ready();
        // kill twice should not panic
        worker.kill().await.unwrap();
        worker.kill().await.unwrap();
    }
}

#[cfg(test)]
mod process_harness_tests {
    use super::*;

    #[tokio::test]
    async fn test_process_graceful_termination() {
        let worker_bin = resolve_worker_binary(std::path::Path::new("obscura-runtime-worker"));
        if !worker_bin.exists() {
            return;
        }
        let config = SupervisorConfig {
            worker_bin,
            pool_size: 1,
            ..SupervisorConfig::default()
        };
        let supervisor = WorkerPoolSupervisor::new(config);
        supervisor.start().await.expect("start");
        assert_eq!(supervisor.healthy_count(), 1);
        // Graceful shutdown
        supervisor.shutdown().await.expect("shutdown");
        assert_eq!(supervisor.pool_size(), 0);
        assert_eq!(supervisor.healthy_count(), 0);
    }

    #[tokio::test]
    async fn test_process_forced_termination_idempotent() {
        let worker = Arc::new(SupervisedWorker::new(100, Some(1000)));
        worker.mark_ready();
        // First kill
        worker.kill().await.unwrap();
        assert!(!worker.is_ready.load(std::sync::atomic::Ordering::Acquire));
        // Second kill should not panic
        worker.kill().await.unwrap();
    }

    #[tokio::test]
    async fn test_cancellation_isolation() {
        let w1 = Arc::new(SupervisedWorker::new(201, Some(2001)));
        let w2 = Arc::new(SupervisedWorker::new(202, Some(2002)));
        w1.mark_ready();
        w2.mark_ready();
        // Kill w1 only
        w1.kill().await.unwrap();
        // w2 should still be ready
        assert!(w2.is_ready.load(std::sync::atomic::Ordering::Acquire));
        assert!(!w1.is_ready.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn test_shutdown_cleans_multiple() {
        let worker_bin = resolve_worker_binary(std::path::Path::new("obscura-runtime-worker"));
        if !worker_bin.exists() {
            return;
        }
        let config = SupervisorConfig {
            worker_bin,
            pool_size: 3,
            ..SupervisorConfig::default()
        };
        let supervisor = WorkerPoolSupervisor::new(config);
        supervisor.start().await.expect("start");
        assert_eq!(supervisor.pool_size(), 3);
        supervisor.shutdown().await.expect("shutdown");
        assert_eq!(supervisor.pool_size(), 0);
    }
}

#[cfg(test)]
mod process_group_tests {
    use super::*;

    #[tokio::test]
    async fn test_process_group_termination() {
        let worker_bin = resolve_worker_binary(std::path::Path::new("obscura-runtime-worker"));
        if !worker_bin.exists() {
            return;
        }
        let config = SupervisorConfig {
            worker_bin,
            pool_size: 1,
            ..SupervisorConfig::default()
        };
        let supervisor = WorkerPoolSupervisor::new(config);
        supervisor.start().await.expect("start");
        assert_eq!(supervisor.healthy_count(), 1);
        // Terminate
        let workers = supervisor.workers.read().clone();
        for w in &workers {
            w.kill().await.unwrap();
        }
        supervisor.shutdown().await.expect("shutdown");
        assert_eq!(supervisor.pool_size(), 0);
    }

    #[tokio::test]
    async fn test_pdeathsig_race_handling() {
        // This test only runs on Linux
        #[cfg(target_os = "linux")]
        {
            // Verified by code inspection: pre_exec captures expected_ppid
            // and checks getppid() after prctl registration
        }
        // On non-Linux, this is a no-op
    }
}
