// Copyright 2018-2025 the Deno authors. MIT license.

use cooked_waker::IntoWaker;
use cooked_waker::ViaRawPointer;
use cooked_waker::Wake;
use cooked_waker::WakeRef;
use std::cell::Cell;
use std::cell::Ref;
use std::cell::RefCell;
use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::btree_set;
use std::future::Future;
use std::mem::MaybeUninit;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::task::ready;
use std::time::Duration;
use tokio::time::Instant;
use tokio::time::Sleep;

pub(crate) type WebTimerId = u64;

/// The minimum number of tombstones required to trigger compaction
const COMPACTION_MINIMUM: usize = 16;

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum TimerType {
  Repeat(NonZeroU64),
  Once,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey(Instant, u64, TimerType, bool);

struct TimerData<T> {
  data: T,
  unrefd: bool,
  #[cfg(any(windows, test))]
  high_res: bool,
  #[cfg(not(any(windows, test)))]
  high_res: (),
}

/// Implements much of the specification described by https://html.spec.whatwg.org/multipage/timers-and-user-prompts.html.
///
/// To ensure that we perform well in the face of large numbers of timers, this implementation assumes
/// that timers may complete in batches that are properly ordered according to the spec. That is to say, timers
/// are executed in an order according to the following specification:
///
/// > Wait until any invocations of this algorithm that had the same global and orderingIdentifier,
/// > that started before this one, and whose milliseconds is equal to or less than this one's,
/// > have completed.
///
/// This complicates timer resolution because timers fire in an order based on _milliseconds_ (aka their original
/// timeout duration) rather than an exact moment in time.
///
/// While we respect the spirit of this paragraph, we make an assumption that all timer callback invocations are
/// instantaneous. This means that we can assume that no further timers have elapsed during the execution of a batch of
/// timers. This does not always hold up in reality (for example, a poorly-written timer in a worker may block on
/// `Atomics.wait` or a synchronous `XMLHttpRequest`), but it allows us to process timers in a simpler and less-racy way.
///
/// We also assume that our underlying timers are high-resolution, and that our underlying timer source resolves in
/// a proper start time + expiration time order.
///
/// Finally, we assume that the event loop time -- the time between which we are able to poll this set of timers -- is
/// non-zero, and that multiple timers may fire during this period and require re-ordering by their original millisecond
/// timeouts.
///
///
/// https://github.com/denoland/deno/pull/12953 -- Add refTimer, unrefTimer API
/// https://github.com/denoland/deno/pull/12862 -- Refactor timers to use one async op per timer
///
/// https://github.com/denoland/deno/issues/11398 -- Spurious assertion error when the callback to setInterval lasts longer than the interval
pub(crate) struct WebTimers<T> {
  next_id: Cell<WebTimerId>,
  timers: RefCell<BTreeSet<TimerKey>>,
  /// We choose a `BTreeMap` over `HashMap` because of memory performance.
  data_map: RefCell<BTreeMap<WebTimerId, TimerData<T>>>,
  /// How many unref'd timers exist?
  unrefd_count: Cell<usize>,
  /// A boxed MutableSleep that will allow us to change the Tokio sleep timeout as needed.
  /// Because this is boxed, we can "safely" unsafely poll it.
  sleep: Box<MutableSleep>,
  /// The high-res timer lock. No-op on platforms other than Windows.
  high_res_timer_lock: HighResTimerLock,
}

impl<T> Default for WebTimers<T> {
  fn default() -> Self {
    Self {
      next_id: Default::default(),
      timers: Default::default(),
      data_map: Default::default(),
      unrefd_count: Default::default(),
      sleep: MutableSleep::new(),
      high_res_timer_lock: Default::default(),
    }
  }
}

pub(crate) struct WebTimersIterator<'a, T> {
  data: Ref<'a, BTreeMap<WebTimerId, TimerData<T>>>,
  timers: Ref<'a, BTreeSet<TimerKey>>,
}

impl<'a, T> IntoIterator for &'a WebTimersIterator<'a, T> {
  type IntoIter = WebTimersIteratorImpl<'a, T>;
  type Item = (u64, bool, bool);

  fn into_iter(self) -> Self::IntoIter {
    WebTimersIteratorImpl {
      data: &self.data,
      timers: self.timers.iter(),
    }
  }
}

pub(crate) struct WebTimersIteratorImpl<'a, T> {
  data: &'a BTreeMap<WebTimerId, TimerData<T>>,
  timers: btree_set::Iter<'a, TimerKey>,
}

impl<T> Iterator for WebTimersIteratorImpl<'_, T> {
  type Item = (u64, bool, bool);
  fn next(&mut self) -> Option<Self::Item> {
    loop {
      let item = self.timers.next()?;
      if self.data.contains_key(&item.1) {
        return Some((item.1, !matches!(item.2, TimerType::Once), item.3));
      }
    }
  }
}

struct MutableSleep {
  sleep: UnsafeCell<Option<Sleep>>,
  ready: Cell<bool>,
  external_waker: UnsafeCell<Option<Waker>>,
  internal_waker: Waker,
}

#[allow(clippy::borrowed_box)]
impl MutableSleep {
  fn new() -> Box<Self> {
    let mut new = Box::new(MaybeUninit::<Self>::uninit());

    new.write(MutableSleep {
      sleep: Default::default(),
      ready: Cell::default(),
      external_waker: UnsafeCell::default(),
      internal_waker: MutableSleepWaker {
        inner: new.as_ptr(),
      }
      .into_waker(),
    });
    unsafe { std::mem::transmute(new) }
  }

  fn poll_ready(self: &Box<Self>, cx: &mut Context) -> Poll<()> {
    if self.ready.take() {
      Poll::Ready(())
    } else {
      let external =
        unsafe { self.external_waker.get().as_mut().unwrap_unchecked() };
      if let Some(external) = external {
        // Already have this waker
        let waker = cx.waker();
        if !external.will_wake(waker) {
          external.clone_from(waker);
        }
        Poll::Pending
      } else {
        *external = Some(cx.waker().clone());
        Poll::Pending
      }
    }
  }

  fn clear(self: &Box<Self>) {
    unsafe {
      *self.sleep.get() = None;
    }
  }

  fn change(self: &Box<Self>, instant: Instant) {
    let pin = unsafe {
      // First replace the current sleep
      *self.sleep.get() = Some(tokio::time::sleep_until(instant));

      // Then get ourselves a Pin to this
      Pin::new_unchecked(
        self
          .sleep
          .get()
          .as_mut()
          .unwrap_unchecked()
          .as_mut()
          .unwrap_unchecked(),
      )
    };

    // Register our waker
    let waker = &self.internal_waker;
    if pin.poll(&mut Context::from_waker(waker)).is_ready() {
      self.ready.set(true);
      self.internal_waker.wake_by_ref();
    }
  }
}

#[repr(transparent)]
#[derive(Clone)]
struct MutableSleepWaker {
  inner: *const MutableSleep,
}

unsafe impl Send for MutableSleepWaker {}
unsafe impl Sync for MutableSleepWaker {}

impl WakeRef for MutableSleepWaker {
  fn wake_by_ref(&self) {
    unsafe {
      let this = self.inner.as_ref().unwrap_unchecked();
      this.ready.set(true);
      let waker = this.external_waker.get().as_mut().unwrap_unchecked();
      if let Some(waker) = waker.as_ref() {
        waker.wake_by_ref();
      }
    }
  }
}

impl Wake for MutableSleepWaker {
  fn wake(self) {
    self.wake_by_ref()
  }
}

impl Drop for MutableSleepWaker {
  fn drop(&mut self) {}
}

unsafe impl ViaRawPointer for MutableSleepWaker {
  type Target = ();

  fn into_raw(self) -> *mut () {
    self.inner as _
  }

  unsafe fn from_raw(ptr: *mut ()) -> Self {
    MutableSleepWaker { inner: ptr as _ }
  }
}

impl<T: Clone> WebTimers<T> {
  /// Returns an internal iterator that locks the internal data structures for the period
  /// of iteration. Calling other methods on this collection will cause a panic.
  pub(crate) fn iter(&self) -> WebTimersIterator<T> {
    WebTimersIterator {
      data: self.data_map.borrow(),
      timers: self.timers.borrow(),
    }
  }

  /// Refs a timer by ID. Invalid IDs are ignored.
  pub fn ref_timer(&self, id: WebTimerId) {
    if let Some(TimerData { unrefd, .. }) =
      self.data_map.borrow_mut().get_mut(&id)
    {
      if std::mem::replace(unrefd, false) {
        self.unrefd_count.set(self.unrefd_count.get() - 1);
      }
    }
  }

  /// Unrefs a timer by ID. Invalid IDs are ignored.
  pub fn unref_timer(&self, id: WebTimerId) {
    if let Some(TimerData { unrefd, .. }) =
      self.data_map.borrow_mut().get_mut(&id)
    {
      if !std::mem::replace(unrefd, true) {
        self.unrefd_count.set(self.unrefd_count.get() + 1);
      }
    }
  }

  /// Queues a timer to be fired in order with the other timers in this set of timers.
  pub fn queue_timer(&self, timeout_ms: u64, data: T) -> WebTimerId {
    self.queue_timer_internal(false, timeout_ms, data, false)
  }

  /// Queues a timer to be fired in order with the other timers in this set of timers.
  pub fn queue_timer_repeat(&self, timeout_ms: u64, data: T) -> WebTimerId {
    self.queue_timer_internal(true, timeout_ms, data, false)
  }

  pub fn queue_system_timer(
    &self,
    repeat: bool,
    timeout_ms: u64,
    data: T,
  ) -> WebTimerId {
    self.queue_timer_internal(repeat, timeout_ms, data, true)
  }

  fn queue_timer_internal(
    &self,
    repeat: bool,
    timeout_ms: u64,
    data: T,
    is_system_timer: bool,
  ) -> WebTimerId {
    #[allow(clippy::let_unit_value)]
    let high_res = self.high_res_timer_lock.maybe_lock(timeout_ms);

    let id = self.next_id.get() + 1;
    self.next_id.set(id);

    let mut timers = self.timers.borrow_mut();
    let deadline = Instant::now()
      .checked_add(Duration::from_millis(timeout_ms))
      .unwrap();
    match timers.first() {
      Some(TimerKey(k, ..)) => {
        if &deadline < k {
          self.sleep.change(deadline);
        }
      }
      _ => {
        self.sleep.change(deadline);
      }
    }

    let timer_type = if repeat {
      TimerType::Repeat(
        NonZeroU64::new(timeout_ms).unwrap_or(NonZeroU64::new(1).unwrap()),
      )
    } else {
      TimerType::Once
    };
    timers.insert(TimerKey(deadline, id, timer_type, is_system_timer));

    let mut data_map = self.data_map.borrow_mut();
    data_map.insert(
      id,
      TimerData {
        data,
        unrefd: false,
        high_res,
      },
    );
    id
  }

  /// Cancels a pending timer in this set of timers, returning the associated data if a timer
  /// with the given ID was found.
  pub fn cancel_timer(&self, timer: u64) -> Option<T> {
    let mut data_map = self.data_map.borrow_mut();
    match data_map.remove(&timer) {
      Some(TimerData {
        data,
        unrefd,
        high_res,
      }) => {
        if data_map.is_empty() {
          // When the # of running timers hits zero, clear the timer tree.
          // When debug assertions are enabled, we do a consistency check.
          debug_assert_eq!(self.unrefd_count.get(), if unrefd { 1 } else { 0 });
          #[cfg(any(windows, test))]
          debug_assert_eq!(self.high_res_timer_lock.is_locked(), high_res);
          self.high_res_timer_lock.clear();
          self.unrefd_count.set(0);
          self.timers.borrow_mut().clear();
          self.sleep.clear();
        } else {
          self.high_res_timer_lock.maybe_unlock(high_res);
          if unrefd {
            self.unrefd_count.set(self.unrefd_count.get() - 1);
          }
        }
        Some(data)
      }
      _ => None,
    }
  }

  /// Poll for any timers that have completed.
  pub fn poll_timers(&self, cx: &mut Context) -> Poll<Vec<(u64, T)>> {
    ready!(self.sleep.poll_ready(cx));
    let now = Instant::now();
    let mut timers = self.timers.borrow_mut();
    let mut data = self.data_map.borrow_mut();
    let mut output = vec![];

    let mut split = timers.split_off(&TimerKey(now, 0, TimerType::Once, false));
    std::mem::swap(&mut split, &mut timers);
    for TimerKey(_, id, timer_type, is_system_timer) in split {
      if let TimerType::Repeat(interval) = timer_type {
        if let Some(TimerData { data, .. }) = data.get(&id) {
          output.push((id, data.clone()));
          timers.insert(TimerKey(
            now
              .checked_add(Duration::from_millis(interval.into()))
              .unwrap(),
            id,
            timer_type,
            is_system_timer,
          ));
        }
      } else if let Some(TimerData {
        data,
        unrefd,
        high_res,
      }) = data.remove(&id)
      {
        self.high_res_timer_lock.maybe_unlock(high_res);
        if unrefd {
          self.unrefd_count.set(self.unrefd_count.get() - 1);
        }
        output.push((id, data));
      }
    }

    // In-effective poll, run a front-compaction and try again later
    if output.is_empty() {
      // We should never have an ineffective poll when the data map is empty, as we check
      // for this in cancel_timer.
      debug_assert!(!data.is_empty());
      while let Some(TimerKey(_, id, ..)) = timers.first() {
        if data.contains_key(id) {
          break;
        } else {
          timers.pop_first();
        }
      }
      if let Some(TimerKey(k, ..)) = timers.first() {
        self.sleep.change(*k);
      }
      return Poll::Pending;
    }

    if data.is_empty() {
      // When the # of running timers hits zero, clear the timer tree.
      if !timers.is_empty() {
        timers.clear();
        self.sleep.clear();
      }
    } else {
      // Run compaction when there are enough tombstones to justify cleanup.
      let tombstone_count = timers.len() - data.len();
      if tombstone_count > COMPACTION_MINIMUM {
        timers.retain(|k| data.contains_key(&k.1));
      }
      if let Some(TimerKey(k, ..)) = timers.first() {
        self.sleep.change(*k);
      }
    }

    Poll::Ready(output)
  }

  /// Is this set of timers empty?
  pub fn is_empty(&self) -> bool {
    self.data_map.borrow().is_empty()
  }

  /// The total number of timers in this collection.
  pub fn len(&self) -> usize {
    self.data_map.borrow().len()
  }

  /// The number of unref'd timers in this collection.
  pub fn unref_len(&self) -> usize {
    self.unrefd_count.get()
  }

  #[cfg(test)]
  pub fn assert_consistent(&self) {
    if self.data_map.borrow().is_empty() {
      // If the data map is empty, we should have no timers, no unref'd count, no high-res lock
      assert_eq!(self.timers.borrow().len(), 0);
      assert_eq!(self.unrefd_count.get(), 0);
      assert!(!self.high_res_timer_lock.is_locked());
    } else {
      assert!(self.unrefd_count.get() <= self.data_map.borrow().len());
      // The high-res lock count must be <= the number of remaining timers
      assert!(self.high_res_timer_lock.lock_count.get() <= self.len());
    }
  }

  pub fn has_pending_timers(&self) -> bool {
    self.len() > self.unref_len()
  }
}

#[cfg(windows)]
#[link(name = "winmm")]
unsafe extern "C" {
  fn timeBeginPeriod(n: u32);
  fn timeEndPeriod(n: u32);
}

#[derive(Default)]
struct HighResTimerLock {
  #[cfg(any(windows, test))]
  lock_count: Cell<usize>,
}

impl HighResTimerLock {
  /// If a timer is requested with <=100ms resolution, request the high-res timer. Since the default
  /// Windows timer period is 15ms, this means a 100ms timer could fire at 115ms (15% late). We assume that
  /// timers longer than 100ms are a reasonable cutoff here.
  ///
  /// The high-res timers on Windows are still limited. Unfortunately this means that our shortest duration 4ms timers
  /// can still be 25% late, but without a more complex timer system or spinning on the clock itself, we're somewhat
  /// bounded by the OS' scheduler itself.
  #[cfg(any(windows, test))]
  const LOW_RES_TIMER_RESOLUTION: u64 = 100;

  #[cfg(any(windows, test))]
  #[inline(always)]
  fn maybe_unlock(&self, high_res: bool) {
    if high_res {
      let old = self.lock_count.get();
      debug_assert!(old > 0);
      let new = old - 1;
      self.lock_count.set(new);
      #[cfg(windows)]
      if new == 0 {
        // SAFETY: Windows API
        unsafe {
          timeEndPeriod(1);
        }
      }
    }
  }

  #[cfg(not(any(windows, test)))]
  #[inline(always)]
  fn maybe_unlock(&self, _high_res: ()) {}

  #[cfg(any(windows, test))]
  #[inline(always)]
  fn maybe_lock(&self, timeout_ms: u64) -> bool {
    if timeout_ms <= Self::LOW_RES_TIMER_RESOLUTION {
      let old = self.lock_count.get();
      #[cfg(windows)]
      if old == 0 {
        // SAFETY: Windows API
        unsafe {
          timeBeginPeriod(1);
        }
      }
      self.lock_count.set(old + 1);
      true
    } else {
      false
    }
  }

  #[cfg(not(any(windows, test)))]
  #[inline(always)]
  fn maybe_lock(&self, _timeout_ms: u64) {}

  #[cfg(any(windows, test))]
  #[inline(always)]
  fn clear(&self) {
    #[cfg(windows)]
    if self.lock_count.get() > 0 {
      // SAFETY: Windows API
      unsafe {
        timeEndPeriod(1);
      }
    }

    self.lock_count.set(0);
  }

  #[cfg(not(any(windows, test)))]
  #[inline(always)]
  fn clear(&self) {}

  #[cfg(any(windows, test))]
  fn is_locked(&self) -> bool {
    self.lock_count.get() > 0
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;
  use std::future::poll_fn;

  /// Miri is way too slow here on some of the larger tests.
  const TEN_THOUSAND: u64 = if cfg!(miri) { 100 } else { 10_000 };

  /// Helper function to support miri + rstest. We cannot use I/O in a miri test.
  fn async_test<F: Future<Output = T>, T>(f: F) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_time()
      .build()
      .unwrap();
    runtime.block_on(f)
  }

  async fn poll_all(timers: &WebTimers<()>) -> Vec<(u64, ())> {
    timers.assert_consistent();
    let len = timers.len();
    let mut v = vec![];
    while !timers.is_empty() {
      let mut batch = poll_fn(|cx| {
        timers.assert_consistent();
        timers.poll_timers(cx)
      })
      .await;
      v.append(&mut batch);
      #[allow(clippy::print_stderr)]
      {
        eprintln!(
          "{} ({} {})",
          v.len(),
          timers.len(),
          timers.data_map.borrow().len(),
        );
      }
      timers.assert_consistent();
    }
    assert_eq!(v.len(), len);
    v
  }

  /// This test attempts to mimic a memory leak fix in the timer compaction logic.
  /// See https://github.com/denoland/deno/issues/27925
  ///
  /// The leak happens when there are enough tombstones to justify cleanup
  /// (tombstone_count > COMPACTION_MINIMUM) but there are also more active timers
  /// than tombstones (tombstone_count <= data.len()). In this scenario, the original
  /// condition won't trigger compaction, allowing tombstones to accumulate.
  #[test]
  fn test_timer_tombstone_memory_leak() {
    const ACTIVE_TIMERS: usize = 100;
    const TOMBSTONES: usize = 30; // > COMPACTION_MINIMUM but < ACTIVE_TIMERS
    const CLEANUP_THRESHOLD: usize = 5; // Threshold to determine if compaction happened
    async_test(async {
      let timers = WebTimers::<()>::default();

      // Create mostly long-lived timers, with a few immediate ones
      // The immediate timers ensure poll_timers returns non-empty output
      // which prevents the front-compaction mechanism from cleaning up tombstones
      let mut active_timer_ids = Vec::with_capacity(ACTIVE_TIMERS);
      for i in 0..ACTIVE_TIMERS {
        let timeout = if i < CLEANUP_THRESHOLD { 1 } else { 10000 };
        active_timer_ids.push(timers.queue_timer(timeout, ()));
      }

      // Create and immediately cancel timers to generate tombstones
      for _ in 0..TOMBSTONES {
        let id = timers.queue_timer(10000, ());
        timers.cancel_timer(id);
      }

      let count_tombstones =
        || timers.timers.borrow().len() - timers.data_map.borrow().len();
      let initial_tombstones = count_tombstones();

      // Verify test setup is correct
      assert!(
        initial_tombstones > COMPACTION_MINIMUM,
        "Test requires tombstones > COMPACTION_MINIMUM"
      );
      assert!(
        initial_tombstones <= ACTIVE_TIMERS,
        "Test requires tombstones <= active_timers"
      );

      // Poll timers to trigger potential compaction
      let _ = poll_fn(|cx| timers.poll_timers(cx)).await;

      let remaining_tombstones = count_tombstones();

      for id in active_timer_ids {
        timers.cancel_timer(id);
      }

      assert!(
        remaining_tombstones < CLEANUP_THRESHOLD,
        "Memory leak: Tombstones not cleaned up"
      );
    });
  }

  #[test]
  fn test_timer() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let _a = timers.queue_timer(1, ());

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 1);
    });
  }

  #[test]
  fn test_high_res_lock() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      assert!(!timers.high_res_timer_lock.is_locked());
      let _a = timers.queue_timer(1, ());
      assert!(timers.high_res_timer_lock.is_locked());

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 1);
      assert!(!timers.high_res_timer_lock.is_locked());
    });
  }

  #[rstest]
  #[test]
  fn test_timer_cancel_1(#[values(0, 1, 2, 3)] which: u64) {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..4 {
        let id = timers.queue_timer(i * 25, ());
        if i == which {
          assert!(timers.cancel_timer(id).is_some());
        }
      }
      assert_eq!(timers.len(), 3);

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 3);
    })
  }

  #[rstest]
  #[test]
  fn test_timer_cancel_2(#[values(0, 1, 2)] which: u64) {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..4 {
        let id = timers.queue_timer(i * 25, ());
        if i == which || i == which + 1 {
          assert!(timers.cancel_timer(id).is_some());
        }
      }
      assert_eq!(timers.len(), 2);

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 2);
    })
  }

  #[test]
  fn test_timers_10_random() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..10 {
        timers.queue_timer((i % 3) * 10, ());
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 10);
    })
  }

  #[test]
  fn test_timers_10_random_cancel() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..10 {
        let id = timers.queue_timer((i % 3) * 10, ());
        timers.cancel_timer(id);
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 0);
    });
  }

  #[rstest]
  #[test]
  fn test_timers_10_random_cancel_after(#[values(true, false)] reverse: bool) {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let mut ids = vec![];
      for i in 0..2 {
        ids.push(timers.queue_timer((i % 3) * 10, ()));
      }
      if reverse {
        ids.reverse();
      }
      for id in ids {
        timers.cancel_timer(id);
        timers.assert_consistent();
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 0);
    });
  }

  #[test]
  fn test_timers_10() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for _i in 0..10 {
        timers.queue_timer(1, ());
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 10);
    });
  }

  #[test]
  fn test_timers_10_000_random() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      for i in 0..TEN_THOUSAND {
        timers.queue_timer(i % 10, ());
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), TEN_THOUSAND as usize);
    });
  }

  /// Cancel a large number of timers at the head of the queue to trigger
  /// a front compaction.
  #[test]
  fn test_timers_cancel_first() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let mut ids = vec![];
      for _ in 0..TEN_THOUSAND {
        ids.push(timers.queue_timer(1, ()));
      }
      for i in 0..10 {
        timers.queue_timer(i * 25, ());
      }
      for id in ids {
        timers.cancel_timer(id);
      }
      let v = poll_all(&timers).await;
      assert_eq!(v.len(), 10);
    });
  }

  #[test]
  fn test_timers_10_000_cancel_most() {
    async_test(async {
      let timers = WebTimers::<()>::default();
      let mut ids = vec![];
      for i in 0..TEN_THOUSAND {
        ids.push(timers.queue_timer(i % 100, ()));
      }

      // This should trigger a compaction
      fastrand::seed(42);
      ids.retain(|_| fastrand::u8(0..10) > 0);
      for id in ids.iter() {
        timers.cancel_timer(*id);
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), TEN_THOUSAND as usize - ids.len());
    });
  }

  #[rstest]
  #[test]
  fn test_chaos(#[values(42, 99, 1000)] seed: u64) {
    async_test(async {
      let timers = WebTimers::<()>::default();
      fastrand::seed(seed);

      let mut count = 0;
      let mut ref_count = 0;

      for _ in 0..TEN_THOUSAND {
        let mut cancelled = false;
        let mut unrefed = false;
        let id = timers.queue_timer(fastrand::u64(0..10), ());
        for _ in 0..fastrand::u64(0..10) {
          if fastrand::u8(0..10) == 0 {
            timers.cancel_timer(id);
            cancelled = true;
          }
          if fastrand::u8(0..10) == 0 {
            timers.ref_timer(id);
            unrefed = false;
          }
          if fastrand::u8(0..10) == 0 {
            timers.unref_timer(id);
            unrefed = true;
          }
        }

        if !cancelled {
          count += 1;
        }
        if !unrefed {
          ref_count += 1;
        }

        timers.assert_consistent();
      }

      #[allow(clippy::print_stderr)]
      {
        eprintln!("count={count} ref_count={ref_count}");
      }

      let v = poll_all(&timers).await;
      assert_eq!(v.len(), count);

      assert!(timers.is_empty());
      assert!(!timers.has_pending_timers());
    });
  }
}
