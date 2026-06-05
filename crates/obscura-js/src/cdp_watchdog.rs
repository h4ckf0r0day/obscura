//! Shared per-command V8 watchdog for the CDP server.
//!
//! The CDP dispatcher holds a process-wide V8 lock around every V8-touching
//! command, so at most one command's isolate is ever executing at a time. That
//! lets a single long-lived watchdog thread bound the current command with a
//! deadline, instead of spawning+joining a thread per command (which adds
//! ~240us per command on the hot dispatch path). `arm` and `disarm` are just a
//! mutex + condvar notify, in the low microseconds.
//!
//! Correctness rests on the V8-lock serialization: between `arm` and `disarm`
//! the caller holds the V8 lock, so no other command can arm, and the single
//! slot is exclusively this command's.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::runtime::IsolateHandle;

struct Slot {
    deadline: Instant,
    handle: IsolateHandle,
    gen: u64,
    fired: Arc<AtomicBool>,
}

struct Shared {
    // (current armed slot, monotonic generation counter)
    state: Mutex<(Option<Slot>, u64)>,
    cv: Condvar,
}

static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

fn shared() -> &'static Arc<Shared> {
    SHARED.get_or_init(|| {
        let s = Arc::new(Shared {
            state: Mutex::new((None, 0)),
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
        let next_deadline = match &guard.0 {
            None => None,
            Some(slot) => {
                let now = Instant::now();
                if slot.deadline <= now {
                    // Overran: terminate this isolate and clear the slot. The
                    // dispatcher's disarm will observe `fired` and clear the V8
                    // termination flag before the next command.
                    slot.fired.store(true, Ordering::SeqCst);
                    slot.handle.terminate_execution();
                    guard.0 = None;
                    None
                } else {
                    Some(slot.deadline - now)
                }
            }
        };
        guard = match next_deadline {
            // No armed command: block until arm() notifies. The worker holds the
            // lock until it waits, so arm() (which needs the lock) cannot notify
            // into the void; no lost wakeup.
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
/// executing `budget` later, it is terminated. O(1), no thread spawn.
pub fn arm(handle: IsolateHandle, budget: Duration) -> Armed {
    let s = shared();
    let mut guard = s.state.lock().unwrap();
    guard.1 += 1;
    let gen = guard.1;
    let fired = Arc::new(AtomicBool::new(false));
    guard.0 = Some(Slot {
        deadline: Instant::now() + budget,
        handle,
        gen,
        fired: fired.clone(),
    });
    s.cv.notify_one();
    Armed { gen, fired }
}

/// Disarm the command's watchdog. Returns true if it had already fired
/// (terminated the isolate), in which case the caller must clear the V8
/// termination flag before the next command runs.
pub fn disarm(armed: Armed) -> bool {
    let s = shared();
    let mut guard = s.state.lock().unwrap();
    // Only clear the slot if it is still ours (a newer command would have a
    // higher gen, though the V8 lock prevents that overlap in practice).
    if guard.0.as_ref().map(|sl| sl.gen) == Some(armed.gen) {
        guard.0 = None;
        s.cv.notify_one();
    }
    armed.fired.load(Ordering::SeqCst)
}
