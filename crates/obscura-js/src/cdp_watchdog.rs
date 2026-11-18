//! Shared per-command V8 watchdog for the CDP server.
//!
//! One long-lived watchdog thread bounds every in-flight V8 command with a
//! deadline, instead of spawning and joining a thread per command (which adds
//! ~240us per command on the hot dispatch path). `arm` and `disarm` are a mutex
//! plus a condvar notify, in the low microseconds.
//!
//! With the thread-per-connection server (issue #430) several connections can
//! have a command armed at the same time (one isolate per connection, each on
//! its own OS thread), so a single global slot would let one connection's arm
//! overwrite another's and leave that command unbounded. The watchdog therefore
//! tracks a set of armed slots keyed by a monotonic generation, fires whichever
//! have overrun, and terminates each through its thread-safe `IsolateHandle`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::runtime::IsolateHandle;

const IDLE: u8 = 0;
const ENTERED: u8 = 1;
const INTERRUPTING: u8 = 2;
const REQUESTED: u8 = 3;

/// A page's watchdogs share an identity across runtime replacement. Connection
/// threads own their pages, so equal page names on other connections cannot match.
#[derive(Clone)]
pub struct ExecutionScope {
    owner: std::thread::ThreadId,
    page: Arc<str>,
    activity: Arc<AtomicU8>,
}

impl ExecutionScope {
    pub fn new(owner: std::thread::ThreadId, page: &str) -> Self {
        Self { owner, page: Arc::from(page), activity: Arc::new(AtomicU8::new(IDLE)) }
    }

    pub(crate) fn enter(&self) {
        // The central runtime guards hold an exclusive JsRuntime borrow, so
        // entries of this runtime cannot overlap or span an await.
        let previous = self.activity.swap(ENTERED, Ordering::SeqCst);
        debug_assert_eq!(previous, IDLE);
    }

    /// Leave synchronous V8 work. A requested interruption must finish before
    /// exit, then its pending V8 flag is cleared before another task can start.
    pub(crate) fn leave(&self) -> bool {
        loop {
            match self.activity.load(Ordering::SeqCst) {
                ENTERED => {
                    if self.activity.compare_exchange(ENTERED, IDLE, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                        return false;
                    }
                }
                INTERRUPTING => std::hint::spin_loop(),
                REQUESTED => { self.activity.store(IDLE, Ordering::SeqCst); return true; }
                _ => unreachable!("leaving an inactive runtime"),
            }
        }
    }

    fn reserve_interrupt(&self) -> Option<InterruptReservation> {
        let activity = self.activity.clone();
        activity.compare_exchange(ENTERED, INTERRUPTING, Ordering::SeqCst, Ordering::SeqCst).ok()?;
        Some(InterruptReservation(activity))
    }
}

// Always release the entry guard, including if interrupt delivery unwinds.
struct InterruptReservation(Arc<AtomicU8>);
impl Drop for InterruptReservation {
    fn drop(&mut self) { self.0.store(REQUESTED, Ordering::SeqCst); }
}

struct Slot {
    scope: Option<ExecutionScope>,
    deadline: Option<Instant>,
    handle: IsolateHandle,
    fired: Arc<AtomicBool>,
}

struct Shared {
    // (armed slots keyed by generation, monotonic generation counter)
    state: Mutex<(HashMap<u64, Slot>, u64)>,
    cv: Condvar,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| {
        let s = Arc::new(Shared {
            state: Mutex::new((HashMap::new(), 0)),
            cv: Condvar::new(),
        });
        let worker = s.clone();
        std::thread::Builder::new()
            .name("cdp-watchdog".into())
            .spawn(move || watchdog_loop(worker))
            .expect("spawn cdp watchdog");
        s
    })
}

fn watchdog_loop(s: Arc<Shared>) {
    let mut guard = s.state.lock().unwrap();
    loop {
        let now = Instant::now();
        // Terminate every slot that has overrun its deadline. The dispatcher's
        // `disarm` will observe `fired` and clear the V8 termination flag before
        // that isolate runs its next command.
        let expired: Vec<u64> = guard
            .0
            .iter()
            .filter(|(_, slot)| slot.deadline.is_some_and(|deadline| deadline <= now))
            .map(|(gen, _)| *gen)
            .collect();
        for gen in expired {
            if let Some(slot) = guard.0.remove(&gen) {
                slot.fired.store(true, Ordering::SeqCst);
                slot.handle.terminate_execution();
            }
        }
        // Sleep until the nearest remaining deadline, or until arm/disarm wakes
        // us. The worker holds the lock until it waits, so a notify cannot be
        // lost into the void.
        let next = guard
            .0
            .values()
            .filter_map(|slot| slot.deadline.map(|deadline| deadline.saturating_duration_since(now)))
            .min();
        guard = match next {
            None => s.cv.wait(guard).unwrap(),
            Some(dur) => s.cv.wait_timeout(guard, dur).unwrap().0,
        };
    }
}

/// Handle to an armed command; pass to [`disarm`].
pub struct Armed {
    gen: u64,
    fired: Arc<AtomicBool>,
}

/// Arm the shared watchdog for the current command. If the isolate is still
/// executing `budget` later, it is terminated. O(1), no thread spawn. Safe to
/// call concurrently from several connections: each command gets its own slot.
pub fn arm(handle: IsolateHandle, budget: Duration) -> Armed {
    arm_scoped(handle, Some(budget), None)
}

/// Register active execution for a page, with an optional automatic deadline.
/// A missing deadline still permits an explicit Runtime.terminateExecution.
pub fn arm_scoped(handle: IsolateHandle, budget: Option<Duration>, scope: Option<ExecutionScope>) -> Armed {
    let s = shared();
    let mut guard = s.state.lock().unwrap();
    guard.1 += 1;
    let gen = guard.1;
    let fired = Arc::new(AtomicBool::new(false));
    guard.0.insert(
        gen,
        Slot {
            scope,
            deadline: budget.map(|budget| Instant::now() + budget),
            handle,
            fired: fired.clone(),
        },
    );
    s.cv.notify_one();
    Armed { gen, fired }
}

/// Disarm the command's watchdog. Returns true if it had already fired
/// (terminated the isolate), in which case the caller must clear the V8
/// termination flag before the next command runs.
pub fn disarm(armed: Armed) -> bool {
    let s = shared();
    let mut guard = s.state.lock().unwrap();
    guard.0.remove(&armed.gen);
    // Wake the worker so it recomputes its sleep if we removed the nearest slot.
    s.cv.notify_one();
    armed.fired.load(Ordering::SeqCst)
}

/// Interrupt only this page's synchronous V8 entry. Whole-phase watchdogs
/// remain armed during I/O, but an idle runtime must not inherit termination.
pub fn interrupt(owner: std::thread::ThreadId, page: &str) {
    let s = shared();
    let guard = s.state.lock().unwrap();
    let active = guard.0.values().find_map(|slot| {
        let target = slot.scope.as_ref().filter(|target| target.owner == owner && target.page.as_ref() == page)?;
        Some((target.reserve_interrupt()?, slot.handle.clone()))
    });
    if let Some((reservation, handle)) = active {
        // Entry guards own manual recovery. Keep automatic deadlines armed:
        // a parser phase can continue into another script after this one.
        handle.terminate_execution();
        drop(reservation);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn interrupt_unwind_releases_the_runtime_entry() {
        let scope = super::ExecutionScope::new(std::thread::current().id(), "page");
        scope.enter();
        let failed = std::panic::catch_unwind(|| {
            let _reservation = scope.reserve_interrupt().unwrap();
            panic!("interrupt delivery failed");
        });
        assert!(failed.is_err());
        assert!(scope.leave());
        scope.enter();
        assert!(!scope.leave());
    }

    #[test]
    fn concurrent_interrupt_exit_leaves_the_next_entry_clean() {
        use std::sync::mpsc::{sync_channel, TryRecvError};
        use std::time::Duration;

        let scope = super::ExecutionScope::new(std::thread::current().id(), "page");
        scope.enter();
        let (reserved_tx, reserved_rx) = sync_channel(0);
        let (release_tx, release_rx) = sync_channel(0);
        let delivery_scope = scope.clone();
        let delivery = std::thread::spawn(move || {
            let reservation = delivery_scope.reserve_interrupt().unwrap();
            reserved_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            drop(reservation);
        });
        reserved_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (exiting_tx, exiting_rx) = sync_channel(0);
        let (finished_tx, finished_rx) = sync_channel(0);
        let exit_scope = scope.clone();
        let exit = std::thread::spawn(move || {
            exiting_tx.send(()).unwrap();
            finished_tx.send(exit_scope.leave()).unwrap();
        });
        exiting_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(scope.activity.load(super::Ordering::SeqCst), super::INTERRUPTING);
        assert!(matches!(finished_rx.try_recv(), Err(TryRecvError::Empty)));
        release_tx.send(()).unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        delivery.join().unwrap();
        exit.join().unwrap();

        scope.enter();
        assert!(!scope.leave());
    }

}
