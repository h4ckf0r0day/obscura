// Copyright 2018-2025 the Deno authors. MIT license.

//! The documentation for the inspector API is sparse, but these are helpful:
//! <https://chromedevtools.github.io/devtools-protocol/>
//! <https://hyperandroid.com/2020/02/12/v8-inspector-from-an-embedder-standpoint/>

use crate::error::CoreError;
use crate::futures::channel::mpsc;
use crate::futures::channel::mpsc::UnboundedReceiver;
use crate::futures::channel::mpsc::UnboundedSender;
use crate::futures::channel::oneshot;
use crate::futures::future::Either;
use crate::futures::future::select;
use crate::futures::prelude::*;
use crate::futures::stream::SelectAll;
use crate::futures::stream::StreamExt;
use crate::futures::task;
use crate::serde_json::Value;
use crate::serde_json::json;
use deno_error::JsErrorBox;
use parking_lot::Mutex;
use std::cell::BorrowMutError;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::mem::take;
use std::pin::Pin;
use std::ptr;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::thread;
use v8::HandleScope;

pub enum InspectorMsgKind {
  Notification,
  Message(i32),
}

pub struct InspectorMsg {
  pub kind: InspectorMsgKind,
  pub content: String,
}

pub type SessionProxySender = UnboundedSender<InspectorMsg>;
pub type SessionProxyReceiver = UnboundedReceiver<String>;

/// Encapsulates an UnboundedSender/UnboundedReceiver pair that together form
/// a duplex channel for sending/receiving messages in V8 session.
pub struct InspectorSessionProxy {
  pub tx: SessionProxySender,
  pub rx: SessionProxyReceiver,
  pub options: InspectorSessionOptions,
}

type InspectorSessionSend = Box<dyn Fn(InspectorMsg)>;

#[derive(Clone, Copy)]
enum PollState {
  Idle,
  Woken,
  Polling,
  Parked,
  Dropped,
}

/// This structure is used responsible for providing inspector interface
/// to the `JsRuntime`.
///
/// It stores an instance of `v8::inspector::V8Inspector` and additionally
/// implements `v8::inspector::V8InspectorClientImpl`.
///
/// After creating this structure it's possible to connect multiple sessions
/// to the inspector, in case of Deno it's either: a "websocket session" that
/// provides integration with Chrome Devtools, or an "in-memory session" that
/// is used for REPL or coverage collection.
pub struct JsRuntimeInspector {
  v8_inspector_client: v8::inspector::V8InspectorClientBase,
  v8_inspector: Rc<RefCell<v8::UniquePtr<v8::inspector::V8Inspector>>>,
  new_session_tx: UnboundedSender<InspectorSessionProxy>,
  sessions: RefCell<SessionContainer>,
  flags: RefCell<InspectorFlags>,
  waker: Arc<InspectorWaker>,
  deregister_tx: Option<oneshot::Sender<()>>,
  is_dispatching_message: Rc<RefCell<bool>>,
  isolate_ptr: *mut v8::Isolate,
  context: v8::Global<v8::Context>,
}

impl Drop for JsRuntimeInspector {
  fn drop(&mut self) {
    // Since the waker is cloneable, it might outlive the inspector itself.
    // Set the poll state to 'dropped' so it doesn't attempt to request an
    // interrupt from the isolate.
    self.waker.update(|w| w.poll_state = PollState::Dropped);

    // V8 automatically deletes all sessions when an `V8Inspector` instance is
    // deleted, however InspectorSession also has a drop handler that cleans
    // up after itself. To avoid a double free, make sure the inspector is
    // dropped last.
    self.sessions.borrow_mut().drop_sessions();

    // Notify counterparty that this instance is being destroyed. Ignoring
    // result because counterparty waiting for the signal might have already
    // dropped the other end of channel.
    if let Some(deregister_tx) = self.deregister_tx.take() {
      let _ = deregister_tx.send(());
    }
  }
}

impl v8::inspector::V8InspectorClientImpl for JsRuntimeInspector {
  fn base(&self) -> &v8::inspector::V8InspectorClientBase {
    &self.v8_inspector_client
  }

  unsafe fn base_ptr(
    this: *const Self,
  ) -> *const v8::inspector::V8InspectorClientBase
  where
    Self: Sized,
  {
    // SAFETY: this pointer is valid for the whole lifetime of inspector
    unsafe { std::ptr::addr_of!((*this).v8_inspector_client) }
  }

  fn base_mut(&mut self) -> &mut v8::inspector::V8InspectorClientBase {
    &mut self.v8_inspector_client
  }

  fn run_message_loop_on_pause(&mut self, context_group_id: i32) {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    self.flags.borrow_mut().on_pause = true;
    let _ = self.poll_sessions(None);
  }

  fn quit_message_loop_on_pause(&mut self) {
    self.flags.borrow_mut().on_pause = false;
  }

  fn run_if_waiting_for_debugger(&mut self, context_group_id: i32) {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    self.flags.borrow_mut().waiting_for_session = false;
  }

  fn ensure_default_context_in_group(
    &mut self,
    context_group_id: i32,
  ) -> Option<v8::Local<v8::Context>> {
    assert_eq!(context_group_id, JsRuntimeInspector::CONTEXT_GROUP_ID);
    let isolate: &mut v8::Isolate = unsafe { &mut *self.isolate_ptr };
    let scope = &mut unsafe { v8::CallbackScope::new(isolate) };
    Some(v8::Local::new(scope, self.context.clone()))
  }

  fn resource_name_to_url(
    &mut self,
    resource_name: &v8::inspector::StringView,
  ) -> Option<v8::UniquePtr<v8::inspector::StringBuffer>> {
    let resource_name = resource_name.to_string();
    let url = url::Url::from_file_path(resource_name).ok()?;
    let src_view = v8::inspector::StringView::from(url.as_str().as_bytes());
    Some(v8::inspector::StringBuffer::create(src_view))
  }
}

impl JsRuntimeInspector {
  /// Currently Deno supports only a single context in `JsRuntime`
  /// and thus it's id is provided as an associated constant.
  const CONTEXT_GROUP_ID: i32 = 1;

  pub fn new(
    isolate_ptr: *mut v8::Isolate,
    scope: &mut v8::HandleScope,
    context: v8::Local<v8::Context>,
    is_main_runtime: bool,
  ) -> Rc<RefCell<Self>> {
    let (new_session_tx, new_session_rx) =
      mpsc::unbounded::<InspectorSessionProxy>();

    let v8_inspector_client =
      v8::inspector::V8InspectorClientBase::new::<Self>();

    let waker = InspectorWaker::new(scope.thread_safe_handle());

    // Create JsRuntimeInspector instance.
    let self__ = Rc::new(RefCell::new(Self {
      v8_inspector_client,
      v8_inspector: Default::default(),
      sessions: RefCell::new(SessionContainer::temporary_placeholder()),
      new_session_tx,
      flags: Default::default(),
      waker,
      deregister_tx: None,
      is_dispatching_message: Default::default(),
      isolate_ptr,
      context: v8::Global::new(scope, context),
    }));
    let mut self_ = self__.borrow_mut();
    self_.v8_inspector = Rc::new(RefCell::new(
      v8::inspector::V8Inspector::create(scope, &mut *self_).into(),
    ));
    self_.sessions = RefCell::new(SessionContainer::new(
      self_.v8_inspector.clone(),
      new_session_rx,
    ));

    // Tell the inspector about the main realm.
    let context_name = v8::inspector::StringView::from(&b"main realm"[..]);
    // NOTE(bartlomieju): this is what Node.js does and it turns out some
    // debuggers (like VSCode) rely on this information to disconnect after
    // program completes
    let aux_data = if is_main_runtime {
      r#"{"isDefault": true}"#
    } else {
      r#"{"isDefault": false}"#
    };
    let aux_data_view = v8::inspector::StringView::from(aux_data.as_bytes());
    self_
      .v8_inspector
      .borrow_mut()
      .as_mut()
      .unwrap()
      .context_created(
        context,
        Self::CONTEXT_GROUP_ID,
        context_name,
        aux_data_view,
      );

    // Poll the session handler so we will get notified whenever there is
    // new incoming debugger activity.
    let _ = self_.poll_sessions(None).unwrap();
    drop(self_);

    self__
  }

  pub fn is_dispatching_message(&self) -> bool {
    *self.is_dispatching_message.borrow()
  }

  pub fn context_destroyed(
    &mut self,
    scope: &mut HandleScope,
    context: v8::Global<v8::Context>,
  ) {
    let context = v8::Local::new(scope, context);
    self
      .v8_inspector
      .borrow_mut()
      .as_mut()
      .unwrap()
      .context_destroyed(context);
  }

  pub fn exception_thrown(
    &self,
    scope: &mut HandleScope,
    exception: v8::Local<'_, v8::Value>,
    in_promise: bool,
  ) {
    let context = scope.get_current_context();
    let message = v8::Exception::create_message(scope, exception);
    let stack_trace = message.get_stack_trace(scope);
    let mut v8_inspector_ref = self.v8_inspector.borrow_mut();
    let v8_inspector = v8_inspector_ref.as_mut().unwrap();
    let stack_trace = v8_inspector.create_stack_trace(stack_trace);
    v8_inspector.exception_thrown(
      context,
      if in_promise {
        v8::inspector::StringView::from("Uncaught (in promise)".as_bytes())
      } else {
        v8::inspector::StringView::from("Uncaught".as_bytes())
      },
      exception,
      v8::inspector::StringView::from("".as_bytes()),
      v8::inspector::StringView::from("".as_bytes()),
      0,
      0,
      stack_trace,
      0,
    );
  }

  pub fn sessions_state(&self) -> SessionsState {
    self.sessions.borrow().sessions_state()
  }

  pub fn poll_sessions(
    &self,
    mut invoker_cx: Option<&mut Context>,
  ) -> Result<Poll<()>, BorrowMutError> {
    // The futures this function uses do not have re-entrant poll() functions.
    // However it is can happen that poll_sessions() gets re-entered, e.g.
    // when an interrupt request is honored while the inspector future is polled
    // by the task executor. We let the caller know by returning some error.
    let mut sessions = self.sessions.try_borrow_mut()?;

    self.waker.update(|w| {
      match w.poll_state {
        PollState::Idle | PollState::Woken => w.poll_state = PollState::Polling,
        _ => unreachable!(),
      };
    });

    // Create a new Context object that will make downstream futures
    // use the InspectorWaker when they are ready to be polled again.
    let waker_ref = task::waker_ref(&self.waker);
    let cx = &mut Context::from_waker(&waker_ref);

    loop {
      loop {
        // Do one "handshake" with a newly connected session at a time.
        if let Some(mut session) = sessions.handshake.take() {
          let poll_result = session.poll_next_unpin(cx);
          match poll_result {
            Poll::Pending => {
              sessions.established.push(session);
              continue;
            }
            Poll::Ready(Some(())) => {
              sessions.established.push(session);
              continue;
            }
            Poll::Ready(None) => {}
          }
        }

        // Accept new connections.
        let poll_result = sessions.session_rx.poll_next_unpin(cx);
        if let Poll::Ready(Some(session_proxy)) = poll_result {
          let session = InspectorSession::new(
            sessions.v8_inspector.clone(),
            self.is_dispatching_message.clone(),
            Box::new(move |msg| {
              let _ = session_proxy.tx.unbounded_send(msg);
            }),
            session_proxy.rx,
            session_proxy.options,
          );
          let prev = sessions.handshake.replace(session);
          assert!(prev.is_none());
        }

        // Poll established sessions.
        match sessions.established.poll_next_unpin(cx) {
          Poll::Ready(Some(())) => {
            continue;
          }
          Poll::Ready(None) => break,
          Poll::Pending => break,
        };
      }

      let should_block =
        self.flags.borrow().on_pause || self.flags.borrow().waiting_for_session;

      let new_state = self.waker.update(|w| {
        match w.poll_state {
          PollState::Woken => {
            // The inspector was woken while the session handler was being
            // polled, so we poll it another time.
            w.poll_state = PollState::Polling;
          }
          PollState::Polling if !should_block => {
            // The session handler doesn't need to be polled any longer, and
            // there's no reason to block (execution is not paused), so this
            // function is about to return.
            w.poll_state = PollState::Idle;
            // Register the task waker that can be used to wake the parent
            // task that will poll the inspector future.
            if let Some(cx) = invoker_cx.take() {
              w.task_waker.replace(cx.waker().clone());
            }
            // Register the address of the inspector, which allows the waker
            // to request an interrupt from the isolate.
            w.inspector_ptr = NonNull::new(self as *const _ as *mut Self);
          }
          PollState::Polling if should_block => {
            // Isolate execution has been paused but there are no more
            // events to process, so this thread will be parked. Therefore,
            // store the current thread handle in the waker so it knows
            // which thread to unpark when new events arrive.
            w.poll_state = PollState::Parked;
            w.parked_thread.replace(thread::current());
          }
          _ => unreachable!(),
        };
        w.poll_state
      });
      match new_state {
        PollState::Idle => break Ok(Poll::Pending), // Yield to task.
        PollState::Polling => {} // Poll the session handler again.
        PollState::Parked => thread::park(), // Park the thread.
        _ => unreachable!(),
      };
    }
  }

  /// This function blocks the thread until at least one inspector client has
  /// established a websocket connection.
  pub fn wait_for_session(&mut self) {
    loop {
      match self.sessions.get_mut().established.iter_mut().next() {
        Some(_session) => {
          self.flags.get_mut().waiting_for_session = false;
          break;
        }
        None => {
          self.flags.get_mut().waiting_for_session = true;
          let _ = self.poll_sessions(None).unwrap();
        }
      };
    }
  }

  /// This function blocks the thread until at least one inspector client has
  /// established a websocket connection.
  ///
  /// After that, it instructs V8 to pause at the next statement.
  /// Frontend must send "Runtime.runIfWaitingForDebugger" message to resume
  /// execution.
  pub fn wait_for_session_and_break_on_next_statement(&mut self) {
    loop {
      match self.sessions.get_mut().established.iter_mut().next() {
        Some(session) => break session.break_on_next_statement(),
        None => {
          self.flags.get_mut().waiting_for_session = true;
          let _ = self.poll_sessions(None).unwrap();
        }
      };
    }
  }

  /// Obtain a sender for proxy channels.
  pub fn get_session_sender(&self) -> UnboundedSender<InspectorSessionProxy> {
    self.new_session_tx.clone()
  }

  /// Create a channel that notifies the frontend when inspector is dropped.
  ///
  /// NOTE: Only a single handler is currently available.
  pub fn add_deregister_handler(&mut self) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel::<()>();
    let prev = self.deregister_tx.replace(tx);
    assert!(
      prev.is_none(),
      "Only a single deregister handler is allowed"
    );
    rx
  }

  pub fn create_raw_session(
    &self,
    options: InspectorSessionOptions,
    send: InspectorSessionSend,
  ) -> UnboundedSender<String> {
    // The 'inbound' channel carries messages received from the session.
    let (inbound_tx, inbound_rx) = mpsc::unbounded();

    // InspectorSessions for a local session is added directly to the "established"
    // sessions, so it doesn't need to go through the session sender.
    let inspector_session = InspectorSession::new(
      self.v8_inspector.clone(),
      self.is_dispatching_message.clone(),
      send,
      inbound_rx,
      options,
    );

    self
      .sessions
      .borrow_mut()
      .established
      .push(inspector_session);
    take(&mut self.flags.borrow_mut().waiting_for_session);

    inbound_tx
  }

  /// Create a local inspector session that can be used on
  /// the same thread as the isolate.
  pub fn create_local_session(
    &self,
    options: InspectorSessionOptions,
  ) -> LocalInspectorSession {
    // The 'outbound' channel carries messages sent to the session.
    let (outbound_tx, outbound_rx) = mpsc::unbounded();

    let receive = Box::new(move |msg| {
      let _ = outbound_tx.unbounded_send(msg);
    });

    let inbound_tx = self.create_raw_session(options, receive);

    LocalInspectorSession::new(inbound_tx, outbound_rx)
  }
}

#[derive(Debug)]
pub struct InspectorSessionOptions {
  pub kind: InspectorSessionKind,
}

#[derive(Default)]
struct InspectorFlags {
  waiting_for_session: bool,
  on_pause: bool,
}

#[derive(Debug)]
pub struct SessionsState {
  pub has_active: bool,
  pub has_blocking: bool,
  pub has_nonblocking: bool,
  pub has_nonblocking_wait_for_disconnect: bool,
}

/// A helper structure that helps coordinate sessions during different
/// parts of their lifecycle.
struct SessionContainer {
  v8_inspector: Rc<RefCell<v8::UniquePtr<v8::inspector::V8Inspector>>>,
  session_rx: UnboundedReceiver<InspectorSessionProxy>,
  handshake: Option<Box<InspectorSession>>,
  established: SelectAll<Box<InspectorSession>>,
}

impl SessionContainer {
  fn new(
    v8_inspector: Rc<RefCell<v8::UniquePtr<v8::inspector::V8Inspector>>>,
    new_session_rx: UnboundedReceiver<InspectorSessionProxy>,
  ) -> Self {
    Self {
      v8_inspector,
      session_rx: new_session_rx,
      handshake: None,
      established: SelectAll::new(),
    }
  }

  /// V8 automatically deletes all sessions when an `V8Inspector` instance is
  /// deleted, however InspectorSession also has a drop handler that cleans
  /// up after itself. To avoid a double free, we need to manually drop
  /// all sessions before dropping the inspector instance.
  fn drop_sessions(&mut self) {
    self.v8_inspector = Default::default();
    self.handshake.take();
    self.established.clear();
  }

  fn sessions_state(&self) -> SessionsState {
    SessionsState {
      has_active: !self.established.is_empty() || self.handshake.is_some(),
      has_blocking: self
        .established
        .iter()
        .any(|s| matches!(s.kind, InspectorSessionKind::Blocking)),
      has_nonblocking: self
        .established
        .iter()
        .any(|s| matches!(s.kind, InspectorSessionKind::NonBlocking { .. })),
      has_nonblocking_wait_for_disconnect: self.established.iter().any(|s| {
        matches!(
          s.kind,
          InspectorSessionKind::NonBlocking {
            wait_for_disconnect: true
          }
        )
      }),
    }
  }

  /// A temporary placeholder that should be used before actual
  /// instance of V8Inspector is created. It's used in favor
  /// of `Default` implementation to signal that it's not meant
  /// for actual use.
  fn temporary_placeholder() -> Self {
    let (_tx, rx) = mpsc::unbounded::<InspectorSessionProxy>();
    Self {
      v8_inspector: Default::default(),
      session_rx: rx,
      handshake: None,
      established: SelectAll::new(),
    }
  }
}

struct InspectorWakerInner {
  poll_state: PollState,
  task_waker: Option<task::Waker>,
  parked_thread: Option<thread::Thread>,
  inspector_ptr: Option<NonNull<JsRuntimeInspector>>,
  isolate_handle: v8::IsolateHandle,
}

// SAFETY: unsafe trait must have unsafe implementation
unsafe impl Send for InspectorWakerInner {}

struct InspectorWaker(Mutex<InspectorWakerInner>);

impl InspectorWaker {
  fn new(isolate_handle: v8::IsolateHandle) -> Arc<Self> {
    let inner = InspectorWakerInner {
      poll_state: PollState::Idle,
      task_waker: None,
      parked_thread: None,
      inspector_ptr: None,
      isolate_handle,
    };
    Arc::new(Self(Mutex::new(inner)))
  }

  fn update<F, R>(&self, update_fn: F) -> R
  where
    F: FnOnce(&mut InspectorWakerInner) -> R,
  {
    let mut g = self.0.lock();
    update_fn(&mut g)
  }
}

impl task::ArcWake for InspectorWaker {
  fn wake_by_ref(arc_self: &Arc<Self>) {
    arc_self.update(|w| {
      match w.poll_state {
        PollState::Idle => {
          // Wake the task, if any, that has polled the Inspector future last.
          if let Some(waker) = w.task_waker.take() {
            waker.wake()
          }
          // Request an interrupt from the isolate if it's running and there's
          // not unhandled interrupt request in flight.
          if let Some(arg) = w
            .inspector_ptr
            .take()
            .map(|ptr| ptr.as_ptr() as *mut c_void)
          {
            w.isolate_handle.request_interrupt(handle_interrupt, arg);
          }
          extern "C" fn handle_interrupt(
            _isolate: &mut v8::Isolate,
            arg: *mut c_void,
          ) {
            // SAFETY: `InspectorWaker` is owned by `JsRuntimeInspector`, so the
            // pointer to the latter is valid as long as waker is alive.
            let inspector = unsafe { &*(arg as *mut JsRuntimeInspector) };
            let _ = inspector.poll_sessions(None);
          }
        }
        PollState::Parked => {
          // Unpark the isolate thread.
          let parked_thread = w.parked_thread.take().unwrap();
          assert_ne!(parked_thread.id(), thread::current().id());
          parked_thread.unpark();
        }
        _ => {}
      };
      w.poll_state = PollState::Woken;
    });
  }
}

#[derive(Debug)]
pub enum InspectorSessionKind {
  Blocking,
  NonBlocking { wait_for_disconnect: bool },
}

/// An inspector session that proxies messages to concrete "transport layer",
/// eg. Websocket or another set of channels.
struct InspectorSession {
  is_dispatching_message: Rc<RefCell<bool>>,
  v8_channel: v8::inspector::ChannelBase,
  v8_session: v8::UniqueRef<v8::inspector::V8InspectorSession>,
  send: InspectorSessionSend,
  rx: SessionProxyReceiver,
  // Describes if session should keep event loop alive, eg. a local REPL
  // session should keep event loop alive, but a Websocket session shouldn't.
  kind: InspectorSessionKind,
}

impl InspectorSession {
  const CONTEXT_GROUP_ID: i32 = 1;

  pub fn new(
    v8_inspector_rc: Rc<RefCell<v8::UniquePtr<v8::inspector::V8Inspector>>>,
    is_dispatching_message: Rc<RefCell<bool>>,
    send: InspectorSessionSend,
    rx: SessionProxyReceiver,
    options: InspectorSessionOptions,
  ) -> Box<Self> {
    new_box_with(move |self_ptr| {
      let v8_channel = v8::inspector::ChannelBase::new::<Self>();
      let mut v8_inspector = v8_inspector_rc.borrow_mut();
      let v8_inspector_ptr = v8_inspector.as_mut().unwrap();
      // TODO(piscisaureus): safety comment
      #[allow(clippy::undocumented_unsafe_blocks)]
      let v8_session = v8_inspector_ptr.connect(
        Self::CONTEXT_GROUP_ID,
        // Todo(piscisaureus): V8Inspector::connect() should require that
        // the 'v8_channel' argument cannot move.
        unsafe { &mut *self_ptr },
        v8::inspector::StringView::empty(),
        v8::inspector::V8InspectorClientTrustLevel::FullyTrusted,
      );

      Self {
        is_dispatching_message,
        v8_channel,
        v8_session,
        send,
        rx,
        kind: options.kind,
      }
    })
  }

  // Dispatch message to V8 session
  fn dispatch_message(&mut self, msg: String) {
    *self.is_dispatching_message.borrow_mut() = true;
    let msg = v8::inspector::StringView::from(msg.as_bytes());
    self.v8_session.dispatch_protocol_message(msg);
    *self.is_dispatching_message.borrow_mut() = false;
  }

  fn send_message(
    &self,
    msg_kind: InspectorMsgKind,
    msg: v8::UniquePtr<v8::inspector::StringBuffer>,
  ) {
    let msg = msg.unwrap().string().to_string();
    (self.send)(InspectorMsg {
      kind: msg_kind,
      content: msg,
    });
  }

  pub fn break_on_next_statement(&mut self) {
    let reason = v8::inspector::StringView::from(&b"debugCommand"[..]);
    let detail = v8::inspector::StringView::empty();
    self
      .v8_session
      .schedule_pause_on_next_statement(reason, detail);
  }
}

impl v8::inspector::ChannelImpl for InspectorSession {
  fn base(&self) -> &v8::inspector::ChannelBase {
    &self.v8_channel
  }

  unsafe fn base_ptr(this: *const Self) -> *const v8::inspector::ChannelBase
  where
    Self: Sized,
  {
    // SAFETY: this pointer is valid for the whole lifetime of inspector
    unsafe { std::ptr::addr_of!((*this).v8_channel) }
  }

  fn base_mut(&mut self) -> &mut v8::inspector::ChannelBase {
    &mut self.v8_channel
  }

  fn send_response(
    &mut self,
    call_id: i32,
    message: v8::UniquePtr<v8::inspector::StringBuffer>,
  ) {
    self.send_message(InspectorMsgKind::Message(call_id), message);
  }

  fn send_notification(
    &mut self,
    message: v8::UniquePtr<v8::inspector::StringBuffer>,
  ) {
    self.send_message(InspectorMsgKind::Notification, message);
  }

  fn flush_protocol_notifications(&mut self) {}
}

impl Stream for InspectorSession {
  type Item = ();

  fn poll_next(
    self: Pin<&mut Self>,
    cx: &mut Context,
  ) -> Poll<Option<Self::Item>> {
    let inner = self.get_mut();
    if let Poll::Ready(maybe_msg) = inner.rx.poll_next_unpin(cx) {
      if let Some(msg) = maybe_msg {
        inner.dispatch_message(msg);
        return Poll::Ready(Some(()));
      } else {
        return Poll::Ready(None);
      }
    }

    Poll::Pending
  }
}

/// A local inspector session that can be used to send and receive protocol messages directly on
/// the same thread as an isolate.
pub struct LocalInspectorSession {
  v8_session_tx: UnboundedSender<String>,
  v8_session_rx: UnboundedReceiver<InspectorMsg>,
  response_tx_map: HashMap<i32, oneshot::Sender<serde_json::Value>>,
  next_message_id: i32,
  notification_tx: UnboundedSender<Value>,
  notification_rx: Option<UnboundedReceiver<Value>>,
}

impl LocalInspectorSession {
  pub fn new(
    v8_session_tx: UnboundedSender<String>,
    v8_session_rx: UnboundedReceiver<InspectorMsg>,
  ) -> Self {
    let response_tx_map = HashMap::new();
    let next_message_id = 0;

    let (notification_tx, notification_rx) = mpsc::unbounded::<Value>();

    Self {
      v8_session_tx,
      v8_session_rx,
      response_tx_map,
      next_message_id,
      notification_tx,
      notification_rx: Some(notification_rx),
    }
  }

  pub fn take_notification_rx(&mut self) -> UnboundedReceiver<Value> {
    self.notification_rx.take().unwrap()
  }

  pub async fn post_message<T: serde::Serialize>(
    &mut self,
    method: &str,
    params: Option<T>,
  ) -> Result<serde_json::Value, CoreError> {
    let id = self.next_message_id;
    self.next_message_id += 1;

    let (response_tx, mut response_rx) =
      oneshot::channel::<serde_json::Value>();
    self.response_tx_map.insert(id, response_tx);

    let message = json!({
        "id": id,
        "method": method,
        "params": params,
    });

    let stringified_msg = serde_json::to_string(&message).unwrap();
    self.v8_session_tx.unbounded_send(stringified_msg).unwrap();

    loop {
      let receive_fut = self.receive_from_v8_session().boxed_local();
      match select(receive_fut, &mut response_rx).await {
        Either::Left(_) => continue,
        Either::Right((result, _)) => {
          let response = result?;
          if let Some(error) = response.get("error") {
            return Err(JsErrorBox::generic(error.to_string()).into());
          }

          let result = response.get("result").unwrap().clone();
          return Ok(result);
        }
      }
    }
  }

  pub async fn receive_from_v8_session(&mut self) {
    let inspector_msg = self.v8_session_rx.next().await.unwrap();
    if let InspectorMsgKind::Message(msg_id) = inspector_msg.kind {
      let message: serde_json::Value =
        match serde_json::from_str(&inspector_msg.content) {
          Ok(v) => v,
          Err(error) => match error.classify() {
            serde_json::error::Category::Syntax => json!({
              "id": msg_id,
              "result": {
                "result": {
                  "type": "error",
                  "description": "Unterminated string literal",
                  "value": "Unterminated string literal",
                },
                "exceptionDetails": {
                  "exceptionId": 0,
                  "text": "Unterminated string literal",
                  "lineNumber": 0,
                  "columnNumber": 0
                },
              },
            }),
            _ => panic!("Could not parse inspector message"),
          },
        };

      self
        .response_tx_map
        .remove(&msg_id)
        .unwrap()
        .send(message)
        .unwrap();
    } else {
      let message = serde_json::from_str(&inspector_msg.content).unwrap();
      // Ignore if the receiver has been dropped.
      let _ = self.notification_tx.unbounded_send(message);
    }
  }
}

fn new_box_with<T>(new_fn: impl FnOnce(*mut T) -> T) -> Box<T> {
  let b = Box::new(MaybeUninit::<T>::uninit());
  let p = Box::into_raw(b) as *mut T;
  // SAFETY: memory layout for `T` is ensured on first line of this function
  unsafe {
    ptr::write(p, new_fn(p));
    Box::from_raw(p)
  }
}
