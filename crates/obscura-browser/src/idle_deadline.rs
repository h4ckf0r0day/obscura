//! Preserve runtime polling while avoiding a coalesced final idle-deadline wake.

use std::future::Future;
use tokio::time::Instant;

pub(crate) async fn wait_for_activity(
    deadline: Instant,
    final_check: bool,
    activity: impl Future,
) {
    #[cfg(target_os = "macos")]
    if final_check {
        let mut activity = std::pin::pin!(activity);
        let mut wake = None;
        let mut attempted = false;
        let activity_or_deadline = std::future::poll_fn(|cx| {
            // Keep the existing runtime poll first, even if both wakes are
            // ready. Never replace page work with a timer-only completion.
            if activity.as_mut().poll(cx).is_ready() {
                return std::task::Poll::Ready(());
            }
            if !attempted {
                attempted = true;
                wake = macos::PreciseWake::new(deadline.into_std());
            }
            if wake.as_mut().is_some_and(|wake| wake.poll(cx)) {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        });
        // Retain the original timeout if timer creation fails, the platform
        // delays dispatch, or Tokio's test clock advances independently.
        let _ = tokio::time::timeout_at(deadline, activity_or_deadline).await;
        return;
    }
    let _ = final_check;
    let _ = tokio::time::timeout_at(deadline, activity).await;
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::future::Future;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::pin::Pin;
    use std::ptr::NonNull;
    use std::task::{Context, Poll};
    use std::time::Instant;
    use tokio::sync::oneshot;

    // C function APIs avoid Objective-C blocks and any extra runtime crate.
    // These signatures and lifetime rules follow dispatch/source.h and
    // dispatch/object.h. Only the address of the opaque type symbol is used.
    #[link(name = "System")]
    unsafe extern "C" {
        static _dispatch_source_type_timer: u8;
        fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
        fn dispatch_source_create(kind: *const c_void, handle: usize, mask: usize,
            queue: *mut c_void) -> *mut c_void;
        fn dispatch_set_context(object: *mut c_void, context: *mut c_void);
        fn dispatch_source_set_event_handler_f(source: *mut c_void, handler: extern "C" fn(*mut c_void));
        fn dispatch_source_set_cancel_handler_f(source: *mut c_void, handler: extern "C" fn(*mut c_void));
        fn dispatch_source_set_timer(source: *mut c_void, start: u64, interval: u64, leeway: u64);
        fn dispatch_time(when: u64, delta: i64) -> u64;
        fn dispatch_resume(object: *mut c_void);
        fn dispatch_source_cancel(source: *mut c_void);
        fn dispatch_release(object: *mut c_void);
    }

    const TIMER_STRICT: usize = 1;
    const TIME_NOW: u64 = 0;
    const TIME_FOREVER: u64 = u64::MAX;

    struct Callback {
        source: NonNull<c_void>,
        deadline: Instant,
        sender: Option<oneshot::Sender<()>>,
    }

    #[cfg(test)]
    static LIVE_CALLBACKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    #[cfg(test)]
    static EARLY_WAKES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[cfg(test)]
    impl Drop for Callback {
        fn drop(&mut self) {
            LIVE_CALLBACKS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn arm(source: NonNull<c_void>, deadline: Instant) {
        let nanos = deadline.saturating_duration_since(Instant::now()).as_nanos()
            .min(i64::MAX as u128) as i64;
        // SAFETY: the caller owns a live source or runs its event handler.
        // This is a one-shot uptime timer; no periodic wake remains armed.
        unsafe {
            dispatch_source_set_timer(source.as_ptr(), dispatch_time(TIME_NOW, nanos), TIME_FOREVER, 0);
        }
    }

    extern "C" fn fired(context: *mut c_void) {
        // SAFETY: dispatch serializes this source's callbacks. The Box remains
        // owned by dispatch until cancelled() runs after this handler returns.
        let callback = unsafe { &mut *context.cast::<Callback>() };
        if Instant::now() < callback.deadline {
            #[cfg(test)]
            EARLY_WAKES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            arm(callback.source, callback.deadline);
            return;
        }
        // A Rust waker must not unwind through a C callback boundary. The
        // original Tokio timeout remains available if notification fails.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if let Some(sender) = callback.sender.take() {
                let _ = sender.send(());
            }
        }));
        // SAFETY: the source retains its handler until this callback returns;
        // cancellation is asynchronous and does not join the page/V8 thread.
        unsafe { dispatch_source_cancel(callback.source.as_ptr()); }
    }

    extern "C" fn cancelled(context: *mut c_void) {
        // SAFETY: this runs once, after the final event handler has returned.
        // It is the sole owner that reconstructs and frees the callback Box.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(context.cast::<Callback>()));
        }));
    }

    pub(super) struct PreciseWake {
        source: NonNull<c_void>,
        receiver: Option<oneshot::Receiver<()>>,
    }

    impl PreciseWake {
        pub(super) fn new(deadline: Instant) -> Option<Self> {
            if deadline <= Instant::now() {
                return None;
            }
            // Strict timers can override coalescing intended to save power.
            // Use one only for a parked final readiness check, never for all
            // polling slices, and never create a dedicated waiting thread.
            // SAFETY: the source is inactive until context and both handlers
            // are installed. The returned owned reference belongs to Self.
            let source = unsafe {
                NonNull::new(dispatch_source_create(
                    (&raw const _dispatch_source_type_timer).cast(), 0, TIMER_STRICT,
                    dispatch_get_global_queue(0, 0),
                ))?
            };
            let (sender, receiver) = oneshot::channel();
            let callback = Box::new(Callback { source, deadline, sender: Some(sender) });
            #[cfg(test)]
            LIVE_CALLBACKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            unsafe {
                dispatch_set_context(source.as_ptr(), Box::into_raw(callback).cast());
                dispatch_source_set_event_handler_f(source.as_ptr(), fired);
                dispatch_source_set_cancel_handler_f(source.as_ptr(), cancelled);
                arm(source, deadline);
                dispatch_resume(source.as_ptr());
            }
            Some(Self { source, receiver: Some(receiver) })
        }

        pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> bool {
            let Some(receiver) = &mut self.receiver else { return false; };
            if let Poll::Ready(result) = Pin::new(receiver).poll(cx) {
                self.receiver = None;
                return result.is_ok();
            }
            false
        }
    }

    impl Drop for PreciseWake {
        fn drop(&mut self) {
            // Unregister the waiter before asynchronous cancellation can drop
            // the sender. Cancelled page work must not receive a stray wake.
            drop(self.receiver.take());
            // SAFETY: Self owns the create reference; callbacks are retained
            // by dispatch until cancellation completes and free their context.
            unsafe {
                dispatch_source_cancel(self.source.as_ptr());
                dispatch_release(self.source.as_ptr());
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        use std::task::{Wake, Waker};
        use std::time::Duration;

        async fn drained() {
            tokio::time::timeout(Duration::from_secs(2), async {
                while LIVE_CALLBACKS.load(Ordering::SeqCst) != 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }).await.expect("cancelled timer context must be released");
        }

        #[tokio::test(flavor = "current_thread")]
        async fn precise_idle_timer_reaches_deadline_without_early_readiness() {
            let deadline = Instant::now() + Duration::from_millis(20);
            let mut wake = PreciseWake::new(deadline).unwrap();
            tokio::time::timeout(Duration::from_secs(2), std::future::poll_fn(|cx| {
                if wake.poll(cx) { Poll::Ready(()) } else { Poll::Pending }
            })).await.expect("dispatch timer must notify independently of the fallback");
            assert!(Instant::now() >= deadline);
            drop(wake);
            drained().await;
        }

        #[tokio::test(flavor = "current_thread")]
        async fn precise_idle_timer_rechecks_and_rearms_an_early_event() {
            let mut wake = PreciseWake::new(Instant::now() + Duration::from_secs(5)).unwrap();
            // Exercise the real C handler with an intentionally early event.
            unsafe { dispatch_source_set_timer(wake.source.as_ptr(), TIME_NOW, TIME_FOREVER, 0); }
            tokio::time::timeout(Duration::from_secs(2), async {
                while EARLY_WAKES.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }).await.expect("early event must reach the deadline recheck");
            assert!(!wake.poll(&mut Context::from_waker(Waker::noop())));
            drop(wake);
            drained().await;
        }

        struct CountWakes(AtomicUsize);
        impl Wake for CountWakes {
            fn wake(self: Arc<Self>) { self.0.fetch_add(1, Ordering::SeqCst); }
        }

        #[tokio::test(flavor = "current_thread")]
        async fn precise_idle_timer_cancellation_releases_context_without_stray_wakes() {
            let count = Arc::new(CountWakes(AtomicUsize::new(0)));
            let waker = Waker::from(Arc::clone(&count));
            for _ in 0..32 {
                let mut wake = PreciseWake::new(Instant::now() + Duration::from_secs(5)).unwrap();
                assert!(!wake.poll(&mut Context::from_waker(&waker)));
                drop(wake);
            }
            drained().await;
            assert_eq!(count.0.load(Ordering::SeqCst), 0);
        }

        struct PanickingWake(AtomicUsize);
        impl Wake for PanickingWake {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("deliberately panicking test waker");
            }
        }

        #[tokio::test(flavor = "current_thread")]
        async fn precise_idle_timer_contains_waker_panics_at_the_c_boundary() {
            let count = Arc::new(PanickingWake(AtomicUsize::new(0)));
            let waker = Waker::from(Arc::clone(&count));
            let mut wake = PreciseWake::new(Instant::now() + Duration::from_millis(20)).unwrap();
            assert!(!wake.poll(&mut Context::from_waker(&waker)));
            drained().await;
            assert_eq!(count.0.load(Ordering::SeqCst), 1);
            drop(wake);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn precise_idle_activity_is_polled_before_creating_a_timer() {
            let mut polled = false;
            super::super::wait_for_activity(tokio::time::Instant::now() + Duration::from_secs(5), true,
                std::future::poll_fn(|_| { polled = true; Poll::Ready(()) })).await;
            assert!(polled);
            assert_eq!(LIVE_CALLBACKS.load(Ordering::SeqCst), 0);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn precise_idle_wake_keeps_the_tokio_deadline_fallback() {
            tokio::time::pause();
            let started = tokio::time::Instant::now();
            let mut wait = Box::pin(super::super::wait_for_activity(
                started + Duration::from_millis(25), true, std::future::pending::<()>()));
            let mut cx = Context::from_waker(Waker::noop());
            assert!(wait.as_mut().poll(&mut cx).is_pending());
            tokio::time::advance(Duration::from_millis(24)).await;
            assert!(wait.as_mut().poll(&mut cx).is_pending());
            tokio::time::advance(Duration::from_millis(2)).await;
            assert!(wait.as_mut().poll(&mut cx).is_ready());
            drop(wait);
            tokio::time::resume();
            drained().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn idle_intermediate_check_keeps_the_existing_timeout() {
        tokio::time::pause();
        let started = Instant::now();
        let mut wait = Box::pin(wait_for_activity(started + Duration::from_millis(50), false,
            std::future::pending::<()>()));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        tokio::time::advance(Duration::from_millis(49)).await;
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        tokio::time::advance(Duration::from_millis(2)).await;
        assert!(wait.as_mut().poll(&mut cx).is_ready());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_deadline_wait_preserves_ready_activity_and_cancellation() {
        let mut polled = false;
        wait_for_activity(Instant::now(), true,
            std::future::poll_fn(|_| { polled = true; std::task::Poll::Ready(()) })).await;
        assert!(polled, "an expired deadline must still poll page activity first");
        let (sender, mut receiver) = tokio::sync::oneshot::channel::<()>();
        wait_for_activity(Instant::now(), true, &mut receiver).await;
        sender.send(()).expect("cancelling the wait must not consume unrelated work");
        receiver.await.unwrap();
    }
}
