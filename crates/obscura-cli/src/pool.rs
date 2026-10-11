//! Worker pool state for the multi-worker `serve` load balancer: routing,
//! per-worker connection accounting, and the recycle policy.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// When a worker should be replaced. Every limit is off by default.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecyclePolicy {
    pub max_connections: Option<usize>,
    pub max_rss_mb: Option<u64>,
    pub max_age: Option<Duration>,
}

impl RecyclePolicy {
    pub fn enabled(&self) -> bool {
        self.max_connections.is_some() || self.max_rss_mb.is_some() || self.max_age.is_some()
    }

    /// The first threshold that is exceeded, if any. `rss_mb` is `None` when it
    /// could not be sampled (non-Linux, process gone), which never recycles.
    pub fn reason(&self, served: usize, rss_mb: Option<u64>, age: Duration) -> Option<String> {
        if let Some(max) = self.max_connections {
            if served >= max {
                return Some(format!("served {served} connections (limit {max})"));
            }
        }
        if let (Some(max), Some(rss)) = (self.max_rss_mb, rss_mb) {
            if rss >= max {
                return Some(format!("rss {rss} MB (limit {max} MB)"));
            }
        }
        if let Some(max) = self.max_age {
            if age >= max {
                return Some(format!("age {}s (limit {}s)", age.as_secs(), max.as_secs()));
            }
        }
        None
    }
}

/// Resident set size of `pid` in MB. Linux only; `None` elsewhere.
pub fn rss_mb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb / 1024)
}

/// One worker process generation: the port it listens on and its live load.
pub struct Worker {
    pub port: u16,
    pub born: Instant,
    ready: AtomicBool,
    active: AtomicUsize,
    served: AtomicUsize,
    max_connections: Option<usize>,
    wake: Arc<Notify>,
}

impl Worker {
    pub fn new(port: u16, max_connections: Option<usize>, wake: Arc<Notify>) -> Arc<Self> {
        Arc::new(Worker {
            port,
            born: Instant::now(),
            ready: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            served: AtomicUsize::new(0),
            max_connections,
            wake,
        })
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Relaxed);
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    pub fn served(&self) -> usize {
        self.served.load(Ordering::Relaxed)
    }

    /// Live proxied connection; released on drop.
    pub fn connection(self: &Arc<Self>) -> Connection {
        self.active.fetch_add(1, Ordering::Relaxed);
        Connection(self.clone())
    }
}

pub struct Connection(Arc<Worker>);

impl Connection {
    pub fn port(&self) -> u16 {
        self.0.port
    }

    /// Count this as a CDP session (not a /json discovery request) and wake
    /// the supervisor when the connection limit is reached.
    pub fn count_session(&self) {
        let served = self.0.served.fetch_add(1, Ordering::Relaxed) + 1;
        if self.0.max_connections.is_some_and(|max| served >= max) {
            self.0.wake.notify_one();
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One pool position. It always holds the worker that takes new connections;
/// a replaced worker is no longer reachable from here, which is what drains it.
pub struct Slot {
    current: Mutex<Arc<Worker>>,
    pub wake: Arc<Notify>,
}

impl Slot {
    pub fn new(first: Arc<Worker>, wake: Arc<Notify>) -> Arc<Self> {
        Arc::new(Slot { current: Mutex::new(first), wake })
    }

    pub fn current(&self) -> Arc<Worker> {
        self.current.lock().unwrap().clone()
    }

    /// Route new connections to `next`; returns the worker that now drains.
    pub fn replace(&self, next: Arc<Worker>) -> Arc<Worker> {
        std::mem::replace(&mut *self.current.lock().unwrap(), next)
    }
}

/// Round-robin over ready workers, skipping crashed ones.
pub fn pick(slots: &[Arc<Slot>], next: &mut usize) -> Option<Connection> {
    (0..slots.len()).find_map(|_| {
        let worker = slots[*next % slots.len()].current();
        *next = next.wrapping_add(1);
        worker.ready.load(Ordering::Relaxed).then(|| worker.connection())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn default_policy_never_recycles() {
        let policy = RecyclePolicy::default();
        assert!(!policy.enabled());
        assert!(policy.reason(usize::MAX, Some(u64::MAX), S * 100_000).is_none());
    }

    #[test]
    fn thresholds_trigger_at_the_limit() {
        let policy = RecyclePolicy {
            max_connections: Some(3),
            max_rss_mb: Some(500),
            max_age: Some(S * 60),
        };
        assert!(policy.reason(2, Some(499), S * 59).is_none());
        assert!(policy.reason(3, Some(0), S).unwrap().contains("connections"));
        assert!(policy.reason(0, Some(500), S).unwrap().contains("rss"));
        assert!(policy.reason(0, Some(0), S * 60).unwrap().contains("age"));
        // An unreadable RSS never triggers the RSS limit.
        assert!(policy.reason(0, None, S).is_none());
    }

    fn pool(n: u16, max: Option<usize>) -> Vec<Arc<Slot>> {
        (0..n)
            .map(|i| {
                let wake = Arc::new(Notify::new());
                Slot::new(Worker::new(9000 + i, max, wake.clone()), wake)
            })
            .collect()
    }

    #[test]
    fn pick_round_robins_and_skips_unready() {
        let slots = pool(3, None);
        let mut next = 0;
        let ports: Vec<u16> = (0..3).map(|_| pick(&slots, &mut next).unwrap().port()).collect();
        assert_eq!(ports, [9000, 9001, 9002]);
        slots[1].current().set_ready(false);
        for _ in 0..6 {
            assert_ne!(pick(&slots, &mut next).unwrap().port(), 9001);
        }
        for slot in &slots {
            slot.current().set_ready(false);
        }
        assert!(pick(&slots, &mut next).is_none());
    }

    #[test]
    fn replaced_worker_drains_and_gets_no_new_connections() {
        let slots = pool(2, Some(2));
        let mut next = 0;
        let held = pick(&slots, &mut next).unwrap();
        let old = slots[0].current();
        assert_eq!(old.active(), 1);

        let wake = slots[0].wake.clone();
        let fresh = Worker::new(9100, Some(2), wake);
        let draining = slots[0].replace(fresh);
        assert!(Arc::ptr_eq(&draining, &old));
        for _ in 0..8 {
            assert_ne!(pick(&slots, &mut next).unwrap().port(), 9000);
        }
        // The live connection keeps the old worker busy until it finishes.
        assert_eq!(draining.active(), 1);
        drop(held);
        assert_eq!(draining.active(), 0);
        // Capacity stays at two ready slots.
        assert_eq!(slots.len(), 2);
        assert!(slots.iter().all(|slot| slot.current().port != 9000));
    }

    #[test]
    fn session_count_wakes_supervisor_at_limit_only() {
        let slots = pool(1, Some(2));
        let mut next = 0;
        let first = pick(&slots, &mut next).unwrap();
        first.count_session();
        let wake = slots[0].wake.clone();
        // Not woken yet: a stored permit would complete the notified future.
        assert!(notified_now(&wake).is_none());
        let second = pick(&slots, &mut next).unwrap();
        second.count_session();
        assert!(notified_now(&wake).is_some());
        assert_eq!(slots[0].current().served(), 2);
        // Discovery connections are active but never counted.
        let discovery = pick(&slots, &mut next).unwrap();
        assert_eq!(slots[0].current().active(), 3);
        drop(discovery);
        assert_eq!(slots[0].current().served(), 2);
    }

    fn notified_now(wake: &Notify) -> Option<()> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_millis(20), wake.notified()).await.ok()
        })
    }

    #[test]
    fn rss_of_self_is_readable_on_linux() {
        if cfg!(target_os = "linux") {
            assert!(rss_mb(std::process::id()).unwrap() > 0);
        } else {
            assert!(rss_mb(std::process::id()).is_none());
        }
    }
}
