//! Supervision tree, self-healing process lifecycle, and pool routing for Obscura Next workers.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::Notify;

use obscura_telemetry::global_metrics;

#[derive(Error, Debug)]
pub enum SupervisorError {
    #[error("Worker spawn failure: {0}")]
    SpawnFailed(String),

    #[error("No healthy workers available")]
    NoWorkersAvailable,

    #[error("Worker {0} unresponsive to heartbeat")]
    HeartbeatTimeout(usize),

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
    pub fn should_recycle(&self, sessions: usize, rss_mb: Option<u64>, age: Duration) -> Option<String> {
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

/// High-level descriptor for a worker in the pool.
pub struct SupervisedWorker {
    pub id: usize,
    pub pid: Option<u32>,
    pub spawned_at: Instant,
    pub status: RwLock<WorkerStatus>,
    pub active_sessions: AtomicUsize,
    pub total_sessions_served: AtomicUsize,
    pub consecutive_failures: AtomicUsize,
    pub is_ready: AtomicBool,
    pub wake_notify: Arc<Notify>,
}

impl SupervisedWorker {
    pub fn new(id: usize, pid: Option<u32>) -> Self {
        Self {
            id,
            pid,
            spawned_at: Instant::now(),
            status: RwLock::new(WorkerStatus::Starting),
            active_sessions: AtomicUsize::new(0),
            total_sessions_served: AtomicUsize::new(0),
            consecutive_failures: AtomicUsize::new(0),
            is_ready: AtomicBool::new(false),
            wake_notify: Arc::new(Notify::new()),
        }
    }

    pub fn mark_ready(&self) {
        *self.status.write() = WorkerStatus::Ready;
        self.is_ready.store(true, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    pub fn mark_draining(&self) {
        *self.status.write() = WorkerStatus::Draining;
        self.is_ready.store(false, Ordering::Release);
    }

    pub fn record_failure(&self) -> usize {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn acquire_session(&self) {
        self.active_sessions.fetch_add(1, Ordering::Relaxed);
        self.total_sessions_served.fetch_add(1, Ordering::Relaxed);
    }

    pub fn release_session(&self) {
        self.active_sessions.fetch_sub(1, Ordering::Relaxed);
        self.wake_notify.notify_one();
    }
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
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            worker_bin: PathBuf::from("obscura-runtime-worker"),
            pool_size: 4,
            heartbeat_interval: Duration::from_secs(5),
            heartbeat_timeout: Duration::from_secs(3),
            recycle_policy: WorkerRecyclePolicy::default(),
            restart_backoff_base: Duration::from_millis(200),
        }
    }
}

/// Worker pool supervisor implementing self-healing and load distribution.
pub struct WorkerPoolSupervisor {
    config: SupervisorConfig,
    workers: Arc<RwLock<Vec<Arc<SupervisedWorker>>>>,
    round_robin_idx: AtomicUsize,
}

impl WorkerPoolSupervisor {
    pub fn new(config: SupervisorConfig) -> Self {
        Self {
            config,
            workers: Arc::new(RwLock::new(Vec::new())),
            round_robin_idx: AtomicUsize::new(0),
        }
    }

    pub fn register_worker(&self, worker: Arc<SupervisedWorker>) {
        self.workers.write().push(worker);
        let metrics = global_metrics();
        if let Some(gauge) = metrics.gauge("obscura_worker_processes_count") {
            gauge.set(self.workers.read().len() as i64);
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
        assert!(policy.should_recycle(5, Some(200), Duration::from_secs(30)).is_none());

        // Session limit exceeded
        let reason = policy.should_recycle(10, Some(200), Duration::from_secs(30)).unwrap();
        assert!(reason.contains("session limit"));

        // Memory limit exceeded
        let reason = policy.should_recycle(5, Some(600), Duration::from_secs(30)).unwrap();
        assert!(reason.contains("RSS memory limit"));

        // Age limit exceeded
        let reason = policy.should_recycle(5, Some(200), Duration::from_secs(65)).unwrap();
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
}
