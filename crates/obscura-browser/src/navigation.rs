use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use obscura_js::runtime::IsolateHandle;

struct RegisteredIsolate {
    generation: u64,
    handle: IsolateHandle,
}

struct IsolateSlot {
    next_generation: u64,
    registered: Option<RegisteredIsolate>,
}

struct NavigationControlInner {
    cancelled: AtomicBool,
    isolate: Mutex<IsolateSlot>,
    notify: tokio::sync::Notify,
}

/// Internal cancellation handle used by protocol navigation owners.
///
/// This is not part of the stable embeddable browser API. Cancellation is
/// sticky: a runtime registered after cancellation is terminated immediately.
#[doc(hidden)]
#[derive(Clone)]
pub struct NavigationControl {
    inner: Arc<NavigationControlInner>,
}

impl NavigationControl {
    #[doc(hidden)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(NavigationControlInner {
                cancelled: AtomicBool::new(false),
                isolate: Mutex::new(IsolateSlot {
                    next_generation: 0,
                    registered: None,
                }),
                notify: tokio::sync::Notify::new(),
            }),
        }
    }

    #[doc(hidden)]
    pub fn cancel(&self) {
        let first = !self.inner.cancelled.swap(true, Ordering::AcqRel);
        if first {
            self.inner.notify.notify_waiters();
        }
        let slot = self
            .inner
            .isolate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Keep registration, termination, and registration drop linearized.
        // IsolateHandle::terminate_execution only requests termination; the
        // owner thread unwinds and drops its registration after this returns.
        if let Some(registered) = slot.registered.as_ref() {
            registered.handle.terminate_execution();
        }
    }

    #[doc(hidden)]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    #[doc(hidden)]
    pub async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        tokio::pin!(notified);
        // Register the waiter before checking the sticky bit, closing the gap
        // between an AtomicBool check and Notify subscription.
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    pub(crate) fn register_runtime(&self, handle: IsolateHandle) -> RuntimeRegistration {
        let mut slot = self
            .inner
            .isolate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slot.next_generation = slot
            .next_generation
            .checked_add(1)
            .expect("navigation isolate generation overflow");
        let generation = slot.next_generation;
        slot.registered = Some(RegisteredIsolate {
            generation,
            handle: handle.clone(),
        });
        if self.is_cancelled() {
            handle.terminate_execution();
        }
        drop(slot);
        RuntimeRegistration {
            inner: self.inner.clone(),
            generation,
        }
    }

    #[cfg(test)]
    fn registered_generation(&self) -> Option<u64> {
        self.inner
            .isolate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .registered
            .as_ref()
            .map(|registered| registered.generation)
    }
}

impl Default for NavigationControl {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct RuntimeRegistration {
    inner: Arc<NavigationControlInner>,
    generation: u64,
}

impl Drop for RuntimeRegistration {
    fn drop(&mut self) {
        let mut slot = self
            .inner
            .isolate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .registered
            .as_ref()
            .is_some_and(|registered| registered.generation == self.generation)
        {
            slot.registered = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::NavigationControl;
    use obscura_js::runtime::ObscuraJsRuntime;

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_is_sticky_for_existing_and_late_waiters() {
        let control = NavigationControl::new();
        control.cancel();
        control.cancel();
        assert!(control.is_cancelled());
        control.cancelled().await;
    }

    #[test]
    fn cancellation_before_registration_terminates_the_runtime() {
        let control = NavigationControl::new();
        control.cancel();
        let mut runtime = ObscuraJsRuntime::new();
        let _registration = control.register_runtime(runtime.isolate_handle());
        assert!(runtime.execute_script("<cancelled>", "globalThis.ran = true;").is_err());
        runtime.cancel_termination();
        assert!(runtime.execute_script("<recovered>", "globalThis.ran = true;").is_ok());
    }

    #[test]
    fn stale_registration_cannot_clear_replacement() {
        let control = NavigationControl::new();
        let runtime_a = ObscuraJsRuntime::new();
        let mut runtime_b = ObscuraJsRuntime::new();
        let registration_a = control.register_runtime(runtime_a.isolate_handle());
        let generation_a = control.registered_generation().unwrap();
        let registration_b = control.register_runtime(runtime_b.isolate_handle());
        let generation_b = control.registered_generation().unwrap();
        assert!(generation_b > generation_a);
        drop(registration_a);
        assert_eq!(control.registered_generation(), Some(generation_b));
        control.cancel();
        control.cancel();
        assert!(runtime_b.execute_script("<new>", "globalThis.newRan = true;").is_err());
        runtime_b.cancel_termination();
        drop(registration_b);
        assert_eq!(control.registered_generation(), None);
    }
}
