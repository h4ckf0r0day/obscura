use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

use crate::dispatch::{self, CdpContext};

// PR #36 comment 4341743194: the deferral queue in `process_with_interception`
// must be bounded so a stalled navigation cannot OOM the process. When the cap
// is reached we return an explicit error response rather than silently dropping.
const MAX_DEFERRED_MESSAGES: usize = 256;
const POST_LOAD_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const FIRST_LIFECYCLE_PUMP_GRACE: std::time::Duration =
    std::time::Duration::from_millis(5);

// The WS-stream forwarding channel must also be bounded: if the LocalSet
// (CDP processor + nav tasks) stalls, the accept thread keeps pushing
// `std::net::TcpStream`s into the queue. An unbounded channel would let
// that queue grow without limit and OOM the process. With a bounded
// capacity, when the LocalSet is saturated the accept thread closes the
// new connection on the spot instead of buffering it — the kernel TCP
// backlog still absorbs short-term spikes, but a long-term stall now
// fails loudly at accept time rather than silently piling up FDs.
const MAX_PENDING_WS_HANDOFFS: usize = 128;

// Cap on *live* CDP connections, each of which costs one OS thread and its own
// V8 isolates. `MAX_PENDING_WS_HANDOFFS` above bounds only the handoff queue —
// connections that have already been handed off are unbounded without this.
//
// 128 matches the handoff bound and is well above any real client fan-out
// (Playwright/Puppeteer use one connection per browser). Threads are what this
// actually bounds: with arenas capped by `cap_malloc_arenas`, 128 idle
// connections cost 146 threads, 33.2 GiB of reserved address space and 51 MiB
// resident -- and nearly all of that 33.2 GiB is V8's process-wide sandbox,
// which is there at zero connections. Override with `--max-connections`.
pub const DEFAULT_MAX_CONNECTIONS: usize = 128;

// How long shutdown waits for connection threads to finish before persisting
// the cookie jar. Well under the 10s `docker stop` gives us before SIGKILL.
const SHUTDOWN_DRAIN_MS: u64 = 3_000;

// Sent to a client that arrives while the server is at `max_connections`, in
// place of dropping the socket unexplained. The client sees a refusal it can
// retry rather than a bare connection reset.
const CONNECTION_LIMIT_RESPONSE: &str = "HTTP/1.1 503 Service Unavailable\r\n\
    Content-Length: 0\r\nConnection: close\r\n\
    X-Obscura-Reason: max-connections\r\n\r\n";
const MAX_TEARDOWN_REQUEST_BYTES: usize = 256 * 1024;
use crate::types::CdpRequest;

struct CdpMessage {
    text: String,
    reply_tx: mpsc::UnboundedSender<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionEnd {
    Open = 0,
    Closed = 1,
    Shutdown = 2,
}

struct ActiveNavigation {
    generation: u64,
    page_id: String,
    context_id: String,
    disposable_context: bool,
    control: obscura_browser::navigation::NavigationControl,
    teardown_requests: std::collections::VecDeque<String>,
    teardown_request_bytes: usize,
}

struct ConnectionControl {
    end: std::sync::atomic::AtomicU8,
    end_notify: Notify,
    next_generation: std::sync::atomic::AtomicU64,
    active: std::sync::Mutex<Option<ActiveNavigation>>,
}

impl ConnectionControl {
    fn new() -> Self {
        Self {
            end: std::sync::atomic::AtomicU8::new(ConnectionEnd::Open as u8),
            end_notify: Notify::new(),
            next_generation: std::sync::atomic::AtomicU64::new(0),
            active: std::sync::Mutex::new(None),
        }
    }

    fn end(&self) -> ConnectionEnd {
        match self.end.load(Ordering::Acquire) {
            1 => ConnectionEnd::Closed,
            2 => ConnectionEnd::Shutdown,
            _ => ConnectionEnd::Open,
        }
    }

    fn signal_end(&self, end: ConnectionEnd) {
        let _ = self.end.compare_exchange(
            ConnectionEnd::Open as u8,
            end as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let control = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|active| active.control.clone());
        if let Some(control) = control {
            control.cancel();
        }
        self.end_notify.notify_waiters();
    }

    async fn ended(&self) -> ConnectionEnd {
        loop {
            let notified = self.end_notify.notified();
            let end = self.end();
            if end != ConnectionEnd::Open {
                return end;
            }
            notified.await;
        }
    }

    fn activate(
        &self,
        page_id: String,
        context_id: String,
        disposable_context: bool,
        control: obscura_browser::navigation::NavigationControl,
    ) -> u64 {
        let generation = self
            .next_generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *active = Some(ActiveNavigation {
            generation,
            page_id,
            context_id,
            disposable_context,
            control: control.clone(),
            teardown_requests: std::collections::VecDeque::new(),
            teardown_request_bytes: 0,
        });
        drop(active);
        if self.end() != ConnectionEnd::Open {
            control.cancel();
        }
        generation
    }

    fn signal_teardown_request(&self, text: &str) -> bool {
        if !text.contains("\"Target.closeTarget\"")
            && !text.contains("\"Target.disposeBrowserContext\"")
        {
            return false;
        }
        if self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_none()
        {
            return false;
        }
        let Ok(request) = serde_json::from_str::<CdpRequest>(text) else {
            return false;
        };
        let mut active_guard = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(active) = active_guard.as_mut() else {
            return false;
        };
        let matches = match request.method.as_str() {
            "Target.closeTarget" => request
                .params
                .get("targetId")
                .and_then(|value| value.as_str())
                == Some(active.page_id.as_str()),
            "Target.disposeBrowserContext" => request
                .params
                .get("browserContextId")
                .and_then(|value| value.as_str())
                == Some(active.context_id.as_str())
                && active.disposable_context,
            _ => false,
        };
        if !matches {
            return false;
        }
        // Once full, let the normal processor/deferred-queue path own the
        // command. That path is also bounded and returns an explicit busy
        // error instead of silently swallowing an unbounded duplicate flood.
        if active.teardown_requests.len() >= MAX_DEFERRED_MESSAGES
            || active.teardown_request_bytes.saturating_add(text.len()) > MAX_TEARDOWN_REQUEST_BYTES
        {
            return false;
        }
        active.teardown_request_bytes += text.len();
        active.teardown_requests.push_back(text.to_string());
        let control = active.control.clone();
        drop(active_guard);
        control.cancel();
        true
    }

    fn finish_navigation(&self, generation: u64) -> Vec<String> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active
            .as_ref()
            .is_some_and(|navigation| navigation.generation == generation)
        {
            return active
                .take()
                .map(|navigation| navigation.teardown_requests.into_iter().collect())
                .unwrap_or_default();
        }
        Vec::new()
    }
}

struct PausedInterception {
    page_id: String,
    resolver: tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
}

enum ServerMessage {
    Cdp(CdpMessage),
    NewConnection {
        reply_tx: mpsc::UnboundedSender<String>,
    },
}

fn enqueue_deferred_cdp(
    queue: &mut std::collections::VecDeque<ServerMessage>,
    queued_elsewhere: usize,
    message: CdpMessage,
    full_reason: &str,
) {
    if queue.len().saturating_add(queued_elsewhere) < MAX_DEFERRED_MESSAGES {
        queue.push_back(ServerMessage::Cdp(message));
        return;
    }
    if let Ok(request) = serde_json::from_str::<CdpRequest>(&message.text) {
        let response = crate::types::CdpResponse::error(
            request.id,
            -32000,
            full_reason.to_string(),
            request.session_id,
        );
        if let Ok(json) = serde_json::to_string(&response) {
            let _ = message.reply_tx.send(json);
        }
    }
}

fn reclassify_deferred_for_lifecycle(
    deferred: &mut std::collections::VecDeque<ServerMessage>,
    post_load_deferred: &mut std::collections::VecDeque<ServerMessage>,
    ctx: &CdpContext,
    lifecycle_page_id: &str,
) -> bool {
    let should_reclassify = deferred.front().is_some_and(|message| match message {
        ServerMessage::Cdp(cdp) => {
            command_targets_other_page(&cdp.text, ctx, lifecycle_page_id)
        }
        ServerMessage::NewConnection { .. } => false,
    });
    if !should_reclassify {
        return false;
    }
    let Some(ServerMessage::Cdp(message)) = deferred.pop_front() else {
        unreachable!("only CDP messages are reclassified")
    };
    enqueue_deferred_cdp(
        post_load_deferred,
        deferred.len(),
        message,
        "Server busy: another page is completing navigation",
    );
    true
}

fn pop_deferred_for_lifecycle_state(
    deferred: &mut std::collections::VecDeque<ServerMessage>,
    post_load_deferred: &mut std::collections::VecDeque<ServerMessage>,
    lifecycle_active: bool,
) -> Option<ServerMessage> {
    if lifecycle_active {
        deferred.pop_front()
    } else {
        post_load_deferred
            .pop_front()
            .or_else(|| deferred.pop_front())
    }
}

pub async fn start(port: u16) -> anyhow::Result<()> {
    start_with_options(port, None, false).await
}

pub async fn start_with_options(
    port: u16,
    proxy: Option<String>,
    stealth: bool,
) -> anyhow::Result<()> {
    start_with_full_options(port, proxy, stealth, None, None).await
}

pub async fn start_with_full_options(
    port: u16,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    storage_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    start_with_host(port, "127.0.0.1", proxy, stealth, user_agent, storage_dir).await
}

pub async fn start_with_host(
    port: u16,
    host: &str,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    storage_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    start_with_host_and_security(port, host, proxy, stealth, user_agent, false, storage_dir).await
}

pub async fn start_with_host_and_security(
    port: u16,
    host: &str,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    allow_file_access: bool,
    storage_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    start_with_full_serve_options(
        port, host, proxy, stealth, user_agent, allow_file_access, storage_dir, false,
    )
    .await
}

pub async fn start_with_host_security_and_storage(
    port: u16,
    host: &str,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    allow_file_access: bool,
    storage_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    start_with_full_serve_options(
        port, host, proxy, stealth, user_agent, allow_file_access, storage_dir, false,
    )
    .await
}

/// Full serve entry point that also accepts `allow_private_network` (issue
/// #33). Older entry points default it to `false` so existing callers and
/// public API consumers are unaffected.
pub async fn start_with_full_serve_options(
    port: u16,
    host: &str,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    allow_file_access: bool,
    storage_dir: Option<std::path::PathBuf>,
    allow_private_network: bool,
) -> anyhow::Result<()> {
    start_with_serve_options_and_limit(
        port,
        host,
        proxy,
        stealth,
        user_agent,
        allow_file_access,
        storage_dir,
        allow_private_network,
        DEFAULT_MAX_CONNECTIONS,
    )
    .await
}

/// As `start_with_full_serve_options`, with an explicit cap on live CDP
/// connections. Each connection owns an OS thread and its pages' V8 isolates,
/// so this is what bounds the server's thread and memory footprint.
#[allow(clippy::too_many_arguments)]
pub async fn start_with_serve_options_and_limit(
    port: u16,
    host: &str,
    proxy: Option<String>,
    stealth: bool,
    user_agent: Option<String>,
    allow_file_access: bool,
    storage_dir: Option<std::path::PathBuf>,
    allow_private_network: bool,
    max_connections: usize,
) -> anyhow::Result<()> {
    let ip: std::net::IpAddr = host
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --host '{}': {}", host, e))?;
    let addr = SocketAddr::new(ip, port);

    // Issue #62: the HTTP control plane (/json/version, /json) must remain
    // reachable even while V8 JS evaluation blocks the tokio LocalSet thread.
    //
    // We use a dedicated OS thread with a blocking std::net::TcpListener so
    // the kernel's accept backlog is always drained promptly. HTTP endpoints
    // are served directly via blocking I/O; WebSocket connections are
    // forwarded to the existing LocalSet for CDP processing.
    let std_listener = std::net::TcpListener::bind(addr)
        .map_err(|e| anyhow::anyhow!("bind {}:{}: {}", host, port, e))?;
    // Non-blocking so the accept thread can alternate between draining the
    // backlog and re-polling parked connections (see the accept thread below).
    std_listener
        .set_nonblocking(true)
        .map_err(|e| anyhow::anyhow!("set_nonblocking: {}", e))?;

    info!("Obscura CDP server listening on ws://{}:{}", host, port);
    info!(
        "DevTools endpoint: ws://{}:{}/devtools/browser",
        host, port
    );
    if allow_file_access {
        info!("file:// navigation enabled (--allow-file-access). Do not expose this port to untrusted networks.");
    }

    let (ws_tx, mut ws_rx) = mpsc::channel::<std::net::TcpStream>(MAX_PENDING_WS_HANDOFFS);

    // Ctrl-C / graceful shutdown coordination.
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let shutdown_notify = Arc::new(Notify::new());

    // Dedicated accept thread: drains the kernel backlog immediately and
    // handles HTTP endpoints (/json/version, /json, /json/protocol) with
    // blocking I/O so they never contend with the LocalSet's V8 work.
    //
    // A connection that is accepted but never sends its request head
    // (speculative browser preconnects, port probes, slow-loris clients)
    // must not be able to park this single thread in a blocking read: every
    // later connection, including CDP clients like Playwright's
    // connectOverCDP, would then sit in the kernel backlog unanswered until
    // its own connect timeout (issue #715). The thread therefore never
    // blocks on a *stream* — undecided connections are parked and re-polled
    // every ACCEPT_POLL_INTERVAL, and dropped once they outlive
    // SILENT_CONNECTION_TTL without sending a request head.
    //
    // While nothing is parked the thread blocks in accept() itself, the
    // pre-#715 fast path: zero added latency for the next connection and no
    // CPU while idle. Blocking on the listener is safe — it waits for the
    // kernel, not for client bytes. While something is parked, the listener
    // is drained without blocking at least once per 1 ms poll round, far
    // above any real connect rate, so the kernel backlog cannot overflow
    // under a connection burst.
    let accept_flag = shutdown_flag.clone();
    std::thread::Builder::new()
        .name("obscura-cdp-accept".into())
        .spawn(move || {
            let mut pending: Vec<(std::net::TcpStream, std::time::Instant)> = Vec::new();
            while !accept_flag.load(Ordering::Relaxed) {
                if pending.is_empty() {
                    // Fast path: nothing parked, block until a connection
                    // arrives, then classify it in the sweep below.
                    let _ = std_listener.set_nonblocking(false);
                    match std_listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(true);
                            pending.push((stream, std::time::Instant::now()));
                        }
                        Err(e) => {
                            error!("Accept error: {}", e);
                            // A persistent error (e.g. EMFILE) must not turn
                            // into a log flood while the thread idles.
                            std::thread::sleep(ACCEPT_POLL_INTERVAL);
                        }
                    }
                    let _ = std_listener.set_nonblocking(true);
                } else {
                    // Drain everything the kernel has already queued for us.
                    loop {
                        match std_listener.accept() {
                            Ok((stream, _)) => {
                                let _ = stream.set_nonblocking(true);
                                if pending.len() < MAX_SILENT_PENDING {
                                    pending.push((stream, std::time::Instant::now()));
                                } else {
                                    warn!(
                                        "dropping connection: {} connections parked without a request head",
                                        MAX_SILENT_PENDING
                                    );
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(e) => {
                                error!("Accept error: {}", e);
                                break;
                            }
                        }
                    }
                }
                // Give every parked connection a chance to speak; keep the
                // ones still silent and inside the TTL, dispatch the ones
                // with a request head. Dropping a stream closes its socket.
                for (stream, since) in std::mem::take(&mut pending) {
                    if since.elapsed() >= SILENT_CONNECTION_TTL {
                        continue;
                    }
                    match peek_request_head(&stream) {
                        PeekStatus::NotReady => pending.push((stream, since)),
                        PeekStatus::Closed => {}
                        PeekStatus::Head(head) => {
                            if let Err(e) = accept_dispatch(stream, port, &ws_tx, &head) {
                                if !format!("{}", e).contains("close") {
                                    error!("Accept dispatch error: {}", e);
                                }
                            }
                        }
                    }
                }
                if !pending.is_empty() {
                    std::thread::sleep(ACCEPT_POLL_INTERVAL);
                }
            }
        })?;

    // This context is a configuration and persistence template. Each WebSocket
    // gets an isolated copy with its own cookie jar and HTTP client (#449),
    // while the thread-per-connection layout from #430 still confines that
    // connection's V8 isolates to one OS thread.
    let mut bctx = obscura_browser::BrowserContext::with_storage_and_network(
        "default".to_string(),
        proxy,
        stealth,
        user_agent,
        storage_dir,
        allow_private_network,
    );
    bctx.allow_file_access = allow_file_access;
    let shared_ctx = Arc::new(bctx);
    // Persistence is deliberately separate from the connection template.
    // Cookie deltas are merged here, but new connections always clone the
    // immutable startup snapshot and can never inherit another live client's
    // session state.
    let persistence_ctx = Arc::new(shared_ctx.isolated_copy("persistence".to_string(), true));
    let persistence_lock = Arc::new(std::sync::Mutex::new(()));

    // One graceful-shutdown watcher for the whole server. It flips the accept
    // flag (stopping the accept thread) and wakes every connection processor via
    // `notify_waiters()`. On its own thread so it needs no LocalSet and cannot be
    // starved by a connection's V8 work. Watches SIGTERM as well as Ctrl-C so
    // `docker stop` / `kill` also flush cookies (issue #333).
    {
        let sf = shutdown_flag.clone();
        let sn = shutdown_notify.clone();
        std::thread::Builder::new()
            .name("obscura-cdp-signal".into())
            .spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    rt.block_on(async {
                        #[cfg(unix)]
                        {
                            use tokio::signal::unix::{signal, SignalKind};
                            match signal(SignalKind::terminate()) {
                                Ok(mut term) => {
                                    tokio::select! {
                                        _ = tokio::signal::ctrl_c() => {}
                                        _ = term.recv() => {}
                                    }
                                }
                                Err(_) => {
                                    let _ = tokio::signal::ctrl_c().await;
                                }
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = tokio::signal::ctrl_c().await;
                        }
                    });
                }
                sf.store(true, Ordering::Relaxed);
                sn.notify_waiters();
            })
            .ok();
    }

    // Force V8 and its process-global isolate tables (the leaptiering
    // JSDispatchTable / external-pointer tables) to initialize once on this main
    // thread before any connection thread creates an isolate. Creating the very
    // first isolate off the main thread segfaults inside
    // InitializeBuiltinJSDispatchTable (#430 thread-per-connection). Building and
    // dropping one runtime here does the one-time setup single-threaded.
    drop(obscura_js::runtime::ObscuraJsRuntime::new());

    cap_malloc_arenas();

    // Live CDP connections, incremented on accept and decremented when a
    // connection thread exits (see `run_connection`).
    let live_connections = Arc::new(AtomicUsize::new(0));
    info!("Connection limit: {}", max_connections);

    // Accept loop: each connection's page processor gets a dedicated OS thread,
    // while WebSocket I/O stays on this runtime. Keeping ingress off the V8
    // thread lets target close, connection loss, and shutdown signal a
    // synchronous navigation without entering its isolate concurrently.
    let mut server_shutdown = Box::pin(shutdown_notify.notified());
    server_shutdown.as_mut().enable();
    loop {
        let stream = tokio::select! {
            stream = ws_rx.recv() => stream,
            _ = &mut server_shutdown => None,
        };
        let stream = match stream {
            Some(s) => s,
            None => break,
        };
        // `select!` is fair when a handoff and shutdown become ready together.
        // The atomic is the sticky authority: never start a connection after
        // the one-shot notification has already fired.
        if shutdown_flag.load(Ordering::Acquire) {
            break;
        }
        // Nagle off + nonblocking on the std socket before it moves to the
        // connection thread. CDP exchanges many small (~100-byte) frames during
        // newPage()/navigate; with Nagle on, each small write waits on an ACK or
        // the 40ms delayed-ACK timer (~90ms on newPage, ~30ms on goto).
        stream
            .set_nonblocking(true)
            .map_err(|e| error!("set_nonblocking on WS stream: {}", e))
            .ok();
        stream
            .set_nodelay(true)
            .map_err(|e| error!("set_nodelay on WS stream: {}", e))
            .ok();
        // Reserve a slot before spawning. `fetch_update` (rather than a load
        // then a store) keeps the check atomic against the accept thread
        // handing off the next stream concurrently.
        let reserved = live_connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max_connections).then_some(n + 1)
            })
            .is_ok();
        if !reserved {
            warn!(
                "refusing CDP connection: at --max-connections ({})",
                max_connections
            );
            refuse_connection(stream);
            continue;
        }
        let tokio_stream = match TcpStream::from_std(stream) {
            Ok(stream) => stream,
            Err(error) => {
                error!("TcpStream::from_std failed: {}", error);
                live_connections.fetch_sub(1, Ordering::AcqRel);
                continue;
            }
        };
        let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ServerMessage>();
        let connection_control = Arc::new(ConnectionControl::new());
        if !run_connection(
            msg_rx,
            shared_ctx.clone(),
            persistence_ctx.clone(),
            persistence_lock.clone(),
            shutdown_notify.clone(),
            live_connections.clone(),
            connection_control.clone(),
        ) {
            continue;
        }
        let ws_shutdown = shutdown_notify.clone();
        let ws_shutdown_flag = shutdown_flag.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection_ws(
                tokio_stream,
                msg_tx,
                connection_control.clone(),
                ws_shutdown,
                ws_shutdown_flag,
            )
            .await
            {
                error!("WebSocket connection error: {}", error);
            }
            connection_control.signal_end(ConnectionEnd::Closed);
        });
    }

    // Server is shutting down. Connection threads are detached, so saving the
    // jar right here would race them: a connection still writing a Set-Cookie
    // loses it, and the process then exits and kills the thread mid-flight.
    // Before the per-connection move, the single processor saved on its own way
    // out, ordered against all connection work on one LocalSet -- draining here
    // is what restores that ordering. `notify_waiters` above has already woken
    // every processor, so this is bounded in practice; the deadline only covers
    // a connection wedged in V8, where its own command watchdog is the backstop.
    let drain_deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_millis(SHUTDOWN_DRAIN_MS);
    loop {
        let live = live_connections.load(Ordering::Acquire);
        if live == 0 {
            break;
        }
        if tokio::time::Instant::now() >= drain_deadline {
            warn!(
                "shutting down with {} connection(s) still live after {}ms; \
                 cookies they write from here are lost",
                live, SHUTDOWN_DRAIN_MS
            );
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    persistence_ctx.save_cookies();
    Ok(())
}

/// Cap the number of per-thread malloc arenas glibc will create.
///
/// glibc hands each new thread its own 64 MiB arena (up to 8x cores). With one
/// thread per connection that is the dominant per-connection memory term:
/// measured with `reliability/conn-scale.py`, 100 connections each running JS
/// reserve 90.0 GiB of address space uncapped and 83.5 GiB capped, and 100 idle
/// connections go from 65 MiB of reserved address space per connection to
/// 2.0 MiB.
///
/// For scale: at the same 100-connection JS workload `main` (one shared
/// isolate) reserves 83.6 GiB, so with the cap this server is level with it on
/// address space. Most of that total is V8's process-wide sandbox, which `main`
/// pays too as soon as it runs any JS at all.
///
/// The resident-set effect matters more than the reservation: freed chunks stay
/// in their arena rather than returning to the OS, so RSS tracks the *peak*
/// number of concurrent connections and never comes back down, which reads as a
/// leak. Measured in the container image against Google Maps, four concurrent
/// connections per round: 350 / 619 / 826 MiB over three rounds uncapped and
/// still climbing linearly, versus 166 / 235 / 269 MiB capped, on a
/// decelerating curve.
///
/// Two arenas cost no measurable throughput here (8 concurrent connections x 12
/// navigations: 1.53s uncapped, 1.50s capped): V8 allocates the JS heap through
/// its own allocator, and the Rust side is dominated by network I/O rather than
/// malloc traffic. Only `serve` calls this, and it owns the process. Respects a
/// caller-set `MALLOC_ARENA_MAX`.
fn cap_malloc_arenas() {
    #[cfg(target_env = "gnu")]
    {
        if std::env::var_os("MALLOC_ARENA_MAX").is_some() {
            return;
        }
        // M_ARENA_MAX is not exported by the libc crate.
        const M_ARENA_MAX: libc::c_int = -8;
        // SAFETY: mallopt is thread-safe; called once here before any
        // connection thread exists.
        if unsafe { libc::mallopt(M_ARENA_MAX, 2) } != 1 {
            warn!("mallopt(M_ARENA_MAX) failed; memory will scale with peak concurrency");
        }
    }
}

/// Run one connection's page processor on its own OS thread. WebSocket I/O
/// remains on the server runtime and communicates only through channels and
/// [`ConnectionControl`]; all Page/V8 ownership, interception work, and local
/// navigation futures stay confined to this thread (#430).
fn run_connection(
    msg_rx: mpsc::UnboundedReceiver<ServerMessage>,
    context_template: Arc<obscura_browser::BrowserContext>,
    persistence_context: Arc<obscura_browser::BrowserContext>,
    persistence_lock: Arc<std::sync::Mutex<()>>,
    shutdown_notify: Arc<Notify>,
    live_connections: Arc<AtomicUsize>,
    connection_control: Arc<ConnectionControl>,
) -> bool {
    // Releases the slot reserved by the accept loop when the thread unwinds,
    // however it exits — clean close, error return, or panic. A plain
    // decrement at the end of the closure would leak slots on the early
    // returns below until the cap wedged the server shut.
    struct SlotGuard(Arc<AtomicUsize>);
    impl Drop for SlotGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    let slot = live_connections.clone();
    let spawned = std::thread::Builder::new()
        .name("obscura-cdp-conn".into())
        .spawn(move || {
            let _slot = SlotGuard(slot);
            let default_context = Arc::new(
                context_template.isolated_copy("default".to_string(), true),
            );
            let initial_cookies = default_context.cookie_jar.get_all_cookies();
            let persisted_context = default_context.clone();
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    error!("connection runtime build failed: {}", e);
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                cdp_processor(
                    msg_rx,
                    default_context,
                    shutdown_notify,
                    connection_control,
                )
                .await;
            });

            // Apply only this connection's cookie changes to the persistence
            // template. Unchanged cookies cannot overwrite another connection's
            // updates, while explicit deletes and replacements still persist.
            if persistence_context.storage_dir.is_some() {
                let _guard = persistence_lock.lock().unwrap_or_else(|e| e.into_inner());
                merge_cookie_delta(
                    &persistence_context.cookie_jar,
                    &initial_cookies,
                    &persisted_context.cookie_jar.get_all_cookies(),
                );
                persistence_context.save_cookies();
            }
        });

    // The closure never ran, so its `SlotGuard` never existed: release the
    // reserved slot here or the cap drifts down on every failed spawn.
    if let Err(e) = spawned {
        error!("connection thread spawn failed: {}", e);
        live_connections.fetch_sub(1, Ordering::AcqRel);
        return false;
    }
    true
}

fn cookie_key(cookie: &obscura_net::CookieInfo) -> (String, String, String) {
    (
        cookie.domain.clone(),
        cookie.name.clone(),
        cookie.path.clone(),
    )
}

fn cookie_values_match(
    left: &obscura_net::CookieInfo,
    right: &obscura_net::CookieInfo,
) -> bool {
    left.value == right.value
        && left.secure == right.secure
        && left.http_only == right.http_only
        && left.same_site == right.same_site
        && left.expires == right.expires
}

fn merge_cookie_delta(
    destination: &obscura_net::CookieJar,
    initial: &[obscura_net::CookieInfo],
    current: &[obscura_net::CookieInfo],
) {
    let initial: HashMap<_, _> = initial.iter().map(|cookie| (cookie_key(cookie), cookie)).collect();
    let current: HashMap<_, _> = current.iter().map(|cookie| (cookie_key(cookie), cookie)).collect();

    for (key, cookie) in &initial {
        if !current.contains_key(key) {
            destination.delete_cookies_filtered(
                &cookie.name,
                &cookie.domain,
                Some(&cookie.path),
            );
        }
    }

    let changed: Vec<_> = current
        .iter()
        .filter_map(|(key, cookie)| match initial.get(key) {
            Some(previous) if cookie_values_match(previous, cookie) => None,
            _ => Some((*cookie).clone()),
        })
        .collect();
    destination.set_cookies_from_cdp(changed);
}

/// Turn away a connection that arrived while the server was at its limit.
///
/// Best-effort: the socket is going away either way, so a failed write just
/// means the client sees a reset instead of the 503.
fn refuse_connection(stream: std::net::TcpStream) {
    use std::io::{Read, Write};
    let mut stream = stream;
    let _ = stream.set_nonblocking(false);

    // The accept thread only peeked at the WebSocket handshake. Consume its
    // bounded HTTP header before closing: Windows resets a socket closed with
    // unread receive data, which can discard the queued 503 response.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(100)));
    let mut request = [0u8; HTTP_PEEK_BUF];
    let mut received = 0;
    while received < request.len() {
        match stream.read(&mut request[received..]) {
            Ok(0) => break,
            Ok(n) => {
                received += n;
                if request[..received].windows(4).any(|end| end == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = stream.write_all(CONNECTION_LIMIT_RESPONSE.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

const HTTP_PEEK_BUF: usize = 4096;

/// How long a freshly accepted connection may sit without sending a request
/// head before the accept thread drops it. Real clients send their handshake
/// immediately after connecting; only probes and preconnects linger.
const SILENT_CONNECTION_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// How often the accept thread re-polls parked connections that have not sent
/// a request head yet. Also the retry delay on a persistent accept error, so
/// it cannot become a log flood. Only paid while something is actually
/// parked; an idle server blocks in accept() and polls nothing.
const ACCEPT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);

/// Cap on connections parked without a request head. Bounds the accept
/// thread's polling work and the server's fd usage under probe floods.
const MAX_SILENT_PENDING: usize = 256;

/// Result of polling a freshly accepted connection for its request head.
enum PeekStatus {
    /// No classifiable request head yet; poll again next accept round.
    NotReady,
    /// Peer went away without sending a full head.
    Closed,
    /// A classifiable request head.
    Head(String),
}

/// Peek — without consuming — at a freshly accepted connection's request
/// head. `GET` requests are only classified once the terminating blank line
/// has arrived, so `/json` route matching never sees a truncated head;
/// anything that cannot be a `GET` is handed over immediately so non-HTTP
/// garbage still gets tungstenite's prompt rejection instead of waiting out
/// the silent-connection TTL.
fn peek_request_head(stream: &std::net::TcpStream) -> PeekStatus {
    let mut buf = [0u8; HTTP_PEEK_BUF];
    let n = match stream.peek(&mut buf) {
        Ok(0) => return PeekStatus::Closed,
        Ok(n) => n,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return PeekStatus::NotReady,
        Err(_) => return PeekStatus::Closed,
    };
    let head = &buf[..n];
    if n >= 4 && head[..4] != *b"GET " {
        return PeekStatus::Head(String::from_utf8_lossy(head).into_owned());
    }
    // A head that overflows the peek buffer is classified with what arrived,
    // matching the pre-polling behavior for oversized headers.
    let complete = n == HTTP_PEEK_BUF || head.windows(4).any(|w| w == b"\r\n\r\n");
    if !complete {
        return PeekStatus::NotReady;
    }
    PeekStatus::Head(String::from_utf8_lossy(head).into_owned())
}

/// Dispatch a freshly-accepted TCP connection on the dedicated accept thread.
///
/// The connection's request head has already been peeked by the accept loop
/// (`peek_request_head`) and is passed in as `head`:
/// - HTTP (`GET /json/*`): serve synchronously via blocking I/O so the
///   response is never stalled by the LocalSet.
/// - WebSocket: forward to the LocalSet for CDP processing.
fn accept_dispatch(
    stream: std::net::TcpStream,
    port: u16,
    ws_tx: &mpsc::Sender<std::net::TcpStream>,
    head: &str,
) -> anyhow::Result<()> {
    let endpoint = if head.contains("/json/version") {
        Some("version")
    } else if head.contains("/json/list") || head.contains("/json\r\n") || head.contains("/json HTTP") {
        Some("list")
    } else if head.contains("/json/protocol") {
        Some("protocol")
    } else {
        None
    };

    if let Some(ep) = endpoint {
        // The request head is already sitting in the kernel receive buffer;
        // switch back to blocking mode for the synchronous /json serve.
        let _ = stream.set_nonblocking(false);
        return handle_http_json_blocking(stream, port, ep);
    }
    // Fall through: GET request that isn't a /json endpoint → treat as
    // WebSocket upgrade (Chromium DevTools clients issue GET with
    // Upgrade: websocket).

    // Try to hand off the WS stream to the LocalSet. If the bounded channel
    // is full the LocalSet is saturated — drop the connection cleanly
    // rather than blocking the accept thread (which would freeze the HTTP
    // control plane that this whole rework exists to keep alive). The
    // dropped `stream` closes itself; the client will see ECONNRESET and
    // can retry.
    ws_tx
        .try_send(stream)
        .map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                warn!("WS handoff channel full ({}); dropping new WebSocket connection", MAX_PENDING_WS_HANDOFFS);
                anyhow::anyhow!("ws handoff channel full")
            }
            mpsc::error::TrySendError::Closed(_) => anyhow::anyhow!("accept channel closed"),
        })
}

/// Serve an HTTP `/json/*` endpoint with blocking I/O on the accept thread.
fn handle_http_json_blocking(
    mut stream: std::net::TcpStream,
    port: u16,
    endpoint: &str,
) -> anyhow::Result<()> {
    use std::io::{Read, Write};

    let mut buf = vec![0u8; 4096];
    let _ = stream.read(&mut buf)?;

    let body = match endpoint {
        "version" => serde_json::to_string_pretty(&json!({
            "Browser": "Chrome/145.0.0.0",
            "Protocol-Version": "1.3",
            "User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
            "V8-Version": "14.5.0.0",
            "WebKit-Version": "537.36",
            "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/browser", port),
        }))?,
        "list" => serde_json::to_string_pretty(&json!([{
            "description": "",
            "devtoolsFrontendUrl": "",
            "id": "page-1",
            "title": "",
            "type": "page",
            "url": "about:blank",
            "webSocketDebuggerUrl": format!("ws://127.0.0.1:{}/devtools/page/page-1", port),
        }]))?,
        "protocol" => {
            serde_json::to_string_pretty(&json!({ "version": { "major": "1", "minor": "3" } }))?
        }
        _ => "{}".to_string(),
    };

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body,
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Per-connection CDP processor. Each connection runs its own processor (with
/// its own `CdpContext` and pages) on its own OS thread, so every page's V8
/// isolate is confined to a single thread. This removes the #430 abort by
/// construction: V8's `heap->isolate() == Isolate::TryGetCurrent()` invariant is
/// per-thread, so two connections' isolates can never collide. All processors
/// own isolated `BrowserContext` (cookie jar and HTTP client). Cookie deltas are
/// merged into the persistence template when the connection thread exits.
async fn cdp_processor(
    mut rx: mpsc::UnboundedReceiver<ServerMessage>,
    default_context: Arc<obscura_browser::BrowserContext>,
    shutdown_notify: Arc<Notify>,
    connection_control: Arc<ConnectionControl>,
) {
    // Keep shutdown cancellation live even while the main processor future is
    // inside `process_with_interception`. This waiter performs no page/V8 work;
    // it only flips the sticky connection control and terminates the currently
    // registered isolate through NavigationControl.
    let shutdown_signal = shutdown_notify.clone();
    let shutdown_control = connection_control.clone();
    let shutdown_watcher = tokio::task::spawn_local(async move {
        shutdown_signal.notified().await;
        shutdown_control.signal_end(ConnectionEnd::Shutdown);
    });
    let mut ctx = CdpContext::new_with_shared_context(default_context);
    let (itx, irx) = mpsc::unbounded_channel::<obscura_js::ops::InterceptedRequest>();
    ctx.intercept_tx = Some(itx);
    let mut intercept_rx: Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>> = Some(irx);
    let mut pending_interceptions = std::collections::VecDeque::new();
    let mut intercepted_paused: HashMap<String, PausedInterception> = HashMap::new();

    // Issue #19 follow-up: messages deferred from inside
    // `process_with_interception` because routing them through
    // `process_cdp_message → dispatch` while a nav was in flight would have
    // tripped V8's TryGetCurrent invariant. Drained at the top of each
    // outer iteration so they get processed sequentially with no other nav
    // in flight.
    let mut deferred: std::collections::VecDeque<ServerMessage> =
        std::collections::VecDeque::new();
    let mut post_load_deferred: std::collections::VecDeque<ServerMessage> =
        std::collections::VecDeque::new();

    // Graceful shutdown: one signal watcher on the accept side flips the flag
    // and calls `notify_waiters()`. Polled once here (via the select! below) it
    // registers and stays registered across iterations, so a later
    // `notify_waiters()` wakes this processor even while it is mid-dispatch.
    let mut shutdown = Box::pin(shutdown_notify.notified());
    shutdown.as_mut().enable();
    // Chromium's PageHandler receives compositor video frames continuously.
    // Obscura has no separate compositor thread yet, so active screencasts get
    // a bounded 30 Hz opportunity on this connection's owning LocalSet.
    let mut screencast_tick = tokio::time::interval(tokio::time::Duration::from_millis(33));
    screencast_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut connection_reply_tx: Option<mpsc::UnboundedSender<String>> = None;
    // A real browser renderer continues servicing timers, networking, posted
    // tasks, and animation callbacks while its DevTools client is silent. Keep
    // one wake-driven deno_core turn armed after work may have been scheduled;
    // the future parks on the runtime's own waker and is cancelled whenever a
    // higher-priority protocol command arrives. Full idle disarms it until the
    // next command/navigation, so static pages consume no polling budget.
    let mut runtime_pump_armed = false;
    let mut runtime_pump_error_streak = 0_u8;
    let mut lifecycle_continuation_page: Option<String> = None;
    let mut lifecycle_release_deadline: Option<tokio::time::Instant> = None;
    let mut lifecycle_first_pump_not_before: Option<tokio::time::Instant> = None;

    loop {
        if lifecycle_first_pump_not_before
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            lifecycle_first_pump_not_before = None;
        }
        // A DOMContentLoaded listener may have queued a replacement before the
        // old runtime parked. Navigation wins over pumping that old document.
        if let (Some(reply_tx), Some((session_id, url, method, body))) = (
            connection_reply_tx.as_ref(),
            take_live_pending_navigation(&ctx),
        ) {
            if let Some(page_id) = ctx.sessions.get(&session_id).cloned() {
                let replaced_page = std::collections::HashSet::from([page_id]);
                fail_paused_interceptions(&replaced_page, &mut intercepted_paused);
                fail_queued_interceptions(
                    &replaced_page,
                    &mut pending_interceptions,
                    &mut intercept_rx,
                );
            }
            let navigation = json!({
                "id": 0,
                "method": "Page.navigate",
                "params": {"url": url, "__method": method, "__body": body},
                "sessionId": session_id,
            })
            .to_string();
            process_with_interception(
                &navigation,
                &mut ctx,
                reply_tx,
                &mut rx,
                &mut intercept_rx,
                &mut pending_interceptions,
                &mut intercepted_paused,
                &mut deferred,
                &mut post_load_deferred,
                false,
                &connection_control,
            )
            .await;
            lifecycle_continuation_page = ctx
                .sessions
                .get(&session_id)
                .and_then(|page_id| ctx.get_page(page_id))
                .filter(|page| {
                    matches!(
                        page.lifecycle,
                        obscura_browser::lifecycle::LifecycleState::DomContentLoaded
                            | obscura_browser::lifecycle::LifecycleState::Loaded
                    )
                })
                .map(|page| page.id.clone());
            lifecycle_first_pump_not_before = lifecycle_continuation_page
                .as_ref()
                .map(|_| tokio::time::Instant::now() + FIRST_LIFECYCLE_PUMP_GRACE);
            lifecycle_release_deadline = None;
            runtime_pump_armed = ctx.pages.iter().any(|page| page.has_js());
            continue;
        }
        // Drain any deferred messages from the previous interception window
        // before pulling new ones off the wire. Each is processed with no
        // nav-task spawn_local in flight, so this connection's only entered
        // Isolate is the one dispatch is about to touch.
        if lifecycle_continuation_page.as_ref().is_some_and(|page_id| {
            reclassify_deferred_for_lifecycle(
                &mut deferred,
                &mut post_load_deferred,
                &ctx,
                page_id,
            )
        }) {
            continue;
        }
        let msg = pop_deferred_for_lifecycle_state(
            &mut deferred,
            &mut post_load_deferred,
            lifecycle_continuation_page.is_some(),
        );
        let msg = if msg.is_some() {
            msg
        } else {
            // Give the first foreground command after navigation the same
            // opportunity ahead of compositor work as it has ahead of V8
            // page tasks. Rasterization can also run synchronously.
            let screencast_active = lifecycle_first_pump_not_before.is_none()
                && has_active_screencast(&ctx);
            let has_intercept_rx = intercept_rx.is_some();
            let first_pump_not_before = lifecycle_first_pump_not_before;
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(m) => Some(m),
                    None => {
                        connection_control.signal_end(ConnectionEnd::Closed);
                        break;
                    }
                },
                _ = &mut shutdown => {
                    tracing::info!("Shutdown signal received (connection processor)");
                    connection_control.signal_end(ConnectionEnd::Shutdown);
                    break;
                },
                _ = async {
                    if let Some(deadline) = lifecycle_release_deadline {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if lifecycle_release_deadline.is_some() => {
                    lifecycle_continuation_page = None;
                    lifecycle_release_deadline = None;
                    lifecycle_first_pump_not_before = None;
                    runtime_pump_armed = ctx.pages.iter().any(|page| page.has_js());
                    None
                },
                pump_result = async {
                    if let Some(deadline) = first_pump_not_before {
                        tokio::time::sleep_until(deadline).await;
                    }
                    pump_live_page_event_loop(
                        &mut ctx,
                        lifecycle_continuation_page.as_deref(),
                    ).await
                }, if runtime_pump_armed => {
                    lifecycle_first_pump_not_before = None;
                    let mut completed_lifecycle = false;
                    let reached_idle = match pump_result {
                        Ok(reached_idle) => {
                            runtime_pump_error_streak = 0;
                            runtime_pump_armed = !reached_idle;
                            Some(reached_idle)
                        }
                        Err(error) => {
                            runtime_pump_error_streak = runtime_pump_error_streak.saturating_add(1);
                            tracing::warn!("autonomous page task failed: {error}");
                            if runtime_pump_error_streak > 3 {
                                if let Some(page_id) = lifecycle_continuation_page
                                    .clone()
                                    .or_else(|| {
                                        ctx.pages
                                            .iter()
                                            .find(|page| page.has_js())
                                            .map(|page| page.id.clone())
                                    })
                                {
                                    let sessions = ctx
                                        .sessions
                                        .iter()
                                        .filter(|(_, owner)| *owner == &page_id)
                                        .map(|(session_id, _)| session_id.clone())
                                        .collect::<Vec<_>>();
                                    for session_id in &sessions {
                                        ctx.pending_events.push(crate::types::CdpEvent {
                                            method: "Inspector.targetCrashed".to_string(),
                                            params: json!({"status": "crashed", "errorCode": 0}),
                                            session_id: Some(session_id.clone()),
                                        });
                                        ctx.pending_events.push(crate::types::CdpEvent::new(
                                            "Target.detachedFromTarget",
                                            json!({"sessionId": session_id, "targetId": &page_id}),
                                        ));
                                    }
                                    ctx.pending_events.push(crate::types::CdpEvent::new(
                                        "Target.targetDestroyed",
                                        json!({"targetId": &page_id}),
                                    ));
                                    ctx.remove_page(&page_id);
                                    if lifecycle_continuation_page.as_deref()
                                        == Some(page_id.as_str())
                                    {
                                        lifecycle_continuation_page = None;
                                        lifecycle_release_deadline = None;
                                        lifecycle_first_pump_not_before = None;
                                    }
                                }
                                runtime_pump_armed = false;
                            } else {
                                runtime_pump_armed = ctx.pages.iter().any(|page| page.has_js());
                                tokio::task::yield_now().await;
                            }
                            None
                        }
                    };
                    let terminal_lifecycle = lifecycle_continuation_page
                        .as_ref()
                        .and_then(|page_id| ctx.get_page(page_id))
                        .is_some_and(|page| {
                            matches!(
                                page.lifecycle,
                                obscura_browser::lifecycle::LifecycleState::Loaded
                                    | obscura_browser::lifecycle::LifecycleState::NetworkIdle
                                    | obscura_browser::lifecycle::LifecycleState::Failed
                            )
                        });
                    if terminal_lifecycle {
                        if reached_idle == Some(true) {
                            completed_lifecycle = true;
                        } else {
                            lifecycle_release_deadline.get_or_insert_with(|| {
                                tokio::time::Instant::now() + POST_LOAD_DRAIN_TIMEOUT
                            });
                        }
                    }
                    sync_live_page_network_events(
                        &mut ctx,
                        lifecycle_continuation_page.as_deref(),
                    );
                    dispatch::drain_runtime_events(&mut ctx);
                    dispatch::drain_binding_calls(&mut ctx);
                    dispatch::drain_frame_events(&mut ctx);
                    // Chromium reports the load-delaying resource completion
                    // and callbacks caused by that work before document load.
                    sync_live_page_lifecycle_events(
                        &mut ctx,
                        lifecycle_continuation_page.as_deref(),
                    );
                    forward_pending_events(&mut ctx, connection_reply_tx.as_ref());
                    if completed_lifecycle {
                        lifecycle_continuation_page = None;
                        lifecycle_release_deadline = None;
                        lifecycle_first_pump_not_before = None;
                    }
                    // A continuously ready page task must still yield to the
                    // WebSocket reader/writer and to shutdown/deadline arms.
                    tokio::task::yield_now().await;
                    None
                },
                Some(intercepted) = receive_interception(&mut pending_interceptions, &mut intercept_rx), if has_intercept_rx => {
                    let route = interception_route(&ctx, &intercepted.owner_page_id);
                    if let (Some((page_id, session_id, frame_id)), Some(reply_tx)) =
                        (route, connection_reply_tx.as_ref())
                    {
                        emit_intercepted_request(
                            intercepted,
                            &page_id,
                            &frame_id,
                            Some(session_id),
                            reply_tx,
                            &mut intercepted_paused,
                        );
                    } else {
                        let _ = intercepted.resolver.send(
                            obscura_js::ops::InterceptResolution::Fail {
                                reason: "Aborted".into(),
                            },
                        );
                    }
                    None
                },
                _ = screencast_tick.tick(), if screencast_active => {
                    pump_and_forward_screencast_frames(
                        &mut ctx,
                        connection_reply_tx.as_ref(),
                    ).await;
                    None
                }
            }
        };

        let Some(msg) = msg else {
            continue;
        };

        match msg {
            ServerMessage::NewConnection { reply_tx } => {
                connection_reply_tx = Some(reply_tx.clone());
                let _ = reply_tx.send(
                    json!({"__init": true})
                        .to_string(),
                );
            }
            ServerMessage::Cdp(cdp_msg) => {
                if lifecycle_continuation_page.as_ref().is_some_and(|page_id| {
                    command_targets_other_page(&cdp_msg.text, &ctx, page_id)
                }) {
                    enqueue_deferred_cdp(
                        &mut post_load_deferred,
                        deferred.len(),
                        cdp_msg,
                        "Server busy: another page is completing navigation",
                    );
                    continue;
                }
                // Route every Page.navigate through the spawn-and-defer path,
                // not just intercepted ones. Holding the V8 lock across a
                // multi-second navigate inside the regular dispatch wedges the
                // entire processor (40-site sweep: 39/40 timeouts). Spawning
                // navigation lets `cdp_processor` keep multiplexing other CDP
                // messages via the `process_with_interception` select loop;
                // unrelated requests get deferred only briefly and are drained
                // as soon as the nav settles.
                let is_navigation = is_navigate_method(&cdp_msg.text);

                if is_navigation {
                    let navigation_page_id = serde_json::from_str::<CdpRequest>(&cdp_msg.text)
                        .ok()
                        .and_then(|request| request_page_id(&request, &ctx));
                    if let Some(page_id) = navigation_page_id.as_ref() {
                        let replaced_page = std::collections::HashSet::from([page_id.clone()]);
                        fail_paused_interceptions(&replaced_page, &mut intercepted_paused);
                        fail_queued_interceptions(
                            &replaced_page,
                            &mut pending_interceptions,
                            &mut intercept_rx,
                        );
                    }
                    process_with_interception(
                        &cdp_msg.text, &mut ctx, &cdp_msg.reply_tx, &mut rx,
                        &mut intercept_rx, &mut pending_interceptions,
                        &mut intercepted_paused,
                        &mut deferred, &mut post_load_deferred, true,
                        &connection_control,
                    ).await;
                    lifecycle_continuation_page = navigation_page_id.filter(|page_id| {
                        ctx.get_page(page_id).is_some_and(|page| {
                            matches!(
                                page.lifecycle,
                                obscura_browser::lifecycle::LifecycleState::DomContentLoaded
                                    | obscura_browser::lifecycle::LifecycleState::Loaded
                            )
                        })
                    });
                    lifecycle_first_pump_not_before = lifecycle_continuation_page
                        .as_ref()
                        .map(|_| tokio::time::Instant::now() + FIRST_LIFECYCLE_PUMP_GRACE);
                    lifecycle_release_deadline = None;
                } else {
                    if let Some((destroyed_pages, disposed_context)) =
                        teardown_owners_for_request(&cdp_msg.text, &ctx)
                    {
                        fail_paused_interceptions(&destroyed_pages, &mut intercepted_paused);
                        fail_queued_interceptions(
                            &destroyed_pages,
                            &mut pending_interceptions,
                            &mut intercept_rx,
                        );
                        discard_destroyed_target_commands(
                            &mut deferred,
                            &ctx,
                            &destroyed_pages,
                            disposed_context.as_deref(),
                        );
                        discard_destroyed_target_commands(
                            &mut post_load_deferred,
                            &ctx,
                            &destroyed_pages,
                            disposed_context.as_deref(),
                        );
                    }
                    let fetch_was_resolved = cdp_msg.text.contains("Fetch.")
                        && handle_fetch_resolution(
                            &cdp_msg.text,
                            &mut ctx,
                            &cdp_msg.reply_tx,
                            &mut intercepted_paused,
                        );
                    if !fetch_was_resolved {
                        process_cdp_message(&cdp_msg.text, &mut ctx, &cdp_msg.reply_tx).await;
                    }
                }
            }
        }

        if lifecycle_continuation_page
            .as_ref()
            .is_some_and(|page_id| ctx.get_page(page_id).is_none())
        {
            lifecycle_continuation_page = None;
            lifecycle_release_deadline = None;
            lifecycle_first_pump_not_before = None;
        }

        // Dispatch may have created a page or scheduled new asynchronous work.
        // A single live isolate is the connection's current active target; the
        // pump will park cheaply if its next task is a distant timer.
        runtime_pump_armed = ctx.pages.iter().any(|page| page.has_js());
        runtime_pump_error_streak = 0;
        // Let the WebSocket writer flush this command's response and the
        // reader enqueue the client's follow-up before background page work is
        // offered another V8 turn.
        tokio::task::yield_now().await;

    }

    // The connection thread merges this context's cookie delta into the
    // persistence template after the processor stops.
    shutdown_watcher.abort();
    let _ = &ctx;
}

fn request_page_id(request: &CdpRequest, ctx: &CdpContext) -> Option<String> {
    if request.method == "Target.sendMessageToTarget" {
        if let Some(page_id) = request
            .params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| ctx.sessions.get(session_id))
        {
            return Some(page_id.clone());
        }
        if let Some(inner) = request.params.get("message").and_then(|value| value.as_str()) {
            if let Ok(inner) = serde_json::from_str::<CdpRequest>(inner) {
                return request_page_id(&inner, ctx);
            }
        }
    }
    request
        .session_id
        .as_ref()
        .and_then(|session_id| ctx.sessions.get(session_id))
        .cloned()
}

fn command_targets_other_page(
    text: &str,
    ctx: &CdpContext,
    lifecycle_page_id: &str,
) -> bool {
    let Ok(request) = serde_json::from_str::<CdpRequest>(text) else {
        return false;
    };
    request_page_id(&request, ctx).is_some_and(|page_id| page_id != lifecycle_page_id)
}

fn teardown_owners_for_request(
    text: &str,
    ctx: &CdpContext,
) -> Option<(std::collections::HashSet<String>, Option<String>)> {
    let request = serde_json::from_str::<CdpRequest>(text).ok()?;
    match request.method.as_str() {
        "Target.closeTarget" => {
            let target_id = request.params.get("targetId")?.as_str()?;
            Some((std::collections::HashSet::from([target_id.to_string()]), None))
        }
        "Target.disposeBrowserContext" => {
            let context_id = request.params.get("browserContextId")?.as_str()?.to_string();
            let pages = ctx.pages
                .iter()
                .filter(|page| page.context.id == context_id)
                .map(|page| page.id.clone())
                .collect();
            Some((pages, Some(context_id)))
        }
        _ => None,
    }
}

async fn receive_interception(
    pending: &mut std::collections::VecDeque<obscura_js::ops::InterceptedRequest>,
    receiver: &mut Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>>,
) -> Option<obscura_js::ops::InterceptedRequest> {
    if let Some(intercepted) = pending.pop_front() {
        return Some(intercepted);
    }
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn interception_route(
    ctx: &CdpContext,
    page_id: &str,
) -> Option<(String, String, String)> {
    let page = ctx.get_page(page_id)?;
    ctx.sessions
        .iter()
        .find(|(_, owner)| owner.as_str() == page_id)
        .map(|(session_id, _)| (page_id.to_string(), session_id.clone(), page.frame_id.clone()))
}

fn emit_intercepted_request(
    intercepted: obscura_js::ops::InterceptedRequest,
    page_id: &str,
    frame_id: &str,
    session_id: Option<String>,
    reply_tx: &mpsc::UnboundedSender<String>,
    intercepted_paused: &mut HashMap<String, PausedInterception>,
) {
    tracing::info!(
        "INTERCEPTION: requestPaused for {} {} (sending to client)",
        intercepted.method,
        intercepted.url
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let request = json!({
        "url": intercepted.url,
        "method": intercepted.method,
        "headers": intercepted.headers,
        "initialPriority": "High",
        "referrerPolicy": "strict-origin-when-cross-origin",
    });
    let request_will_be_sent = json!({
        "method": "Network.requestWillBeSent",
        "params": {
            "requestId": intercepted.request_id,
            "loaderId": "",
            "documentURL": "",
            "request": request,
            "timestamp": now,
            "wallTime": now,
            "initiator": {"type": "script"},
            "type": intercepted.resource_type,
            "frameId": frame_id,
        },
        "sessionId": session_id,
    });
    let _ = reply_tx.send(request_will_be_sent.to_string());

    let request_paused = json!({
        "method": "Fetch.requestPaused",
        "params": {
            "requestId": intercepted.request_id,
            "request": request,
            "frameId": frame_id,
            "resourceType": intercepted.resource_type,
            "networkId": intercepted.request_id,
            "responseErrorReason": null,
            "responseStatusCode": null,
            "responseHeaders": null,
        },
        "sessionId": session_id,
    });
    let _ = reply_tx.send(request_paused.to_string());
    intercepted_paused.insert(intercepted.request_id, PausedInterception {
        page_id: page_id.to_string(),
        resolver: intercepted.resolver,
    });
}

async fn pump_live_page_event_loop(
    ctx: &mut CdpContext,
    lifecycle_page_id: Option<&str>,
) -> Result<bool, String> {
    let page_index = lifecycle_page_id
        .and_then(|page_id| ctx.pages.iter().position(|page| page.id == page_id))
        .or_else(|| ctx.pages.iter().position(|page| page.has_js()));
    let Some(page_index) = page_index else {
        return Ok(true);
    };
    let page = &mut ctx.pages[page_index];
    page.run_autonomous_event_loop_turn().await
}

fn sync_live_page_lifecycle_events(ctx: &mut CdpContext, lifecycle_page_id: Option<&str>) {
    let route_for = |page: &obscura_browser::Page| {
        if !page.has_js() {
            return None;
        }
        let loader_id = ctx.current_loader_ids.get(&page.id)?.clone();
        let session_id = ctx.navigation_sessions.get(&page.id)?.clone();
        Some((
            session_id,
            page.id.clone(),
            page.frame_id.clone(),
            loader_id,
            page.lifecycle,
        ))
    };
    let route = if let Some(page_id) = lifecycle_page_id {
        ctx.get_page(page_id).and_then(route_for)
    } else {
        ctx.pages.iter().find_map(route_for)
    };
    let Some((session_id, page_id, frame_id, loader_id, lifecycle)) = route else {
        return;
    };
    crate::domains::page::emit_document_lifecycle_state(
        ctx,
        &session_id,
        &frame_id,
        &loader_id,
        &page_id,
        lifecycle,
    );
}

fn sync_live_page_network_events(ctx: &mut CdpContext, lifecycle_page_id: Option<&str>) {
    let route_for = |page: &obscura_browser::Page| {
        if !page.has_js() {
            return None;
        }
        let session_id = ctx.navigation_sessions.get(&page.id)?.clone();
        Some((
            session_id,
            page.id.clone(),
            page.frame_id.clone(),
            page.url_string(),
        ))
    };
    let page_route = if let Some(page_id) = lifecycle_page_id {
        ctx.get_page(page_id).and_then(route_for)
    } else {
        ctx.pages.iter().find_map(route_for)
    };
    let Some((session_id, page_id, frame_id, page_url)) = page_route else {
        return;
    };
    let network_events = {
        let Some(page) = ctx.get_page_mut(&page_id) else {
            return;
        };
        page.sync_js_network_events();
        page.network_events.drain(..).collect::<Vec<_>>()
    };
    crate::domains::page::emit_runtime_network_events(
        ctx,
        &session_id,
        &frame_id,
        &page_url,
        &page_id,
        &network_events,
    );
}

fn take_live_pending_navigation(
    ctx: &CdpContext,
) -> Option<(String, String, String, String)> {
    ctx.pages.iter().find_map(|page| {
        if !page.has_js() {
            return None;
        }
        let session_id = ctx
            .navigation_sessions
            .get(&page.id)
            .and_then(Clone::clone)?;
        let (url, method, body) = page.take_pending_navigation()?;
        Some((session_id, url, method, body))
    })
}

fn forward_pending_events(
    ctx: &mut CdpContext,
    reply_tx: Option<&mpsc::UnboundedSender<String>>,
) {
    let Some(reply_tx) = reply_tx else {
        return;
    };
    for event in ctx.pending_events.drain(..) {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = reply_tx.send(json);
        }
    }
}

fn has_active_screencast(ctx: &CdpContext) -> bool {
    #[cfg(feature = "render")]
    {
        !ctx.screencasts.is_empty()
    }
    #[cfg(not(feature = "render"))]
    {
        let _ = ctx;
        false
    }
}

async fn pump_and_forward_screencast_frames(
    ctx: &mut CdpContext,
    reply_tx: Option<&mpsc::UnboundedSender<String>>,
) {
    #[cfg(feature = "render")]
    crate::domains::page::pump_screencast_frames(ctx).await;
    #[cfg(not(feature = "render"))]
    let _ = ctx;

    forward_pending_events(ctx, reply_tx);
}

// Whether a raw CDP frame is exactly a `Page.navigate` call, and so should take
// the spawn-and-defer navigation path. Matching on the parsed method rather than
// a `contains("Page.navigate")` substring avoids catching
// `Page.navigateToHistoryEntry` (goBack / goForward), which has no `url` param
// and belongs to its own handler, or any other frame that merely embeds the
// literal text (e.g. a `Runtime.evaluate` expression). See issue #363.
fn is_navigate_method(text: &str) -> bool {
    serde_json::from_str::<CdpRequest>(text)
        .map(|req| req.method == "Page.navigate")
        .unwrap_or(false)
}

// Parse a CDP header list (`[{"name":..,"value":..}, ..]`, as used by
// Fetch.continueRequest / fulfillRequest) into a map. Returns None when the
// `headers` field is absent, so the caller can leave the request's headers
// untouched rather than clearing them.
fn parse_cdp_headers(params: &serde_json::Value) -> Option<HashMap<String, String>> {
    let arr = params.get("headers")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|h| {
                Some((
                    h.get("name")?.as_str()?.to_string(),
                    h.get("value")?.as_str()?.to_string(),
                ))
            })
            .collect(),
    )
}

fn handle_fetch_resolution(
    text: &str,
    ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
    intercepted_paused: &mut HashMap<String, PausedInterception>,
) -> bool {
    if let Ok(req) = serde_json::from_str::<CdpRequest>(text) {
        let method = req.method.as_str();
        let request_id = req.params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
        tracing::info!("INTERCEPTION resolution: {} for {}, paused_count={}", method, request_id, intercepted_paused.len());

        let session_matches_owner = intercepted_paused.get(request_id).is_some_and(|paused| {
            req.session_id
                .as_ref()
                .and_then(|session_id| ctx.sessions.get(session_id))
                .is_none_or(|page_id| page_id == &paused.page_id)
        });
        if session_matches_owner {
            let Some(paused) = intercepted_paused.remove(request_id) else {
                return false;
            };
            tracing::info!("INTERCEPTION resolved: {}", request_id);
            let resolution = match method {
                "Fetch.continueRequest" => obscura_js::ops::InterceptResolution::Continue {
                    // Honor the client's overrides (Playwright route.continue,
                    // Puppeteer request.continue). op_fetch_url applies each and
                    // re-validates a rewritten URL through the SSRF gate. Leaving
                    // these None silently sent the request unmodified (issue #365).
                    url: req.params.get("url").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    method: req.params.get("method").and_then(|v| v.as_str()).map(|s| s.to_string()),
                    headers: parse_cdp_headers(&req.params),
                    body: req.params.get("postData").and_then(|v| v.as_str()).map(|s| s.to_string()),
                },
                "Fetch.fulfillRequest" => {
                    let status = req.params.get("responseCode").and_then(|v| v.as_u64()).unwrap_or(200) as u16;
                    let raw_body = req.params.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let body = decode_base64(raw_body);
                    let headers = req.params.get("responseHeaders")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|h| {
                            Some((h.get("name")?.as_str()?.to_string(), h.get("value")?.as_str()?.to_string()))
                        }).collect())
                        .unwrap_or_default();
                    obscura_js::ops::InterceptResolution::Fulfill { status, headers, body }
                }
                "Fetch.failRequest" => {
                    let reason = req.params.get("errorReason").and_then(|v| v.as_str()).unwrap_or("Failed").to_string();
                    obscura_js::ops::InterceptResolution::Fail { reason }
                }
                _ => return false,
            };
            let _ = paused.resolver.send(resolution);
            let resp = crate::types::CdpResponse::success(req.id, json!({}), req.session_id);
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = reply_tx.send(json);
            }
            return true;
        }
        // Internal JS fetch interceptions have owner/generation-qualified IDs.
        // Once their owner is replaced or destroyed, do not let the legacy
        // static Fetch handler acknowledge the stale ID as if it still existed.
        if request_id.contains(":intercept-") {
            let response = crate::types::CdpResponse::error(
                req.id,
                -32000,
                "Invalid InterceptionId".to_string(),
                req.session_id,
            );
            if let Ok(json) = serde_json::to_string(&response) {
                let _ = reply_tx.send(json);
            }
            return true;
        }
    }
    false
}

fn fail_paused_interceptions(
    destroyed_pages: &std::collections::HashSet<String>,
    intercepted_paused: &mut HashMap<String, PausedInterception>,
) {
    let request_ids: Vec<String> = intercepted_paused
        .iter()
        .filter(|(_, paused)| destroyed_pages.contains(&paused.page_id))
        .map(|(request_id, _)| request_id.clone())
        .collect();
    for request_id in request_ids {
        let Some(paused) = intercepted_paused.remove(&request_id) else { continue };
        let _ = paused.resolver.send(obscura_js::ops::InterceptResolution::Fail {
            reason: "Aborted".into(),
        });
    }
}

fn fail_queued_interceptions(
    destroyed_pages: &std::collections::HashSet<String>,
    pending: &mut std::collections::VecDeque<obscura_js::ops::InterceptedRequest>,
    intercept_rx: &mut Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>>,
) {
    if let Some(receiver) = intercept_rx.as_mut() {
        while let Ok(intercepted) = receiver.try_recv() {
            pending.push_back(intercepted);
        }
    }
    let mut retained = std::collections::VecDeque::with_capacity(pending.len());
    while let Some(intercepted) = pending.pop_front() {
        if destroyed_pages.contains(&intercepted.owner_page_id) {
            let _ = intercepted.resolver.send(obscura_js::ops::InterceptResolution::Fail {
                reason: "Aborted".into(),
            });
        } else {
            retained.push_back(intercepted);
        }
    }
    *pending = retained;
}

fn discard_destroyed_target_commands(
    deferred: &mut std::collections::VecDeque<ServerMessage>,
    ctx: &CdpContext,
    destroyed_pages: &std::collections::HashSet<String>,
    disposed_context: Option<&str>,
) {
    let mut retained = std::collections::VecDeque::with_capacity(deferred.len());
    while let Some(message) = deferred.pop_front() {
        let discard = match &message {
            ServerMessage::Cdp(message) => serde_json::from_str::<CdpRequest>(&message.text)
                .ok()
                .is_some_and(|request| {
                    request_page_id(&request, ctx)
                        .is_some_and(|page_id| destroyed_pages.contains(&page_id))
                        || request.params.get("targetId").and_then(|value| value.as_str())
                            .is_some_and(|page_id| destroyed_pages.contains(page_id))
                        || disposed_context.is_some_and(|context_id| {
                            request.params.get("browserContextId").and_then(|value| value.as_str())
                                == Some(context_id)
                        })
                }),
            ServerMessage::NewConnection { .. } => false,
        };
        if discard {
            if let ServerMessage::Cdp(message) = message {
                if let Ok(request) = serde_json::from_str::<CdpRequest>(&message.text) {
                    let response = crate::types::CdpResponse::error(
                        request.id,
                        -32000,
                        "Target closed".to_string(),
                        request.session_id,
                    );
                    if let Ok(json) = serde_json::to_string(&response) {
                        let _ = message.reply_tx.send(json);
                    }
                }
            }
        } else {
            retained.push_back(message);
        }
    }
    *deferred = retained;
}

fn finish_inflight_teardown(
    text: &str,
    ctx: &mut CdpContext,
    page_id: &str,
    context_id: &str,
    reply_tx: &mpsc::UnboundedSender<String>,
) {
    let Ok(request) = serde_json::from_str::<CdpRequest>(text) else {
        return;
    };
    let result = match request.method.as_str() {
        "Target.closeTarget" => Ok(json!({"success": ctx.destroy_target(page_id)})),
        "Target.disposeBrowserContext" => ctx
            .destroy_browser_context(context_id, Some(page_id))
            .map(|_| json!({})),
        _ => return,
    };
    for event in ctx.pending_events.drain(..) {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = reply_tx.send(json);
        }
    }
    let response = match result {
        Ok(result) => crate::types::CdpResponse::success(
            request.id,
            result,
            request.session_id,
        ),
        Err(error) => crate::types::CdpResponse::error(
            request.id,
            -32000,
            error,
            request.session_id,
        ),
    };
    if let Ok(json) = serde_json::to_string(&response) {
        let _ = reply_tx.send(json);
    }
}

async fn process_with_interception(
    text: &str,
    ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
    rx: &mut mpsc::UnboundedReceiver<ServerMessage>,
    intercept_rx: &mut Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>>,
    pending_interceptions: &mut std::collections::VecDeque<obscura_js::ops::InterceptedRequest>,
    intercepted_paused: &mut HashMap<String, PausedInterception>,
    deferred: &mut std::collections::VecDeque<ServerMessage>,
    post_load_deferred: &mut std::collections::VecDeque<ServerMessage>,
    send_command_response: bool,
    connection_control: &Arc<ConnectionControl>,
) {
    let req: CdpRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            warn!("Invalid CDP: {}", e);
            return;
        }
    };

    tracing::info!("INTERCEPTION navigate: {} (id={})", req.method, req.id);

    let session_id = &req.session_id;
    let page_id = session_id
        .as_ref()
        .and_then(|sid| ctx.sessions.get(sid))
        .cloned();

    let page_id = match page_id {
        Some(id) => id,
        None => {
            process_cdp_message(text, ctx, reply_tx).await;
            return;
        }
    };

    let page_index = ctx.pages.iter().position(|p| p.id == page_id);
    let mut page = match page_index {
        Some(idx) => ctx.pages.remove(idx),
        None => {
            process_cdp_message(text, ctx, reply_tx).await;
            return;
        }
    };

    // Issue #19 follow-up: V8 only allows ONE entered Isolate per OS thread.
    // The regular dispatch path enforces this via `get_session_page_mut`
    // (which `suspend_js`'es every other page before letting the target
    // page run JS). The interception path here bypasses that — it removes
    // the target page and spawns a nav task — so we have to enforce the
    // same invariant explicitly. Otherwise nav-2's `init_js` constructs
    // Isolate-2 while page-1's Isolate-1 is still alive in ctx.pages, and
    // the next V8 scope unwind aborts the process via `Context::Exit`'s
    // `heap->isolate() == Isolate::TryGetCurrent()` check.
    for other in ctx.pages.iter_mut() {
        if other.has_js() {
            other.suspend_js();
        }
    }

    let url = req.params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let nav_method = req.params.get("__method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
    let nav_body = req.params.get("__body").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let preload_scripts: Vec<String> = ctx.preload_scripts.iter().map(|(_, s)| s.clone()).collect();

    if let Some(tx) = &ctx.intercept_tx {
        page.set_intercept_tx(tx.clone());
    }

    let session_for_events = req.session_id.clone();
    let frame_id = page.frame_id.clone();
    let loader_id = format!("loader-{}", uuid::Uuid::new_v4());
    let navigation_context_id = page.context.id.clone();
    let navigation_control = obscura_browser::navigation::NavigationControl::new();
    page.set_navigation_control(navigation_control.clone());
    let navigation_generation = connection_control.activate(
        page_id.clone(),
        navigation_context_id.clone(),
        navigation_context_id != ctx.default_context.id,
        navigation_control.clone(),
    );

    let url_owned = url.to_string();
    let nav_v8_lock = ctx.v8_lock.clone();

    let mut navigation_task = tokio::task::spawn_local(async move {
        // Issue #19: serialize this connection's V8 work across its pages. This
        // nav task runs while the connection's processor keeps pumping other CDP
        // messages via `dispatch` (which takes the same per-connection lock), so
        // both sides coordinate on one page's isolate at a time on this thread.
        // The lock is per-connection, so other connections are unaffected (#430).
        let _v8_guard = nav_v8_lock.lock_owned().await;
        // Preloads (addBinding shims, addScriptToEvaluateOnNewDocument sources)
        // must run BEFORE the page's own scripts (CDP contract). Hand them
        // to the page so navigate_single can inject them at the right point.
        page.set_preload_scripts(preload_scripts);
        let result = {
            let navigation = page.navigate_with_wait_post(
                &url_owned,
                obscura_browser::lifecycle::WaitUntil::DomContentLoaded,
                &nav_method,
                &nav_body,
            );
            tokio::pin!(navigation);
            tokio::select! {
                result = &mut navigation => result.map_err(|error| error.to_string()),
                _ = navigation_control.cancelled() => {
                    Err("Navigation cancelled".to_string())
                }
            }
        };
        page.finish_navigation_control();
        drop(_v8_guard);
        (page, result)
    });

    let navigate_result: Result<(), String>;
    let page_back: Option<obscura_browser::Page>;
    let mut task_failed = false;
    let mut rx_open = true;

    // Issue #19 follow-up (PR #36 maintainer's fetch-intercept repro):
    // While the spawned nav task is executing V8 (potentially parked on
    // `op_fetch_url`'s `resolve_rx.await` *with Isolate-N still entered*),
    // we must NOT let the parent's `select!` route foreign Cdp messages
    // through `process_cdp_message → dispatch → page handlers`, because
    // those handlers call `get_session_page_mut` which `suspend_js`'es
    // OTHER pages (drops their `JsRuntime`, which calls
    // `JsRealmInner::destroy`). That trips V8's
    // `heap->isolate() == Isolate::TryGetCurrent()` invariant and aborts
    // the process via `V8_Fatal`.
    //
    // This connection's `ctx.v8_lock` doesn't save us here: it's a
    // `tokio::sync::Mutex` that is released around `.await`s inside V8
    // ops, so it doesn't actually keep the V8 enter/exit pair contiguous
    // on the thread.
    //
    // Park foreign Cdp messages into the outer deferred queue so the
    // outer `cdp_processor` loop processes them after this nav fully
    // completes (and its JsRuntime is no longer in flight on the
    // LocalSet).
    loop {
        let has_irx = intercept_rx.is_some();

        tokio::select! {
            result = &mut navigation_task => {
                match result {
                    Ok((returned_page, result)) => {
                        page_back = Some(returned_page);
                        navigate_result = result;
                    }
                    Err(error) => {
                        tracing::error!("navigation task failed: {error}");
                        page_back = None;
                        navigate_result = Err(format!("navigation task failed: {error}"));
                        task_failed = true;
                    }
                }
                break;
            }
            Some(intercepted) = receive_interception(pending_interceptions, intercept_rx), if has_irx => {
                let owner_page_id = intercepted.owner_page_id.clone();
                let (owner_session, owner_frame) = if owner_page_id == page_id {
                    (session_for_events.clone(), frame_id.clone())
                } else if let Some((_, session, frame)) = interception_route(ctx, &owner_page_id) {
                    (Some(session), frame)
                } else {
                    let _ = intercepted.resolver.send(obscura_js::ops::InterceptResolution::Fail {
                        reason: "Aborted".into(),
                    });
                    continue;
                };
                emit_intercepted_request(
                    intercepted,
                    &owner_page_id,
                    &owner_frame,
                    owner_session,
                    reply_tx,
                    intercepted_paused,
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
            msg = rx.recv(), if rx_open => {
                let Some(msg) = msg else {
                    rx_open = false;
                    connection_control.signal_end(ConnectionEnd::Closed);
                    continue;
                };
                tracing::info!("INTERCEPTION select: received CDP message during navigation");
                match msg {
                    ServerMessage::NewConnection { reply_tx: new_tx } => {
                        // Safe: no V8 enter, just bookkeeping.
                        let pid = ctx.create_page();
                        let sid = format!("{}-session", pid);
                        ctx.sessions.insert(sid.clone(), pid.clone());
                        let _ = new_tx.send(json!({"__init": true, "pageId": pid, "sessionId": sid}).to_string());
                    }
                    ServerMessage::Cdp(msg) => {
                        if connection_control.signal_teardown_request(&msg.text) {
                            // The transport normally recognizes teardown first,
                            // but a close queued immediately behind navigate may
                            // arrive before the processor registers ownership.
                            // Recheck here to close that ordering window.
                        } else if msg.text.contains("Fetch.continueRequest")
                            || msg.text.contains("Fetch.fulfillRequest")
                            || msg.text.contains("Fetch.failRequest")
                        {
                            // Safe: only flips a oneshot to resume the parked
                            // op inside the spawned nav task. No V8 enter on
                            // this side; the actual V8 work happens back on
                            // the nav task's thread.
                            handle_fetch_resolution(&msg.text, ctx, &msg.reply_tx, intercepted_paused);
                        } else {
                            // UNSAFE during nav: would route through dispatch,
                            // which can `suspend_js` other pages and trip the
                            // V8 invariant. Defer until nav completes —
                            // pushed to the outer `cdp_processor` queue so
                            // it's processed sequentially with no nav task
                            // in flight.
                            tracing::info!("INTERCEPTION: deferring CDP message until nav completes");
                            enqueue_deferred_cdp(
                                deferred,
                                post_load_deferred.len(),
                                msg,
                                "Server busy: navigation in progress, try again later",
                            );
                        }
                    }
                }
            }
        }
    }

    // Deferred messages are handled by the outer `cdp_processor` loop
    // (it drains `deferred` before pulling the next message off `rx`).

    let teardown_requests = connection_control.finish_navigation(navigation_generation);
    let connection_end = connection_control.end();
    if !teardown_requests.is_empty() || connection_end != ConnectionEnd::Open || task_failed {
        let disposing_context = teardown_requests.iter().any(|text| {
            serde_json::from_str::<CdpRequest>(text)
                .ok()
                .is_some_and(|request| request.method == "Target.disposeBrowserContext")
        });
        let mut destroyed_pages = std::collections::HashSet::from([page_id.clone()]);
        if connection_end != ConnectionEnd::Open {
            destroyed_pages.extend(ctx.pages.iter().map(|page| page.id.clone()));
        } else if disposing_context {
            destroyed_pages.extend(
                ctx.pages
                    .iter()
                    .filter(|page| page.context.id == navigation_context_id)
                    .map(|page| page.id.clone()),
            );
        }
        fail_paused_interceptions(&destroyed_pages, intercepted_paused);
        fail_queued_interceptions(
            &destroyed_pages,
            pending_interceptions,
            intercept_rx,
        );
        discard_destroyed_target_commands(
            deferred,
            ctx,
            &destroyed_pages,
            disposing_context.then_some(navigation_context_id.as_str()),
        );
        discard_destroyed_target_commands(
            post_load_deferred,
            ctx,
            &destroyed_pages,
            disposing_context.then_some(navigation_context_id.as_str()),
        );
        drop(page_back);
        if !teardown_requests.is_empty() {
            for teardown_request in teardown_requests {
                finish_inflight_teardown(
                    &teardown_request,
                    ctx,
                    &page_id,
                    &navigation_context_id,
                    reply_tx,
                );
            }
        } else {
            ctx.remove_page(&page_id);
        }
        if task_failed && connection_end == ConnectionEnd::Open {
            let response = crate::types::CdpResponse::error(
                req.id,
                -32000,
                navigate_result.err().unwrap_or_else(|| "navigation task failed".into()),
                req.session_id.clone(),
            );
            if send_command_response {
                if let Ok(json) = serde_json::to_string(&response) {
                    let _ = reply_tx.send(json);
                }
            }
        }
        return;
    }

    let mut page = page_back.expect("completed navigation should return the page");

    // Fold in network events for script-initiated requests (fetch/XHR/dynamic
    // resource) so they emit as Network.requestWillBeSent / responseReceived
    // alongside the static navigation subresources (#406).
    page.sync_js_network_events();
    let network_events: Vec<_> = page.network_events.drain(..).collect();
    let page_url = page.url_string();
    let page_id_for_events = page.id.clone();
    let reached_network_idle = page.lifecycle.is_network_idle();

    ctx.pages.push(page);

    #[cfg(feature = "render")]
    let navigation_succeeded = navigate_result.is_ok();
    let response = match navigate_result {
        Ok(()) => crate::types::CdpResponse::success(
            req.id,
            json!({"frameId": frame_id, "loaderId": loader_id}),
            req.session_id.clone(),
        ),
        Err(e) => crate::types::CdpResponse::error(req.id, -32000, e, req.session_id.clone()),
    };

    if send_command_response {
        if let Ok(json) = serde_json::to_string(&response) {
            let _ = reply_tx.send(json);
        }
    }

    // Shared event emission: includes the post-#190 Network.requestWillBeSent
    // -before-frameNavigated ordering, the #189 requestId=loaderId trick that
    // makes `page.goto()` resolve to a Response, and the #192 per-isolated-
    // world fresh context ids. Pushes to `ctx.pending_events`; we then drain
    // to the WS reply channel.
    crate::domains::page::emit_navigation_events(
        ctx,
        &session_for_events,
        &frame_id,
        &loader_id,
        &page_url,
        &page_id_for_events,
        &network_events,
        obscura_browser::lifecycle::WaitUntil::DomContentLoaded,
        reached_network_idle,
    );
    #[cfg(feature = "render")]
    if navigation_succeeded {
        if let Err(error) = crate::domains::page::queue_screencast_frame(
            ctx, &session_for_events, false,
        ) {
            tracing::warn!("could not produce post-navigation screencast frame: {error}");
        }
    }
    for event in ctx.pending_events.drain(..) {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = reply_tx.send(json);
        }
    }
}

async fn process_cdp_message(
    text: &str,
    ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
) {
    let req: CdpRequest = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            warn!("Invalid CDP: {}: {}", e, crate::util::truncate_on_char_boundary(text, 200));
            return;
        }
    };

    tracing::debug!("CDP: {} (id={}, s={:?})", req.method, req.id, req.session_id);

    let response = dispatch::dispatch(&req, ctx).await;

    // Chromium CDP semantics: events emitted as a side-effect of a command
    // (e.g. Target.targetCreated + Target.attachedToTarget from
    // Target.createTarget) MUST arrive BEFORE the command's response.
    // Playwright awaits the response and immediately reads state wired up
    // by those events; if the response lands first, accessing
    // Target._page errors with "Cannot read properties of undefined".
    for event in ctx.pending_events.drain(..) {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = reply_tx.send(json);
        }
    }

    if let Ok(json) = serde_json::to_string(&response) {
        let _ = reply_tx.send(json);
    }

    if let Some((nav_url, nav_method, nav_body)) = check_pending_navigation(ctx, &req.session_id) {
        tracing::info!("JS-triggered nav: {} {} (body: {} bytes)", nav_method, nav_url, nav_body.len());
        let nav_req = CdpRequest {
            id: 0,
            method: "Page.navigate".to_string(),
            params: json!({"url": nav_url, "__method": nav_method, "__body": nav_body}),
            session_id: req.session_id.clone(),
        };
        let _ = dispatch::dispatch(&nav_req, ctx).await;
        for event in ctx.pending_events.drain(..) {
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = reply_tx.send(json);
            }
        }
    }
}

fn decode_base64(input: &str) -> String {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter_map(val).collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
            chunk.get(3).copied().unwrap_or(0),
        ];
        out.push((b[0] << 2) | (b[1] >> 4));
        if chunk.len() > 2 { out.push((b[1] << 4) | (b[2] >> 2)); }
        if chunk.len() > 3 { out.push((b[2] << 6) | b[3]); }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn fast_path_response(text: &str) -> Option<String> {
    let req: CdpRequest = serde_json::from_str(text).ok()?;

    let result = match req.method.as_str() {
        "Network.enable" | "Network.setCacheDisabled" | "Network.setRequestInterception" |
        "Page.enable" | "Page.setLifecycleEventsEnabled" | "Page.setInterceptFileChooserDialog" |
        "Runtime.runIfWaitingForDebugger" | "Runtime.discardConsoleEntries" |
        "Performance.enable" | "Log.enable" | "Security.enable" |
        "Emulation.setTouchEmulationEnabled" |
        "CSS.enable" | "Accessibility.enable" | "ServiceWorker.enable" |
        "Inspector.enable" | "Debugger.enable" | "Profiler.enable" |
        "HeapProfiler.enable" | "Overlay.enable" | "Storage.enable" |
        "Target.setAutoAttach" => {
            Some(json!({}))
        }
        "Browser.getVersion" => {
            Some(json!({
                "protocolVersion": "1.3",
                "product": "Chrome/145.0.0.0",
                "revision": "@0000000000000000000000000000000000000000",
                "userAgent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36",
                "jsVersion": "14.5.0.0",
            }))
        }
        "Browser.setDownloadBehavior" | "Browser.getWindowBounds" => {
            Some(json!({}))
        }
        _ => None,
    };

    if let Some(value) = result {
        let resp = crate::types::CdpResponse::success(req.id, value, req.session_id);
        serde_json::to_string(&resp).ok()
    } else {
        None
    }
}

fn check_pending_navigation(ctx: &CdpContext, session_id: &Option<String>) -> Option<(String, String, String)> {
    let page_id = session_id
        .as_ref()
        .and_then(|sid| ctx.sessions.get(sid))?;
    let page = ctx.pages.iter().find(|p| &p.id == page_id)?;
    page.take_pending_navigation()
}

async fn handle_connection_ws(
    stream: TcpStream,
    msg_tx: mpsc::UnboundedSender<ServerMessage>,
    connection_control: Arc<ConnectionControl>,
    shutdown_notify: Arc<Notify>,
    shutdown_flag: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    // tokio_tungstenite wraps the stream in a 128 KiB write BufWriter by
    // default. CDP traffic is many small (~100-byte) frames, and that buffer
    // adds extra latency per frame. write_buffer_size=0 makes every WS write
    // hit the socket directly. Combined with set_nodelay(true) above, gets
    // per-frame latency on localhost down toward ideal.
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
    let mut cfg = WebSocketConfig::default();
    cfg.write_buffer_size = 0;
    cfg.max_write_buffer_size = 64 << 20;
    let mut websocket_shutdown = Box::pin(shutdown_notify.notified());
    websocket_shutdown.as_mut().enable();
    if shutdown_flag.load(Ordering::Acquire) {
        connection_control.signal_end(ConnectionEnd::Shutdown);
        return Ok(());
    }
    let ws_stream = tokio::select! {
        result = tokio_tungstenite::accept_async_with_config(stream, Some(cfg)) => result?,
        _ = &mut websocket_shutdown => {
            connection_control.signal_end(ConnectionEnd::Shutdown);
            return Ok(());
        }
    };
    info!("WebSocket connected");
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();

    let _ = msg_tx.send(ServerMessage::NewConnection {
        reply_tx: reply_tx.clone(),
    });
    if let Some(init_msg) = reply_rx.recv().await {
        tracing::debug!("Connection init: {}", &init_msg[..init_msg.len().min(100)]);
    }

    let send_control = connection_control.clone();
    let mut send_task = tokio::spawn(async move {
        while let Some(msg) = reply_rx.recv().await {
            if msg.contains("\"__init\"") {
                continue;
            }
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                send_control.signal_end(ConnectionEnd::Closed);
                break;
            }
        }
    });

    loop {
        let msg = tokio::select! {
            msg = ws_receiver.next() => msg,
            end = connection_control.ended() => {
                if end != ConnectionEnd::Open {
                    break;
                }
                continue;
            }
            _ = &mut websocket_shutdown => {
                connection_control.signal_end(ConnectionEnd::Shutdown);
                break;
            }
        };
        let Some(msg) = msg else {
            break;
        };
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("WS read error: {}", e);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                if text.contains("\"Browser.close\"") {
                    if let Ok(req) = serde_json::from_str::<CdpRequest>(&text) {
                        let resp = crate::types::CdpResponse::success(req.id, json!({}), None);
                        if let Ok(json) = serde_json::to_string(&resp) {
                            let _ = reply_tx.send(json);
                        }
                    }
                    connection_control.signal_end(ConnectionEnd::Closed);
                    break;
                }

                if connection_control.signal_teardown_request(&text) {
                    continue;
                }

                if let Some(resp) = fast_path_response(&text) {
                    let _ = reply_tx.send(resp);
                } else {
                    let _ = msg_tx.send(ServerMessage::Cdp(CdpMessage {
                        text: text.to_string(),
                        reply_tx: reply_tx.clone(),
                    }));
                }
            }
            Message::Close(_) => {
                info!("WS closed by client");
                break;
            }
            _ => {}
        }
    }

    connection_control.signal_end(ConnectionEnd::Closed);
    drop(reply_tx);
    drop(msg_tx);
    if tokio::time::timeout(tokio::time::Duration::from_secs(1), &mut send_task)
        .await
        .is_err()
    {
        send_task.abort();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        enqueue_deferred_cdp, handle_fetch_resolution, is_navigate_method,
        fail_paused_interceptions, fail_queued_interceptions,
        merge_cookie_delta, parse_cdp_headers, pop_deferred_for_lifecycle_state,
        reclassify_deferred_for_lifecycle, request_page_id, take_live_pending_navigation,
        CdpMessage, PausedInterception, ServerMessage, MAX_DEFERRED_MESSAGES,
        cdp_processor, ConnectionControl, ConnectionEnd,
        finish_inflight_teardown,
    };
    #[cfg(feature = "render")]
    use super::{pump_and_forward_screencast_frames, pump_live_page_event_loop};
    use obscura_net::{CookieInfo, CookieJar};
    use serde_json::json;
    use std::collections::{HashMap, VecDeque};

    #[test]
    fn interception_cleanup_is_scoped_to_destroyed_page_owners() {
        let (a_paused_tx, mut a_paused_rx) = tokio::sync::oneshot::channel();
        let (b_paused_tx, mut b_paused_rx) = tokio::sync::oneshot::channel();
        let mut paused = HashMap::from([
            ("a-paused".to_string(), PausedInterception {
                page_id: "page-a".to_string(),
                resolver: a_paused_tx,
            }),
            ("b-paused".to_string(), PausedInterception {
                page_id: "page-b".to_string(),
                resolver: b_paused_tx,
            }),
        ]);
        let destroyed = std::collections::HashSet::from(["page-b".to_string()]);
        fail_paused_interceptions(&destroyed, &mut paused);
        assert!(paused.contains_key("a-paused"));
        assert!(matches!(a_paused_rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert!(matches!(b_paused_rx.try_recv(), Ok(obscura_js::ops::InterceptResolution::Fail { .. })));

        let (a_queued_tx, mut a_queued_rx) = tokio::sync::oneshot::channel();
        let (b_queued_tx, mut b_queued_rx) = tokio::sync::oneshot::channel();
        let (_intercept_tx, intercept_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut intercept_rx = Some(intercept_rx);
        let mut queued = VecDeque::from([
            obscura_js::ops::InterceptedRequest {
                owner_page_id: "page-a".to_string(),
                request_id: "a-queued".to_string(),
                url: "https://example.com/a".to_string(),
                method: "GET".to_string(),
                headers: HashMap::new(),
                resource_type: "Fetch".to_string(),
                resolver: a_queued_tx,
            },
            obscura_js::ops::InterceptedRequest {
                owner_page_id: "page-b".to_string(),
                request_id: "b-queued".to_string(),
                url: "https://example.com/b".to_string(),
                method: "GET".to_string(),
                headers: HashMap::new(),
                resource_type: "Fetch".to_string(),
                resolver: b_queued_tx,
            },
        ]);
        fail_queued_interceptions(&destroyed, &mut queued, &mut intercept_rx);
        assert_eq!(queued.len(), 1);
        assert_eq!(queued.front().unwrap().owner_page_id, "page-a");
        assert!(matches!(a_queued_rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert!(matches!(b_queued_rx.try_recv(), Ok(obscura_js::ops::InterceptResolution::Fail { .. })));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_cancels_task_owned_stalled_navigation() {
        std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
        let fixture = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fixture_url = format!("http://{}/stalled", fixture.local_addr().unwrap());
        let (requested_tx, requested_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = fixture.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            let _ = requested_tx.send(());
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        });

        tokio::task::LocalSet::new().run_until(async move {
            let (msg_tx, msg_rx) = tokio::sync::mpsc::unbounded_channel();
            let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
            let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
            let control = std::sync::Arc::new(ConnectionControl::new());
            let processor = tokio::task::spawn_local(cdp_processor(
                msg_rx,
                std::sync::Arc::new(obscura_browser::BrowserContext::new("default".to_string())),
                shutdown.clone(),
                control.clone(),
            ));
            msg_tx.send(ServerMessage::NewConnection { reply_tx: reply_tx.clone() }).unwrap();
            let _ = reply_rx.recv().await.expect("processor init");
            msg_tx.send(ServerMessage::Cdp(CdpMessage {
                text: json!({"id": 1, "method": "Target.createTarget", "params": {"url": "about:blank"}}).to_string(),
                reply_tx: reply_tx.clone(),
            })).unwrap();
            let mut target_id = None;
            let mut session_id = None;
            while target_id.is_none() || session_id.is_none() {
                let message: serde_json::Value = serde_json::from_str(&reply_rx.recv().await.unwrap()).unwrap();
                if message["id"] == 1 {
                    target_id = message["result"]["targetId"].as_str().map(str::to_string);
                }
                if message["method"] == "Target.attachedToTarget" {
                    session_id = message["params"]["sessionId"].as_str().map(str::to_string);
                }
            }
            msg_tx.send(ServerMessage::Cdp(CdpMessage {
                text: json!({
                    "id": 2, "method": "Page.navigate", "sessionId": session_id.unwrap(),
                    "params": {"url": fixture_url},
                }).to_string(),
                reply_tx,
            })).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), requested_rx)
                .await.expect("stalled request was not reached").expect("fixture signal dropped");
            shutdown.notify_waiters();
            tokio::time::timeout(std::time::Duration::from_secs(2), processor)
                .await.expect("shutdown did not cancel navigation").expect("processor task panicked");
            assert_eq!(control.end(), ConnectionEnd::Shutdown);
            assert!(control.active.lock().unwrap().is_none());
            drop(msg_tx);
            drop(target_id);
        }).await;
    }

    #[test]
    fn duplicate_inflight_close_requests_each_receive_a_response() {
        let mut ctx = crate::dispatch::CdpContext::new();
        let page_id = ctx.create_page();
        let context_id = ctx.default_context.id.clone();
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
        finish_inflight_teardown(
            &json!({"id": 1, "method": "Target.closeTarget", "params": {"targetId": page_id}}).to_string(),
            &mut ctx,
            &page_id,
            &context_id,
            &reply_tx,
        );
        finish_inflight_teardown(
            &json!({"id": 2, "method": "Target.closeTarget", "params": {"targetId": page_id}}).to_string(),
            &mut ctx,
            &page_id,
            &context_id,
            &reply_tx,
        );
        let mut responses = HashMap::new();
        let mut destroyed = 0;
        while let Ok(text) = reply_rx.try_recv() {
            let message: serde_json::Value = serde_json::from_str(&text).unwrap();
            if let Some(id) = message["id"].as_u64() {
                responses.insert(id, message);
            } else if message["method"] == "Target.targetDestroyed" {
                destroyed += 1;
            }
        }
        assert_eq!(responses[&1]["result"]["success"], json!(true));
        assert_eq!(responses[&2]["result"]["success"], json!(false));
        assert_eq!(destroyed, 1);
    }

    #[test]
    fn mixed_inflight_teardown_batches_destroy_target_once() {
        fn run(methods: &[&str]) -> (usize, usize) {
            let mut ctx = crate::dispatch::CdpContext::new();
            let context_id = ctx.create_browser_context();
            let page_id = ctx.create_page_in_context(Some(&context_id)).unwrap();
            let session_id = format!("{page_id}-session");
            ctx.sessions.insert(session_id, page_id.clone());
            // Match process_with_interception: the task owns the Page while
            // CdpContext retains its session/context identity.
            ctx.pages.retain(|page| page.id != page_id);
            let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
            for (offset, method) in methods.iter().enumerate() {
                let params = if *method == "Target.closeTarget" {
                    json!({"targetId": page_id})
                } else {
                    json!({"browserContextId": context_id})
                };
                finish_inflight_teardown(
                    &json!({"id": offset + 1, "method": method, "params": params}).to_string(),
                    &mut ctx,
                    &page_id,
                    &context_id,
                    &reply_tx,
                );
            }
            let mut destroyed = 0;
            let mut responses = 0;
            while let Ok(text) = reply_rx.try_recv() {
                let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                destroyed += usize::from(message["method"] == "Target.targetDestroyed");
                responses += usize::from(message.get("id").is_some());
            }
            (destroyed, responses)
        }
        for methods in [
            ["Target.closeTarget", "Target.disposeBrowserContext"],
            ["Target.disposeBrowserContext", "Target.closeTarget"],
            ["Target.disposeBrowserContext", "Target.disposeBrowserContext"],
        ] {
            let (destroyed, responses) = run(&methods);
            assert_eq!(destroyed, 1, "{methods:?}");
            assert_eq!(responses, 2, "{methods:?}");
        }
    }

    #[test]
    fn inflight_teardown_queue_is_bounded() {
        let control = ConnectionControl::new();
        let navigation = obscura_browser::navigation::NavigationControl::new();
        let generation = control.activate(
            "page-1".to_string(),
            "default".to_string(),
            false,
            navigation,
        );
        for id in 0..MAX_DEFERRED_MESSAGES {
            assert!(control.signal_teardown_request(
                &json!({"id": id, "method": "Target.closeTarget", "params": {"targetId": "page-1"}}).to_string()
            ));
        }
        assert!(!control.signal_teardown_request(
            &json!({"id": MAX_DEFERRED_MESSAGES, "method": "Target.closeTarget", "params": {"targetId": "page-1"}}).to_string()
        ));
        assert_eq!(control.finish_navigation(generation).len(), MAX_DEFERRED_MESSAGES);
    }

    fn cookie(name: &str, value: &str) -> CookieInfo {
        CookieInfo {
            name: name.to_string(),
            value: value.to_string(),
            domain: "example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
            same_site: "Lax".to_string(),
            expires: None,
        }
    }

    #[test]
    fn lifecycle_reclassification_preserves_fifo_and_the_shared_queue_bound() {
        let mut ctx = crate::dispatch::CdpContext::new();
        let lifecycle_page = ctx.create_page();
        let foreign_page = ctx.create_page();
        ctx.sessions
            .insert("lifecycle-session".to_string(), lifecycle_page.clone());
        ctx.sessions
            .insert("foreign-session".to_string(), foreign_page);
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
        let message = |id: i64| CdpMessage {
            text: json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": "foreign-session",
                "params": {"expression": id.to_string()},
            })
            .to_string(),
            reply_tx: reply_tx.clone(),
        };
        let mut deferred = VecDeque::from([
            ServerMessage::Cdp(message(1)),
            ServerMessage::Cdp(message(2)),
        ]);
        let mut post_load = VecDeque::new();

        assert!(reclassify_deferred_for_lifecycle(
            &mut deferred,
            &mut post_load,
            &ctx,
            &lifecycle_page,
        ));
        assert!(reclassify_deferred_for_lifecycle(
            &mut deferred,
            &mut post_load,
            &ctx,
            &lifecycle_page,
        ));
        enqueue_deferred_cdp(&mut post_load, deferred.len(), message(3), "full");
        let ids = post_load
            .iter()
            .map(|entry| match entry {
                ServerMessage::Cdp(message) => {
                    serde_json::from_str::<serde_json::Value>(&message.text).unwrap()["id"]
                        .as_i64()
                        .unwrap()
                }
                ServerMessage::NewConnection { .. } => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2, 3]);

        deferred = VecDeque::from([
            ServerMessage::Cdp(message(11)),
            ServerMessage::Cdp(CdpMessage {
                text: json!({
                    "id": 12,
                    "method": "Target.closeTarget",
                    "params": {"targetId": lifecycle_page},
                })
                .to_string(),
                reply_tx: reply_tx.clone(),
            }),
            ServerMessage::Cdp(message(13)),
        ]);
        post_load.clear();
        assert!(reclassify_deferred_for_lifecycle(
            &mut deferred,
            &mut post_load,
            &ctx,
            &lifecycle_page,
        ));
        let owner_close = pop_deferred_for_lifecycle_state(
            &mut deferred,
            &mut post_load,
            true,
        )
        .expect("the owner close remains actionable during lifecycle");
        let ServerMessage::Cdp(owner_close) = owner_close else {
            unreachable!()
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&owner_close.text).unwrap()["id"],
            12,
        );
        let first_foreign = pop_deferred_for_lifecycle_state(
            &mut deferred,
            &mut post_load,
            false,
        )
        .unwrap();
        let second_foreign = pop_deferred_for_lifecycle_state(
            &mut deferred,
            &mut post_load,
            false,
        )
        .unwrap();
        let ids = [first_foreign, second_foreign].map(|entry| match entry {
            ServerMessage::Cdp(message) => {
                serde_json::from_str::<serde_json::Value>(&message.text).unwrap()["id"]
                    .as_i64()
                    .unwrap()
            }
            ServerMessage::NewConnection { .. } => unreachable!(),
        });
        assert_eq!(ids, [11, 13]);

        post_load.clear();
        for id in 0..MAX_DEFERRED_MESSAGES {
            post_load.push_back(ServerMessage::Cdp(message(id as i64)));
        }
        deferred.push_back(ServerMessage::Cdp(message(999)));
        assert!(reclassify_deferred_for_lifecycle(
            &mut deferred,
            &mut post_load,
            &ctx,
            &lifecycle_page,
        ));
        assert_eq!(post_load.len(), MAX_DEFERRED_MESSAGES);
        let rejected: serde_json::Value = serde_json::from_str(
            &reply_rx.try_recv().expect("the overflow command must receive an error"),
        )
        .unwrap();
        assert_eq!(rejected["id"], 999);
        assert_eq!(rejected["error"]["code"], -32000);

        deferred.clear();
        post_load.clear();
        for id in 0..(MAX_DEFERRED_MESSAGES - 1) {
            post_load.push_back(ServerMessage::Cdp(message(id as i64)));
        }
        enqueue_deferred_cdp(
            &mut deferred,
            post_load.len(),
            message(1_000),
            "full",
        );
        enqueue_deferred_cdp(
            &mut deferred,
            post_load.len(),
            message(1_001),
            "full",
        );
        assert_eq!(deferred.len() + post_load.len(), MAX_DEFERRED_MESSAGES);
        let rejected: serde_json::Value = serde_json::from_str(
            &reply_rx.try_recv().expect(
                "a navigation-time enqueue must include the existing post-load queue",
            ),
        )
        .unwrap();
        assert_eq!(rejected["id"], 1_001);
        assert_eq!(rejected["error"]["code"], -32000);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn page_runtime_and_shutdown_progress_under_silence_and_command_flood() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
                let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
                let request_shutdown = shutdown.clone();
                let default_context = crate::dispatch::CdpContext::new().default_context;
                let connection_control = std::sync::Arc::new(super::ConnectionControl::new());
                let processor = tokio::task::spawn_local(super::cdp_processor(
                    server_rx,
                    default_context,
                    shutdown,
                    connection_control,
                ));

                server_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: reply_tx.clone(),
                    })
                    .unwrap();
                let init = reply_rx.recv().await.expect("processor init");
                assert!(init.contains("__init"));

                let send = |value: serde_json::Value| {
                    server_tx
                        .send(super::ServerMessage::Cdp(super::CdpMessage {
                            text: value.to_string(),
                            reply_tx: reply_tx.clone(),
                        }))
                        .unwrap();
                };
                send(json!({
                    "id": 1,
                    "method": "Target.createTarget",
                    "params": {"url": "about:blank"},
                }));

                let mut session_id = None;
                loop {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("create target response timeout")
                        .expect("create target response channel"),
                    )
                    .unwrap();
                    if session_id.is_none() {
                        session_id = value["params"]["sessionId"]
                            .as_str()
                            .map(str::to_string);
                    }
                    if value["id"] == 1 {
                        break;
                    }
                }
                let session_id = session_id.expect("attached page session");

                send(json!({
                    "id": 2,
                    "method": "Runtime.evaluate",
                    "sessionId": session_id,
                    "params": {
                        "expression": "(() => { setTimeout(() => globalThis.__autonomousDone = 'yes', 40); return 'armed'; })()",
                        "returnByValue": true,
                    },
                }));
                loop {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("timer arm response timeout")
                        .expect("timer arm response channel"),
                    )
                    .unwrap();
                    if value["id"] == 2 {
                        break;
                    }
                }

                // This is deliberately host/client time. No CDP message is sent
                // while the timeout becomes due; Chrome's renderer still runs,
                // and Obscura's connection-owned page pump must do the same.
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;

                send(json!({
                    "id": 3,
                    "method": "Runtime.evaluate",
                    "sessionId": session_id,
                    "params": {
                        "expression": "globalThis.__autonomousDone || 'missing'",
                        "returnByValue": true,
                    },
                }));
                loop {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("timer observation response timeout")
                        .expect("timer observation response channel"),
                    )
                    .unwrap();
                    if value["id"] == 3 {
                        assert_eq!(value["result"]["result"]["value"], "yes");
                        break;
                    }
                }

                send(json!({
                    "id": 4,
                    "method": "Runtime.enable",
                    "sessionId": session_id,
                    "params": {},
                }));
                while serde_json::from_str::<serde_json::Value>(
                    &reply_rx.recv().await.expect("Runtime.enable response channel"),
                )
                .unwrap()["id"] != 4
                {}
                send(json!({
                    "id": 5,
                    "method": "Runtime.evaluate",
                    "sessionId": session_id,
                    "params": {
                        "expression": "setTimeout(() => console.log('__pump_fairness_marker__'), 0)",
                    },
                }));
                while serde_json::from_str::<serde_json::Value>(
                    &reply_rx.recv().await.expect("timer response channel"),
                )
                .unwrap()["id"] != 5
                {}

                const FLOOD: i64 = 4_096;
                for id in 10_000..10_000 + FLOOD {
                    send(json!({"id": id, "method": "Browser.getVersion", "params": {}}));
                }
                let mut saw_last_flood_response = false;
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    loop {
                        let value: serde_json::Value = serde_json::from_str(
                            &reply_rx.recv().await.expect("flood response channel"),
                        )
                        .unwrap();
                        saw_last_flood_response |= value["id"] == 10_000 + FLOOD - 1;
                        if value["method"] == "Runtime.consoleAPICalled"
                            && value["params"]["args"][0]["value"]
                                == "__pump_fairness_marker__"
                        {
                            break;
                        }
                    }
                })
                .await
                .expect("continuous commands starved the page event loop");
                assert!(
                    !saw_last_flood_response,
                    "the page pump ran only after draining the unbounded command backlog",
                );

                for id in 20_000..20_000 + FLOOD {
                    send(json!({"id": id, "method": "Browser.getVersion", "params": {}}));
                }
                request_shutdown.notify_one();
                tokio::time::timeout(std::time::Duration::from_secs(2), processor)
                    .await
                    .expect("continuous commands starved processor shutdown")
                    .expect("processor task");
            })
            .await;
    }

    #[test]
    fn cookie_delta_merges_changes_without_reverting_other_connections() {
        let destination = CookieJar::new();
        destination.set_cookies_from_cdp(vec![cookie("sid", "newer"), cookie("other", "kept")]);
        let initial = vec![cookie("sid", "old"), cookie("removed", "old")];
        let current = vec![cookie("sid", "old"), cookie("added", "value")];

        merge_cookie_delta(&destination, &initial, &current);

        let cookies = destination.get_all_cookies();
        assert!(cookies.iter().any(|c| c.name == "sid" && c.value == "newer"));
        assert!(cookies.iter().any(|c| c.name == "other"));
        assert!(cookies.iter().any(|c| c.name == "added"));
        assert!(!cookies.iter().any(|c| c.name == "removed"));
    }

    // Issue #363: only an exact Page.navigate may take the spawn-and-defer
    // navigation path. A substring match also caught Page.navigateToHistoryEntry
    // (goBack / goForward), which has no `url` param, so it was misrouted into
    // the raw-navigate path and failed with "Invalid URL" instead of reaching
    // its real handler.
    #[test]
    fn only_exact_page_navigate_routes_as_navigation() {
        assert!(is_navigate_method(
            r#"{"id":1,"method":"Page.navigate","params":{"url":"https://example.com"}}"#
        ));
        assert!(!is_navigate_method(
            r#"{"id":2,"method":"Page.navigateToHistoryEntry","params":{"entryId":0}}"#
        ));
    }

    // A Runtime.evaluate whose expression merely contains the literal
    // "Page.navigate" must not be misrouted, and malformed input is not a
    // navigation.
    #[test]
    fn unrelated_methods_do_not_route_as_navigation() {
        assert!(!is_navigate_method(
            r#"{"id":3,"method":"Runtime.evaluate","params":{"expression":"'Page.navigate'"}}"#
        ));
        assert!(!is_navigate_method("not json"));
    }

    #[test]
    fn legacy_send_message_wrapper_resolves_its_effective_page() {
        let mut ctx = crate::dispatch::CdpContext::new();
        ctx.sessions
            .insert("legacy-session".to_string(), "page-2".to_string());
        let request: crate::types::CdpRequest = serde_json::from_value(json!({
            "id": 4,
            "method": "Target.sendMessageToTarget",
            "params": {
                "sessionId": "legacy-session",
                "message": "{\"id\":5,\"method\":\"Runtime.evaluate\",\"params\":{\"expression\":\"1\"}}"
            }
        }))
        .unwrap();
        assert_eq!(request_page_id(&request, &ctx).as_deref(), Some("page-2"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detached_navigation_owner_retargets_autonomous_navigation() {
        let mut ctx = crate::dispatch::CdpContext::new();
        let page_id = ctx.create_page();
        ctx.get_page_mut(&page_id)
            .unwrap()
            .navigate_with_wait(
                "data:text/html,current",
                obscura_browser::lifecycle::WaitUntil::Load,
            )
            .await
            .unwrap();
        ctx.sessions
            .insert("detached-session".to_string(), page_id.clone());
        ctx.sessions
            .insert("remaining-session".to_string(), page_id.clone());
        ctx.navigation_sessions
            .insert(page_id.clone(), Some("detached-session".to_string()));

        crate::domains::target::handle(
            "detachFromTarget",
            &json!({"sessionId": "detached-session"}),
            &mut ctx,
            &None,
        )
        .await
        .unwrap();
        ctx.get_page_mut(&page_id)
            .unwrap()
            .evaluate("location.href = 'data:text/html,replacement'");

        let pending = take_live_pending_navigation(&ctx).expect("autonomous navigation");
        assert_eq!(pending.0, "remaining-session");
        assert_eq!(pending.1, "data:text/html,replacement");
    }

    // Issue #365: Fetch.continueRequest header overrides must be parsed from the
    // CDP `[{name, value}]` list so they can be applied to the outgoing request.
    #[test]
    fn parse_cdp_headers_reads_name_value_pairs() {
        let params = json!({
            "headers": [
                {"name": "X-A", "value": "1"},
                {"name": "X-B", "value": "2"},
            ]
        });
        let headers = parse_cdp_headers(&params).expect("headers present");
        assert_eq!(headers.get("X-A").map(String::as_str), Some("1"));
        assert_eq!(headers.get("X-B").map(String::as_str), Some("2"));
    }

    // No `headers` field means "leave the request's headers untouched", which is
    // None, not an empty map that would clear them.
    #[test]
    fn parse_cdp_headers_absent_is_none() {
        assert!(parse_cdp_headers(&json!({"url": "https://example.com"})).is_none());
    }

    #[test]
    fn fetch_resolution_is_handled_once_by_the_outer_processor() {
        let (resolution_tx, mut resolution_rx) = tokio::sync::oneshot::channel();
        let mut paused = HashMap::from([("request-1".to_string(), PausedInterception {
            page_id: "page-1".to_string(),
            resolver: resolution_tx,
        })]);
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut ctx = crate::dispatch::CdpContext::new();

        assert!(handle_fetch_resolution(
            r#"{"id":17,"method":"Fetch.continueRequest","params":{"requestId":"request-1"}}"#,
            &mut ctx,
            &reply_tx,
            &mut paused,
        ));
        assert!(matches!(
            resolution_rx.try_recv(),
            Ok(obscura_js::ops::InterceptResolution::Continue { .. })
        ));
        let response: serde_json::Value =
            serde_json::from_str(&reply_rx.try_recv().expect("one command response")).unwrap();
        assert_eq!(response["id"], 17);
        assert!(reply_rx.try_recv().is_err(), "must not emit a duplicate response");
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn autonomous_screencast_pumps_timers_and_retains_backpressured_damage() {
        let mut ctx = crate::dispatch::CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id.clone());
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((96.0, 64.0));
        crate::domains::page::handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0;width:96px;height:64px;background:red'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate screencast fixture");
        ctx.pending_events.clear();
        crate::domains::page::handle(
            "startScreencast",
            &json!({}),
            &mut ctx,
            &session,
        )
        .await
        .expect("start screencast");
        let stream_id = ctx
            .pending_events
            .iter()
            .find(|event| event.method == "Page.screencastFrame")
            .and_then(|event| event.params["sessionId"].as_i64())
            .expect("initial stream id");
        ctx.pending_events.clear();

        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
        ctx.get_session_page_mut(&session)
            .expect("page")
            .evaluate(
                "setTimeout(() => document.body.setAttribute('style', 'margin:0;width:96px;height:64px;background:green'), 0)",
            );
        pump_live_page_event_loop(&mut ctx, None).await.unwrap();
        pump_and_forward_screencast_frames(&mut ctx, Some(&reply_tx)).await;
        let first_update: serde_json::Value = serde_json::from_str(
            &reply_rx.try_recv().expect("timer mutation should emit a frame"),
        )
        .unwrap();
        assert_eq!(first_update["method"], "Page.screencastFrame");
        assert_eq!(first_update["params"]["sessionId"], stream_id);
        assert!(reply_rx.try_recv().is_err());
        assert_eq!(ctx.screencasts[&session_id].frames_in_flight, 2);

        // The second mutation is pumped while the two-frame acknowledgement
        // window is full. It must not emit yet, but its damage must remain
        // pending and appear immediately after capacity is returned.
        ctx.get_session_page_mut(&session)
            .expect("page")
            .evaluate(
                "setTimeout(() => document.body.setAttribute('style', 'margin:0;width:96px;height:64px;background:blue'), 0)",
            );
        pump_live_page_event_loop(&mut ctx, None).await.unwrap();
        pump_and_forward_screencast_frames(&mut ctx, Some(&reply_tx)).await;
        assert!(reply_rx.try_recv().is_err());
        assert!(ctx.screencasts[&session_id].autonomous_frame_pending);

        crate::domains::page::handle(
            "screencastFrameAck",
            &json!({"sessionId": stream_id}),
            &mut ctx,
            &session,
        )
        .await
        .expect("ack current frame");
        pump_and_forward_screencast_frames(&mut ctx, Some(&reply_tx)).await;
        let after_ack: serde_json::Value = serde_json::from_str(
            &reply_rx
                .try_recv()
                .expect("backpressured damage should emit after ack"),
        )
        .unwrap();
        assert_eq!(after_ack["method"], "Page.screencastFrame");
        assert_eq!(after_ack["params"]["sessionId"], stream_id);
        assert!(!ctx.screencasts[&session_id].autonomous_frame_pending);
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    async fn autonomous_screencast_observes_raf_visual_mutations() {
        let mut ctx = crate::dispatch::CdpContext::new();
        let page_id = ctx.create_page();
        let session_id = format!("{page_id}-session");
        ctx.sessions.insert(session_id.clone(), page_id);
        let session = Some(session_id.clone());
        ctx.get_session_page_mut(&session)
            .expect("page")
            .set_viewport((96.0, 64.0));
        crate::domains::page::handle(
            "navigate",
            &json!({
                "url": "data:text/html,<html style='margin:0'><body style='margin:0;width:96px;height:64px;background:red'></body></html>",
                "waitUntil": "load",
            }),
            &mut ctx,
            &session,
        )
        .await
        .expect("navigate visual-damage fixture");
        ctx.pending_events.clear();
        crate::domains::page::handle(
            "startScreencast",
            &json!({}),
            &mut ctx,
            &session,
        )
        .await
        .expect("start screencast");
        let initial = ctx
            .pending_events
            .iter()
            .find(|event| event.method == "Page.screencastFrame")
            .expect("initial frame");
        let stream_id = initial.params["sessionId"].as_i64().unwrap();
        let initial_data = initial.params["data"].as_str().unwrap().to_string();
        ctx.pending_events.clear();
        crate::domains::page::handle(
            "screencastFrameAck",
            &json!({"sessionId": stream_id}),
            &mut ctx,
            &session,
        )
        .await
        .expect("ack initial frame");

        // Bypass CDP dispatch after scheduling the callback. The only path
        // which can deliver and capture this update is the active stream's
        // periodic event-loop/render pump.
        ctx.get_session_page_mut(&session)
            .expect("page")
            .evaluate(
                "requestAnimationFrame(() => document.body.setAttribute('style','margin:0;width:96px;height:64px;background:lime'))",
            );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        pump_live_page_event_loop(&mut ctx, None).await.unwrap();
        pump_and_forward_screencast_frames(&mut ctx, None).await;
        let raf_frame = ctx
            .pending_events
            .iter()
            .find(|event| event.method == "Page.screencastFrame")
            .expect("RAF visual mutation must autonomously emit a frame");
        assert_ne!(
            raf_frame.params["data"].as_str().unwrap(),
            initial_data,
            "RAF-driven paint must capture the updated visible state"
        );
    }
}
