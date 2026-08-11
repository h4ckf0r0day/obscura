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
use crate::registry::TargetRegistry;

// PR #36 comment 4341743194: the deferral queue in `process_with_interception`
// must be bounded so a stalled navigation cannot OOM the process. When the cap
// is reached we return an explicit error response rather than silently dropping.
const MAX_DEFERRED_MESSAGES: usize = 256;

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
use crate::types::CdpRequest;

struct CdpMessage {
    text: String,
    reply_tx: mpsc::UnboundedSender<String>,
}

enum ServerMessage {
    Cdp(CdpMessage),
    NewConnection {
        reply_tx: mpsc::UnboundedSender<String>,
    },
    /// A session-scoped CDP command that targets a page owned by ANOTHER
    /// connection. Thread-per-connection (#430) confines the live `Page` and
    /// its V8 isolate to the owning connection's thread, so the caller's
    /// processor forwards the original request here; the owner executes it
    /// against its own session for `page_id` and streams the response (and
    /// any events) back through `reply_tx`, rewriting the session id to the
    /// caller's so the client can correlate the reply.
    RemoteExec {
        text: String,
        page_id: String,
        reply_tx: mpsc::UnboundedSender<String>,
    },
    /// The client's WebSocket closed. Pages created by this connection are
    /// NOT torn down: they stay alive on this thread (thread-per-connection
    /// #430) and remain visible in the global registry and drivable from
    /// other connections via `RemoteExec`. The processor keeps serving those
    /// commands and exits once it owns no pages, unwinding the thread.
    ConnectionClosed,
}

/// Process-wide map from page target id to the owning connection's processor
/// channel. Each connection's processor keeps it current after every message
/// it handles (register its own pages, drop ids it no longer owns); other
/// connections consult it when a session-scoped command must be routed to the
/// page's real owner.
type RemoteOwners =
    Arc<std::sync::Mutex<HashMap<String, mpsc::UnboundedSender<ServerMessage>>>>;

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
    std_listener
        .set_nonblocking(false)
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

    // Process-wide page target registry (issue #544). Every connection shares
    // this one so Target.getTargets on any WebSocket and /json/list on the
    // HTTP accept thread see the same live pages; page ids minted from it are
    // unique across the whole server.
    let target_registry = TargetRegistry::default();

    // Process-wide map from page id to the owning connection's processor
    // channel, used to route session-scoped commands to remote pages.
    let remote_owners: RemoteOwners = Arc::new(std::sync::Mutex::new(HashMap::new()));

    // Ctrl-C / graceful shutdown coordination.
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let shutdown_notify = Arc::new(Notify::new());

    // Dedicated accept thread: drains the kernel backlog immediately and
    // handles HTTP endpoints (/json/version, /json, /json/protocol) with
    // blocking I/O so they never contend with the LocalSet's V8 work.
    let accept_flag = shutdown_flag.clone();
    let http_registry = target_registry.clone();
    std::thread::Builder::new()
        .name("obscura-cdp-accept".into())
        .spawn(move || {
            for stream in std_listener.incoming() {
                if accept_flag.load(Ordering::Relaxed) {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        if let Err(e) = accept_dispatch(stream, port, &ws_tx, &http_registry) {
                            if !format!("{}", e).contains("close") {
                                error!("Accept dispatch error: {}", e);
                            }
                        }
                    }
                    Err(e) => error!("Accept error: {}", e),
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

    // Accept loop: hand each WebSocket connection to its own OS thread so its
    // pages' isolates live on a dedicated thread.
    loop {
        let stream = tokio::select! {
            stream = ws_rx.recv() => stream,
            _ = shutdown_notify.notified() => None,
        };
        let stream = match stream {
            Some(s) => s,
            None => break,
        };
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
        run_connection(
            stream,
            shared_ctx.clone(),
            persistence_ctx.clone(),
            persistence_lock.clone(),
            shutdown_notify.clone(),
            live_connections.clone(),
            remote_owners.clone(),
            target_registry.clone(),
        );
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

/// Run one WebSocket connection on its own OS thread: a `current_thread` tokio
/// runtime + `LocalSet` hosting this connection's `cdp_processor` (with its own
/// `CdpContext` and pages) and its frame reader. Confining a connection's pages
/// to one thread is what removes the #430 abort; the interception handshake and
/// the nav `spawn_local` all stay on this one thread, so no cross-thread V8
/// plumbing is needed.
fn run_connection(
    std_stream: std::net::TcpStream,
    context_template: Arc<obscura_browser::BrowserContext>,
    persistence_context: Arc<obscura_browser::BrowserContext>,
    persistence_lock: Arc<std::sync::Mutex<()>>,
    shutdown_notify: Arc<Notify>,
    live_connections: Arc<AtomicUsize>,
    remote_owners: RemoteOwners,
    target_registry: TargetRegistry,
) {
    // Releases the slot reserved by the accept loop exactly once: on explicit
    // `release()` (the normal client-disconnect path, where the thread keeps
    // living as the host of the pages it created) or on drop (early return /
    // panic). The cap bounds ACTIVE clients, so a connection whose socket is
    // gone frees its slot immediately even while its thread persists; without
    // the flag, dropping the guard would double-decrement.
    struct SlotGuard(Arc<AtomicUsize>, bool);
    impl SlotGuard {
        fn release(&mut self) {
            if !self.1 {
                self.0.fetch_sub(1, Ordering::AcqRel);
                self.1 = true;
            }
        }
    }
    impl Drop for SlotGuard {
        fn drop(&mut self) {
            self.release();
        }
    }

    let slot = live_connections.clone();
    let spawned = std::thread::Builder::new()
        .name("obscura-cdp-conn".into())
        .spawn(move || {
            let mut slot_guard = SlotGuard(slot, false);
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
                let tokio_stream = match TcpStream::from_std(std_stream) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("TcpStream::from_std failed: {}", e);
                        return;
                    }
                };
                let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ServerMessage>();
                let processor = tokio::task::spawn_local(cdp_processor(
                    msg_rx,
                    default_context,
                    shutdown_notify,
                    target_registry,
                    msg_tx.clone(),
                    remote_owners,
                ));
            if let Err(e) = handle_connection_ws(tokio_stream, msg_tx.clone()).await {
                error!("WebSocket connection error: {}", e);
            }
            // The client is gone. Release the accept-loop slot now: the cap
            // counts active clients, and this thread may keep living as the
            // owner of the pages the connection created.
            slot_guard.release();
            // The pages this connection created survive its disconnect
            // (Chrome semantics): they stay alive on this thread and remain
            // visible in the global registry and drivable from other
            // connections until explicitly closed or process shutdown. Tell
            // the processor the socket is gone; it keeps serving RemoteExec
            // commands while it owns pages and exits once its last page is
            // closed, which unwinds this thread.
            let _ = msg_tx.send(ServerMessage::ConnectionClosed);
            drop(msg_tx);
            let _ = processor.await;
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
    }
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
    use std::io::Write;
    let mut stream = stream;
    let _ = stream.set_nonblocking(false);
    let _ = stream.write_all(CONNECTION_LIMIT_RESPONSE.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

const HTTP_PEEK_BUF: usize = 4096;
const WS_PEEK_BUF: usize = 4;

/// Dispatch a freshly-accepted TCP connection on the dedicated accept thread.
///
/// Peek at the first bytes to decide HTTP vs WebSocket:
/// - HTTP (`GET /json/*`): serve synchronously via blocking I/O so the
///   response is never stalled by the LocalSet.
/// - WebSocket: set non-blocking, convert to tokio `TcpStream`, and forward
///   to the LocalSet for CDP processing.
fn accept_dispatch(
    stream: std::net::TcpStream,
    port: u16,
    ws_tx: &mpsc::Sender<std::net::TcpStream>,
    target_registry: &TargetRegistry,
) -> anyhow::Result<()> {
    let mut buf = [0u8; WS_PEEK_BUF];
    let n = stream.peek(&mut buf)?;

    if n >= 4 && &buf == b"GET " {
        let mut peek_buf = [0u8; HTTP_PEEK_BUF];
        let n = stream.peek(&mut peek_buf)?;
        let line = String::from_utf8_lossy(&peek_buf[..n]);

        let endpoint = if line.contains("/json/version") {
            Some("version")
        } else if line.contains("/json/list") || line.contains("/json\r\n") || line.contains("/json HTTP") {
            Some("list")
        } else if line.contains("/json/protocol") {
            Some("protocol")
        } else {
            None
        };

        if let Some(ep) = endpoint {
            return handle_http_json_blocking(stream, port, ep, target_registry);
        }
        // Fall through: GET request that isn't a /json endpoint → treat as
        // WebSocket upgrade (Chromium DevTools clients issue GET with
        // Upgrade: websocket).
    }

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
    target_registry: &TargetRegistry,
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
        // The list must mirror the live target registry (issue #544), not a
        // hardcoded synthetic about:blank page. Every page any connection has
        // created or navigated shows up here with its current url/title.
        "list" => {
            let list: Vec<serde_json::Value> = target_registry
                .all()
                .into_iter()
                .map(|target| {
                    json!({
                        "description": "",
                        "devtoolsFrontendUrl": "",
                        "id": target.target_id,
                        "title": target.title,
                        "type": "page",
                        "url": target.url,
                        "webSocketDebuggerUrl": format!(
                            "ws://127.0.0.1:{}/devtools/page/{}",
                            port, target.target_id
                        ),
                    })
                })
                .collect();
            serde_json::to_string_pretty(&list)?
        }
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
    target_registry: TargetRegistry,
    my_msg_tx: mpsc::UnboundedSender<ServerMessage>,
    remote_owners: RemoteOwners,
) {
    let mut ctx = CdpContext::new_with_shared_context_and_registry(default_context, target_registry);
    let (itx, irx) = mpsc::unbounded_channel::<obscura_js::ops::InterceptedRequest>();
    ctx.intercept_tx = Some(itx);
    let mut intercept_rx: Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>> = Some(irx);
    let mut intercepted_paused: HashMap<String, tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>> = HashMap::new();

    // Issue #19 follow-up: messages deferred from inside
    // `process_with_interception` because routing them through
    // `process_cdp_message → dispatch` while a nav was in flight would have
    // tripped V8's TryGetCurrent invariant. Drained at the top of each
    // outer iteration so they get processed sequentially with no other nav
    // in flight.
    let mut deferred: std::collections::VecDeque<ServerMessage> =
        std::collections::VecDeque::new();

    // Graceful shutdown: one signal watcher on the accept side flips the flag
    // and calls `notify_waiters()`. Polled once here (via the select! below) it
    // registers and stays registered across iterations, so a later
    // `notify_waiters()` wakes this processor even while it is mid-dispatch.
    let mut shutdown = Box::pin(shutdown_notify.notified());
    // Chromium's PageHandler receives compositor video frames continuously.
    // Obscura has no separate compositor thread yet, so active screencasts get
    // a bounded 30 Hz opportunity on this connection's owning LocalSet.
    let mut screencast_tick = tokio::time::interval(tokio::time::Duration::from_millis(33));
    screencast_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut connection_reply_tx: Option<mpsc::UnboundedSender<String>> = None;
    // True once the client's WebSocket has closed. Pages created by this
    // connection survive the disconnect (they stay drivable via RemoteExec
    // from other connections); the processor exits once it owns no pages.
    let mut ws_closed = false;
    // A real browser renderer continues servicing timers, networking, posted
    // tasks, and animation callbacks while its DevTools client is silent. Keep
    // one wake-driven deno_core turn armed after work may have been scheduled;
    // the future parks on the runtime's own waker and is cancelled whenever a
    // higher-priority protocol command arrives. Full idle disarms it until the
    // next command/navigation, so static pages consume no polling budget.
    let mut runtime_pump_armed = false;
    let mut runtime_pump_error_streak = 0_u8;

    loop {
        // Drain any deferred messages from the previous interception window
        // before pulling new ones off the wire. Each is processed with no
        // nav-task spawn_local in flight, so this connection's only entered
        // Isolate is the one dispatch is about to touch.
        let msg = if let Some(d) = deferred.pop_front() {
            Some(d)
        } else {
            let screencast_active = has_active_screencast(&ctx);
            let live_page_route = ctx
                .pages
                .iter()
                .find(|page| page.has_js())
                .and_then(|page| {
                    ctx.sessions
                        .iter()
                        .find(|(_, page_id)| *page_id == &page.id)
                        .map(|(session_id, _)| (session_id.clone(), page.frame_id.clone()))
                });
            let has_intercept_rx = intercept_rx.is_some();
            tokio::select! {
                biased;
                msg = rx.recv() => match msg {
                    Some(m) => Some(m),
                    None => break,
                },
                _ = &mut shutdown => {
                    tracing::info!("Shutdown signal received (connection processor)");
                    break;
                },
                pump_result = pump_live_page_event_loop(&mut ctx), if runtime_pump_armed => {
                    match pump_result {
                        Ok(reached_idle) => {
                            runtime_pump_error_streak = 0;
                            runtime_pump_armed = !reached_idle;
                        }
                        Err(error) => {
                            runtime_pump_error_streak = runtime_pump_error_streak.saturating_add(1);
                            runtime_pump_armed = runtime_pump_error_streak <= 3
                                && ctx.pages.iter().any(|page| page.has_js());
                            tracing::warn!("autonomous page task failed: {error}");
                            tokio::task::yield_now().await;
                        }
                    }
                    sync_live_page_network_events(&mut ctx);
                    dispatch::drain_binding_calls(&mut ctx);
                    forward_pending_events(&mut ctx, connection_reply_tx.as_ref());
                    if let (Some(reply_tx), Some((session_id, url, method, body))) = (
                        connection_reply_tx.as_ref(),
                        take_live_pending_navigation(&ctx),
                    ) {
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
                            &mut intercepted_paused,
                            &mut deferred,
                            false,
                        )
                        .await;
                        runtime_pump_armed = ctx.pages.iter().any(|page| page.has_js());
                    }
                    None
                },
                Some(intercepted) = async {
                    if let Some(ref mut receiver) = intercept_rx {
                        receiver.recv().await
                    } else {
                        std::future::pending().await
                    }
                }, if has_intercept_rx => {
                    if let (Some((session_id, frame_id)), Some(reply_tx)) =
                        (live_page_route.as_ref(), connection_reply_tx.as_ref())
                    {
                        emit_intercepted_request(
                            intercepted,
                            frame_id,
                            Some(session_id.clone()),
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
        let is_connection_closed = matches!(msg, ServerMessage::ConnectionClosed);

        match msg {
            ServerMessage::NewConnection { reply_tx } => {
                connection_reply_tx = Some(reply_tx.clone());
                // Issue #543: a browser-level client that connects and immediately
                // calls Target.getTargets must see the existing page targets (the
                // CDP spec makes targets globally visible). Without this, a fresh
                // connection's CdpContext has an empty `pages` registry and
                // getTargets returns [], which breaks puppeteer/playwright-style
                // clients before they can call Target.createTarget/attachToTarget.
                // Mirror the interception path below, which already creates a
                // page + session per new connection.
                let pid = ctx.create_page();
                let sid = format!("{pid}-session");
                ctx.sessions.insert(sid.clone(), pid.clone());
                let _ = reply_tx.send(
                    json!({"__init": true, "pageId": pid, "sessionId": sid}).to_string(),
                );
            }
            ServerMessage::Cdp(cdp_msg) => {
                // Cross-connection session routing: a session-scoped command
                // whose session routes to a page owned by ANOTHER connection
                // is forwarded to that connection's processor (remote exec).
                // The live Page and its V8 isolate stay on the owner's thread
                // (#430); only the routing crosses connections.
                if forward_remote_cdp(&cdp_msg, &ctx, &remote_owners) {
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
                    process_with_interception(
                        &cdp_msg.text, &mut ctx, &cdp_msg.reply_tx, &mut rx,
                        &mut intercept_rx, &mut intercepted_paused,
                        &mut deferred, true,
                    ).await;
                } else {
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
            ServerMessage::RemoteExec {
                text,
                page_id,
                reply_tx,
            } => {
                handle_remote_exec(
                    &mut ctx,
                    &text,
                    &page_id,
                    &reply_tx,
                    &mut rx,
                    &mut intercept_rx,
                    &mut intercepted_paused,
                    &mut deferred,
                )
                .await;
            }
            ServerMessage::ConnectionClosed => {
                // The client socket is gone, but the pages this connection
                // created stay alive on this thread (#430): they remain in
                // the global registry and are still drivable from other
                // connections via RemoteExec. Stop pumping this connection's
                // own event stream — there is no socket to receive it — drop
                // any screencasts bound to the dead session, and drop any
                // events left queued for the departed client. The autonomous
                // JS pump is disarmed at the re-arm site below (not here: the
                // post-match re-arm runs after every message), so orphaned
                // pages are frozen until a later command re-arms the pump.
                ws_closed = true;
                connection_reply_tx = None;
                ctx.pending_events.clear();
                #[cfg(feature = "render")]
                ctx.screencasts.clear();
            }
        }

        // Dispatch may have created a page or scheduled new asynchronous work.
        // A single live isolate is the connection's current active target; the
        // pump will park cheaply if its next task is a distant timer.
        // Disconnect disarms the autonomous pump (the orphaned pages freeze
        // until driven again); any later message — including a RemoteExec from
        // another connection driving the surviving pages — re-arms it.
        runtime_pump_armed = if is_connection_closed {
            false
        } else {
            ctx.pages.iter().any(|page| page.has_js())
        };
        runtime_pump_error_streak = 0;
        sync_remote_ownership(&ctx, &remote_owners, &my_msg_tx);

        // An orphaned connection (client disconnected) exits once it owns no
        // pages: nothing is left to serve, so the thread unwinds and the
        // connection's slot (already released) and owner-map entries settle.
        // Pages are removed from the registry as they are closed, so no
        // targets are stranded by the exit. Any final response for the
        // message that dropped the last page was already queued to the
        // caller's channel before this point, so its (earlier-woken) rewrite
        // task delivers it before this LocalSet tears down.
        if ws_closed && ctx.pages.is_empty() {
            break;
        }
    }

    // The connection thread merges this context's cookie delta into the
    // persistence template after the processor stops.
    //
    // This connection is going away: unregister every page it owned so other
    // connections stop routing remote commands to a dead processor.
    {
        let mut map = remote_owners.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, tx| !tx.same_channel(&my_msg_tx));
    }
    let _ = &ctx;
}

/// Whether a CDP message's session routes to a page owned by another
/// connection, and if so, forward it to that connection's processor (or fail
/// it when the target is gone). Returns true when the message was fully
/// handled (forwarded, or answered with an error) and must not be dispatched
/// locally.
fn forward_remote_cdp(
    cdp_msg: &CdpMessage,
    ctx: &CdpContext,
    remote_owners: &RemoteOwners,
) -> bool {
    let Ok(req) = serde_json::from_str::<CdpRequest>(&cdp_msg.text) else {
        return false;
    };

    // Browser-level `Target.closeTarget` (no sessionId) is browser-global: it
    // may name a target owned by another connection. Route it to the owner so
    // the live `Page` is torn down for real — its isolate lives on the owner's
    // thread (#430), so a local tombstone alone would hide the target from
    // getTargets//json/list but leak the Page until the owner next syncs,
    // which an orphaned owner (disconnected client) never does.
    if req.session_id.is_none() && req.method == "Target.closeTarget" {
        let Some(target_id) = req.params.get("targetId").and_then(|v| v.as_str()) else {
            return false;
        };
        if ctx.get_page(target_id).is_some() {
            return false; // owned here: normal local dispatch tears it down
        }
        if !ctx.registry.all().iter().any(|t| t.target_id == target_id) {
            return false; // unknown target: local dispatch surfaces the error
        }
        let Some(owner_tx) = remote_owners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(target_id)
            .cloned()
        else {
            // Owner not registered yet: fall back to local dispatch, which
            // tombstones the target so it still disappears everywhere.
            return false;
        };
        let forwarded = ServerMessage::RemoteExec {
            text: cdp_msg.text.clone(),
            page_id: target_id.to_string(),
            reply_tx: cdp_msg.reply_tx.clone(),
        };
        if owner_tx.send(forwarded).is_err() {
            // Owner is gone; fail the command rather than pretending success.
            let resp = crate::types::CdpResponse::error(
                req.id,
                -32000,
                "Target not found".to_string(),
                req.session_id.clone(),
            );
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = cdp_msg.reply_tx.send(json);
            }
            return true;
        }
        return true;
    }

    let Some(session_id) = &req.session_id else {
        return false; // browser-level command: handled locally
    };
    let Some(page_id) = ctx.sessions.get(session_id) else {
        return false; // no session route at all: local dispatch surfaces the error
    };
    // The implicit browser target is a pseudo-page every connection shares;
    // its session must stay local (Target.* / Browser.* on it never resolve a
    // real Page, and it is not a registry target).
    if page_id == "browser" {
        return false;
    }
    if ctx.get_page(page_id).is_some() {
        return false; // owned here: normal path
    }
    // Remote target. If it left the registry (closed / owner disconnected),
    // answer with a protocol error instead of forwarding to a dead owner.
    if !ctx.registry.all().iter().any(|t| t.target_id == *page_id) {
        let resp = crate::types::CdpResponse::error(
            req.id,
            -32000,
            "Target not found".to_string(),
            req.session_id.clone(),
        );
        if let Ok(json) = serde_json::to_string(&resp) {
            let _ = cdp_msg.reply_tx.send(json);
        }
        return true;
    }
    let Some(owner_tx) = remote_owners
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(page_id)
        .cloned()
    else {
        // Owner not registered yet; local dispatch surfaces "No page for
        // session", which the client can retry.
        return false;
    };
    let forwarded = ServerMessage::RemoteExec {
        text: cdp_msg.text.clone(),
        page_id: page_id.clone(),
        reply_tx: cdp_msg.reply_tx.clone(),
    };
    if owner_tx.send(forwarded).is_err() {
        // Owner connection is gone but its page lingered in the map; fail the
        // command rather than letting the client hang on a dead route.
        let resp = crate::types::CdpResponse::error(
            req.id,
            -32000,
            "Target not found".to_string(),
            req.session_id.clone(),
        );
        if let Ok(json) = serde_json::to_string(&resp) {
            let _ = cdp_msg.reply_tx.send(json);
        }
    }
    true
}

/// Execute a forwarded remote command on behalf of another connection. The
/// request is rewritten to this connection's own session for the page (find
/// or mint one), executed through the normal pipeline (navigation uses the
/// spawn-and-defer path, everything else plain dispatch), and every outgoing
/// message is rewritten back to the caller's session id so the client can
/// correlate the reply with its session. The page's V8 isolate is never
/// shared: all execution happens on this (the owning) connection's thread.
async fn handle_remote_exec(
    ctx: &mut CdpContext,
    text: &str,
    page_id: &str,
    caller_reply_tx: &mpsc::UnboundedSender<String>,
    rx: &mut mpsc::UnboundedReceiver<ServerMessage>,
    intercept_rx: &mut Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>>,
    intercepted_paused: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    >,
    deferred: &mut std::collections::VecDeque<ServerMessage>,
) {
    let Ok(mut req) = serde_json::from_str::<CdpRequest>(text) else {
        return;
    };
    let caller_session = req.session_id.clone();
    // This connection's own session for the page. Pages created here (init or
    // createTarget) already carry a managed `{page_id}-session`; mint one for
    // anything else rather than leaving the caller's session unresolved.
    let owner_session = ctx
        .sessions
        .iter()
        .find(|(_, pid)| pid.as_str() == page_id)
        .map(|(sid, _)| sid.clone())
        .unwrap_or_else(|| {
            let sid = ctx.next_target_session(page_id);
            ctx.sessions.insert(sid.clone(), page_id.to_string());
            sid
        });
    req.session_id = Some(owner_session.clone());
    let rewritten = serde_json::to_string(&req).unwrap_or_else(|_| text.to_string());

    // Stream every reply through a rewrite channel: this command's response
    // and events carry THIS connection's session id, but they belong to the
    // caller's session.
    let (wrap_tx, mut wrap_rx) = mpsc::unbounded_channel::<String>();
    let caller_tx = caller_reply_tx.clone();
    let owner_sid = owner_session.clone();
    let caller_sid = caller_session.clone();
    tokio::task::spawn_local(async move {
        while let Some(msg) = wrap_rx.recv().await {
            if let Some(out) = rewrite_session_id(&msg, &owner_sid, caller_sid.as_deref()) {
                let _ = caller_tx.send(out);
            }
        }
    });

    let is_navigation = is_navigate_method(&rewritten);
    if is_navigation {
        process_with_interception(
            &rewritten, ctx, &wrap_tx, rx, intercept_rx, intercepted_paused, deferred, true,
        )
        .await;
    } else {
        let fetch_was_resolved = rewritten.contains("Fetch.")
            && handle_fetch_resolution(&rewritten, ctx, &wrap_tx, intercepted_paused);
        if !fetch_was_resolved {
            process_cdp_message(&rewritten, ctx, &wrap_tx).await;
        }
    }
}

/// Rewrite the `sessionId` field of an outgoing CDP message from the owner's
/// session id to the caller's (or strip it when the caller has none).
/// Messages that do not carry the owner's session id (browser-level events
/// without a session) pass through untouched.
fn rewrite_session_id(msg: &str, from: &str, to: Option<&str>) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(msg).ok()?;
    let obj = value.as_object_mut()?;
    if obj.get("sessionId").and_then(|v| v.as_str()) != Some(from) {
        return Some(msg.to_string());
    }
    match to {
        Some(caller) => {
            obj.insert("sessionId".into(), json!(caller));
        }
        None => {
            obj.remove("sessionId");
        }
    }
    serde_json::to_string(&value).ok()
}

/// Keep the process-wide owner map in sync with this connection's pages:
/// register every page this connection currently owns and drop stale entries
/// for pages it used to own (closed or dropped). Called after every message.
fn sync_remote_ownership(
    ctx: &CdpContext,
    remote_owners: &RemoteOwners,
    my_msg_tx: &mpsc::UnboundedSender<ServerMessage>,
) {
    let mut map = remote_owners.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|id, tx| {
        if ctx.pages.iter().any(|p| &p.id == id) {
            true
        } else {
            !tx.same_channel(my_msg_tx)
        }
    });
    for page in &ctx.pages {
        map.insert(page.id.clone(), my_msg_tx.clone());
    }
}

fn emit_intercepted_request(
    intercepted: obscura_js::ops::InterceptedRequest,
    frame_id: &str,
    session_id: Option<String>,
    reply_tx: &mpsc::UnboundedSender<String>,
    intercepted_paused: &mut HashMap<
        String,
        tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>,
    >,
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
    intercepted_paused.insert(intercepted.request_id, intercepted.resolver);
}

async fn pump_live_page_event_loop(ctx: &mut CdpContext) -> Result<bool, String> {
    let Some(page) = ctx.pages.iter_mut().find(|page| page.has_js()) else {
        return Ok(true);
    };
    page.run_autonomous_event_loop_turn().await
}

fn sync_live_page_network_events(ctx: &mut CdpContext) {
    let page_route = ctx.pages.iter().find(|page| page.has_js()).and_then(|page| {
        ctx.sessions
            .iter()
            .find(|(_, page_id)| *page_id == &page.id)
            .map(|(session_id, _)| {
                (
                    Some(session_id.clone()),
                    page.id.clone(),
                    page.frame_id.clone(),
                    page.url_string(),
                )
            })
    });
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
    let page = ctx.pages.iter().find(|page| page.has_js())?;
    let session_id = ctx
        .sessions
        .iter()
        .find(|(_, page_id)| *page_id == &page.id)
        .map(|(session_id, _)| session_id.clone())?;
    let (url, method, body) = page.take_pending_navigation()?;
    Some((session_id, url, method, body))
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
    _ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
    intercepted_paused: &mut HashMap<String, tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>>,
) -> bool {
    if let Ok(req) = serde_json::from_str::<CdpRequest>(text) {
        let method = req.method.as_str();
        let request_id = req.params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
        tracing::info!("INTERCEPTION resolution: {} for {}, paused_count={}", method, request_id, intercepted_paused.len());

        if let Some(resolver) = intercepted_paused.remove(request_id) {
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
            let _ = resolver.send(resolution);
            let resp = crate::types::CdpResponse::success(req.id, json!({}), req.session_id);
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = reply_tx.send(json);
            }
            return true;
        }
    }
    false
}

async fn process_with_interception(
    text: &str,
    ctx: &mut CdpContext,
    reply_tx: &mpsc::UnboundedSender<String>,
    rx: &mut mpsc::UnboundedReceiver<ServerMessage>,
    intercept_rx: &mut Option<mpsc::UnboundedReceiver<obscura_js::ops::InterceptedRequest>>,
    intercepted_paused: &mut HashMap<String, tokio::sync::oneshot::Sender<obscura_js::ops::InterceptResolution>>,
    deferred: &mut std::collections::VecDeque<ServerMessage>,
    send_command_response: bool,
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
    let wait_until = crate::domains::page::parse_wait_until(&req.params);
    let nav_method = req.params.get("__method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
    let nav_body = req.params.get("__body").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let preload_scripts: Vec<String> = ctx.preload_scripts.iter().map(|(_, s)| s.clone()).collect();

    if let Some(tx) = &ctx.intercept_tx {
        page.set_intercept_tx(tx.clone());
    }

    let session_for_events = req.session_id.clone();
    let frame_id = page.frame_id.clone();
    let loader_id = format!("loader-{}", uuid::Uuid::new_v4());

    let (nav_done_tx, mut nav_done_rx) = mpsc::channel::<(obscura_browser::Page, Result<(), String>)>(1);
    let url_owned = url.to_string();
    let nav_v8_lock = ctx.v8_lock.clone();

    tokio::task::spawn_local(async move {
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
        let result = if nav_method == "POST" && !nav_body.is_empty() {
            page.navigate_with_wait_post(&url_owned, wait_until, &nav_method, &nav_body).await
        } else {
            page.navigate_with_wait(&url_owned, wait_until).await
        }
        .map_err(|e| e.to_string());
        drop(_v8_guard);
        let _ = nav_done_tx.send((page, result)).await;
    });

    let navigate_result: Result<(), String>;
    let page_back: Option<obscura_browser::Page>;

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
            Some((returned_page, result)) = nav_done_rx.recv() => {
                page_back = Some(returned_page);
                navigate_result = result;
                break;
            }
            Some(intercepted) = async {
                if let Some(ref mut irx) = intercept_rx {
                    irx.recv().await
                } else {
                    std::future::pending().await
                }
            }, if has_irx => {
                emit_intercepted_request(
                    intercepted,
                    &frame_id,
                    session_for_events.clone(),
                    reply_tx,
                    intercepted_paused,
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            }
            Some(msg) = rx.recv() => {
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
                        if msg.text.contains("Fetch.continueRequest")
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
                            if deferred.len() >= MAX_DEFERRED_MESSAGES {
                                tracing::warn!("INTERCEPTION: deferred queue full ({}), returning error to client", MAX_DEFERRED_MESSAGES);
                                if let Ok(req) = serde_json::from_str::<CdpRequest>(&msg.text) {
                                    let resp = crate::types::CdpResponse::error(
                                        req.id,
                                        -32000,
                                        "Server busy: navigation in progress, try again later".to_string(),
                                        req.session_id,
                                    );
                                    if let Ok(json) = serde_json::to_string(&resp) {
                                        let _ = msg.reply_tx.send(json);
                                    }
                                }
                            } else {
                                tracing::info!("INTERCEPTION: deferring CDP message until nav completes");
                                deferred.push_back(ServerMessage::Cdp(msg));
                            }
                        }
                    }
                    ServerMessage::RemoteExec {
                        text,
                        page_id,
                        reply_tx,
                    } => {
                        // Same V8 hazard as a session command: executing a
                        // forwarded remote command would dispatch (and possibly
                        // enter an isolate) while this nav task has one
                        // entered. Defer it to the outer processor queue like
                        // any other Cdp message.
                        if deferred.len() >= MAX_DEFERRED_MESSAGES {
                            tracing::warn!(
                                "INTERCEPTION: deferred queue full ({}), returning error to client",
                                MAX_DEFERRED_MESSAGES
                            );
                            if let Ok(req) = serde_json::from_str::<CdpRequest>(&text) {
                                let resp = crate::types::CdpResponse::error(
                                    req.id,
                                    -32000,
                                    "Server busy: navigation in progress, try again later"
                                        .to_string(),
                                    req.session_id,
                                );
                                if let Ok(json) = serde_json::to_string(&resp) {
                                    let _ = reply_tx.send(json);
                                }
                            }
                        } else {
                            tracing::info!(
                                "INTERCEPTION: deferring remote exec until nav completes"
                            );
                            deferred.push_back(ServerMessage::RemoteExec {
                                text,
                                page_id,
                                reply_tx,
                            });
                        }
                    }
                    ServerMessage::ConnectionClosed => {
                        // No V8 involved: this only flips the processor's
                        // `ws_closed` flag. It must not be lost (the orphaned
                        // processor would then never exit once its pages are
                        // closed), so defer it unconditionally — unlike Cdp /
                        // RemoteExec it carries no payload that could overflow
                        // the cap.
                        deferred.push_back(ServerMessage::ConnectionClosed);
                    }
                }
            }
        }
    }

    // Deferred messages are handled by the outer `cdp_processor` loop
    // (it drains `deferred` before pulling the next message off `rx`).

    let mut page = page_back.expect("navigation task should return the page");

    // Fold in network events for script-initiated requests (fetch/XHR/dynamic
    // resource) so they emit as Network.requestWillBeSent / responseReceived
    // alongside the static navigation subresources (#406).
    page.sync_js_network_events();
    let network_events: Vec<_> = page.network_events.drain(..).collect();
    let page_url = page.url_string();
    let page_id_for_events = page.id.clone();
    let reached_network_idle = page.lifecycle.is_network_idle();

    ctx.pages.push(page);
    // Navigation changed the page's url/title: refresh its global target
    // entry so every connection's getTargets and /json/list see the new
    // document, not the stale about:blank snapshot.
    ctx.sync_registry();

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
        wait_until,
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
    //
    // Target.setDiscoverTargets is the exception: Chromium acknowledges it
    // first and only then floods Target.targetCreated for every existing
    // target. Puppeteer's ChromeTargetManager snapshots the targets known at
    // the moment the response resolves (#storeExistingTargetsForInit) and
    // waits for each of them to attach before the connect promise settles;
    // if the flood arrives first, every discovered target lands in that set
    // and (with pages excluded from the auto-attach filter) the client waits
    // forever for attachedToTarget events that never come. Match Chromium's
    // ordering so connect completes and the flood is processed afterwards.
    let response_before_events = req.method == "Target.setDiscoverTargets";
    if response_before_events {
        if let Ok(json) = serde_json::to_string(&response) {
            let _ = reply_tx.send(json);
        }
        for event in ctx.pending_events.drain(..) {
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = reply_tx.send(json);
            }
        }
    } else {
        for event in ctx.pending_events.drain(..) {
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = reply_tx.send(json);
            }
        }
        if let Ok(json) = serde_json::to_string(&response) {
            let _ = reply_tx.send(json);
        }
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

    // Only methods with NO side effects may live here: a fast-path reply
    // bypasses the domain handlers, so any command that emits events or
    // mutates state must dispatch normally instead (Target.setAutoAttach
    // used to be short-circuited here, which silently dropped its
    // attachedToTarget flood).
    let result = match req.method.as_str() {
        "Network.enable" | "Network.setCacheDisabled" | "Network.setRequestInterception" |
        "Page.enable" | "Page.setLifecycleEventsEnabled" | "Page.setInterceptFileChooserDialog" |
        "Runtime.runIfWaitingForDebugger" | "Runtime.discardConsoleEntries" |
        "Performance.enable" | "Log.enable" | "Security.enable" |
        "Emulation.setTouchEmulationEnabled" |
        "CSS.enable" | "Accessibility.enable" | "ServiceWorker.enable" |
        "Inspector.enable" | "Debugger.enable" | "Profiler.enable" |
        "HeapProfiler.enable" | "Overlay.enable" | "Storage.enable" => {
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
    let ws_stream = tokio_tungstenite::accept_async_with_config(stream, Some(cfg)).await?;
    info!("WebSocket connected");
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<String>();

    let _ = msg_tx.send(ServerMessage::NewConnection {
        reply_tx: reply_tx.clone(),
    });
    if let Some(init_msg) = reply_rx.recv().await {
        tracing::debug!("Connection init: {}", &init_msg[..init_msg.len().min(100)]);
    }

    let send_task = tokio::task::spawn_local(async move {
        while let Some(msg) = reply_rx.recv().await {
            if msg.contains("\"__init\"") {
                continue;
            }
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(msg) = ws_receiver.next().await {
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
                    break;
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

    send_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        handle_fetch_resolution, is_navigate_method, merge_cookie_delta, parse_cdp_headers,
    };
    #[cfg(feature = "render")]
    use super::{pump_and_forward_screencast_frames, pump_live_page_event_loop};
    use obscura_net::{CookieInfo, CookieJar};
    use serde_json::json;
    use std::collections::HashMap;

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

    #[tokio::test(flavor = "current_thread")]
    async fn page_runtime_advances_while_cdp_client_is_silent() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
                let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
                let default_context = crate::dispatch::CdpContext::new().default_context.clone();
                let (my_msg_tx, _my_msg_rx) = tokio::sync::mpsc::unbounded_channel();
                let processor = tokio::task::spawn_local(super::cdp_processor(
                    server_rx,
                    default_context,
                    shutdown,
                    crate::registry::TargetRegistry::default(),
                    my_msg_tx,
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
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

                drop(server_tx);
                tokio::time::timeout(std::time::Duration::from_secs(2), processor)
                    .await
                    .expect("processor shutdown timeout")
                    .expect("processor task");
            })
            .await;
    }

    // Issue #543: a browser-level client that connects and immediately calls
    // Target.getTargets must see the existing page targets. A fresh connection
    // used to start with an empty pages registry (pages only appeared after a
    // session-scoped event like Target.createTarget), so getTargets returned
    // [] and puppeteer/playwright-style clients broke before driving any page.
    #[tokio::test(flavor = "current_thread")]
    async fn fresh_connection_sees_page_targets_in_get_targets() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
                let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
                let default_context = crate::dispatch::CdpContext::new().default_context.clone();
                let (my_msg_tx, _my_msg_rx) = tokio::sync::mpsc::unbounded_channel();
                let processor = tokio::task::spawn_local(super::cdp_processor(
                    server_rx,
                    default_context,
                    shutdown,
                    crate::registry::TargetRegistry::default(),
                    my_msg_tx,
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
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
                send(json!({"id": 1, "method": "Target.getTargets", "params": {}}));

                loop {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("getTargets response timeout")
                        .expect("getTargets response channel"),
                    )
                    .unwrap();
                    if value["id"] == 1 {
                        let targets = value["result"]["targetInfos"]
                            .as_array()
                            .expect("targetInfos must be an array");
                        assert!(
                            !targets.is_empty(),
                            "getTargets must list the connection's page target"
                        );
                        assert_eq!(targets[0]["type"], "page");
                        break;
                    }
                }

                drop(server_tx);
                tokio::time::timeout(std::time::Duration::from_secs(2), processor)
                    .await
                    .expect("processor shutdown timeout")
                    .expect("processor task");
            })
            .await;
    }

    // Chromium answers Target.setDiscoverTargets before flooding
    // Target.targetCreated for existing targets. Puppeteer's
    // ChromeTargetManager snapshots the targets known at the moment the
    // response resolves and waits for each to attach; if the flood arrived
    // first, every discovered target would land in that set and the connect
    // promise would hang forever (no attachedToTarget ever follows for pages
    // excluded from the auto-attach filter).
    #[tokio::test(flavor = "current_thread")]
    async fn set_discover_targets_response_precedes_target_created_flood() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
                let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
                let default_context = crate::dispatch::CdpContext::new().default_context.clone();
                let (my_msg_tx, _my_msg_rx) = tokio::sync::mpsc::unbounded_channel();
                let processor = tokio::task::spawn_local(super::cdp_processor(
                    server_rx,
                    default_context,
                    shutdown,
                    crate::registry::TargetRegistry::default(),
                    my_msg_tx,
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
                ));

                server_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: reply_tx.clone(),
                    })
                    .unwrap();
                let _init = reply_rx.recv().await.expect("processor init");
                server_tx
                    .send(super::ServerMessage::Cdp(super::CdpMessage {
                        text: json!({
                            "id": 1,
                            "method": "Target.setDiscoverTargets",
                            "params": {},
                        })
                        .to_string(),
                        reply_tx: reply_tx.clone(),
                    }))
                    .unwrap();

                let mut saw_response = false;
                let mut saw_flood = false;
                for _ in 0..8 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("reply timeout")
                        .expect("reply channel"),
                    )
                    .unwrap();
                    if value["id"] == 1 {
                        assert!(!saw_response, "duplicate setDiscoverTargets response");
                        saw_response = true;
                    } else if value.get("method").and_then(|m| m.as_str())
                        == Some("Target.targetCreated")
                    {
                        assert!(
                            saw_response,
                            "targetCreated flood must arrive AFTER the setDiscoverTargets response (got {value})"
                        );
                        saw_flood = true;
                        break;
                    }
                }
                assert!(saw_response, "must receive the setDiscoverTargets response");
                assert!(saw_flood, "must receive the targetCreated flood");

                drop(server_tx);
                tokio::time::timeout(std::time::Duration::from_secs(2), processor)
                    .await
                    .expect("processor shutdown timeout")
                    .expect("processor task");
            })
            .await;
    }

    // Regression: Target.setAutoAttach must NOT be served by the reply fast
    // path (which would ack without emitting anything). It dispatches through
    // the domain handler, so the client receives Target.attachedToTarget for
    // its page with a usable session (puppeteer's browser.pages() depends on
    // that event).
    #[tokio::test(flavor = "current_thread")]
    async fn set_auto_attach_emits_attached_to_target_through_server_path() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
                let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
                let default_context = crate::dispatch::CdpContext::new().default_context.clone();
                let (my_msg_tx, _my_msg_rx) = tokio::sync::mpsc::unbounded_channel();
                let processor = tokio::task::spawn_local(super::cdp_processor(
                    server_rx,
                    default_context,
                    shutdown,
                    crate::registry::TargetRegistry::default(),
                    my_msg_tx,
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
                ));

                server_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: reply_tx.clone(),
                    })
                    .unwrap();
                let _init = reply_rx.recv().await.expect("processor init");
                server_tx
                    .send(super::ServerMessage::Cdp(super::CdpMessage {
                        text: json!({
                            "id": 1,
                            "method": "Target.setAutoAttach",
                            "params": {"autoAttach": true, "flatten": true},
                        })
                        .to_string(),
                        reply_tx: reply_tx.clone(),
                    }))
                    .unwrap();

                let mut saw_attached = false;
                let mut attached_session = None;
                for _ in 0..6 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("reply timeout")
                        .expect("reply channel"),
                    )
                    .unwrap();
                    if value.get("method").and_then(|m| m.as_str())
                        == Some("Target.attachedToTarget")
                    {
                        assert_eq!(value["params"]["targetInfo"]["type"], "page");
                        attached_session = value["params"]["sessionId"].as_str().map(str::to_owned);
                        saw_attached = true;
                        break;
                    }
                }
                assert!(
                    saw_attached,
                    "setAutoAttach must emit attachedToTarget via the server path (fast-path regression)"
                );
                assert!(
                    attached_session.is_some() && !attached_session.as_deref().unwrap().is_empty(),
                    "attachedToTarget must carry a session id"
                );

                drop(server_tx);
                tokio::time::timeout(std::time::Duration::from_secs(2), processor)
                    .await
                    .expect("processor shutdown timeout")
                    .expect("processor task");
            })
            .await;
    }

    // Cross-connection session routing (#430 follow-up): a command sent on a
    // session that attaches to a page owned by ANOTHER connection must be
    // forwarded to that connection's processor and executed there (the page's
    // V8 isolate is thread-confined), with the response coming back on the
    // caller's channel carrying the caller's session id.
    #[tokio::test(flavor = "current_thread")]
    async fn remote_session_commands_route_to_the_owning_connection() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let registry = crate::registry::TargetRegistry::default();
                let remote_owners: super::RemoteOwners =
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());

                // Owner connection: its NewConnection init creates a page the
                // caller will drive remotely. The processor's `my_msg_tx` is
                // the sender of its OWN rx channel (that is what gets
                // registered in the owner map so forwarded RemoteExec messages
                // reach this processor). Teardown uses the shutdown Notify
                // because the processor holds a sender clone of its own rx.
                let (owner_tx, owner_rx) = tokio::sync::mpsc::unbounded_channel();
                let (owner_reply_tx, mut owner_reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let owner_default = crate::dispatch::CdpContext::new().default_context.clone();
                let owner_processor = tokio::task::spawn_local(super::cdp_processor(
                    owner_rx,
                    owner_default,
                    shutdown.clone(),
                    registry.clone(),
                    owner_tx.clone(),
                    remote_owners.clone(),
                ));
                owner_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: owner_reply_tx.clone(),
                    })
                    .unwrap();
                let owner_init: serde_json::Value = serde_json::from_str(
                    &tokio::time::timeout(std::time::Duration::from_secs(2), owner_reply_rx.recv())
                        .await
                        .expect("owner init timeout")
                        .expect("owner init channel"),
                )
                .unwrap();
                let owner_page = owner_init["pageId"].as_str().unwrap().to_string();

                // Caller connection.
                let (caller_tx, caller_rx) = tokio::sync::mpsc::unbounded_channel();
                let (caller_reply_tx, mut caller_reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let caller_default = crate::dispatch::CdpContext::new().default_context.clone();
                let caller_processor = tokio::task::spawn_local(super::cdp_processor(
                    caller_rx,
                    caller_default,
                    shutdown.clone(),
                    registry,
                    caller_tx.clone(),
                    remote_owners,
                ));
                caller_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: caller_reply_tx.clone(),
                    })
                    .unwrap();
                let _caller_init = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    caller_reply_rx.recv(),
                )
                .await
                .expect("caller init timeout")
                .expect("caller init channel");

                let send = |value: serde_json::Value,
                            tx: &tokio::sync::mpsc::UnboundedSender<super::ServerMessage>,
                            reply: &tokio::sync::mpsc::UnboundedSender<String>| {
                    tx.send(super::ServerMessage::Cdp(super::CdpMessage {
                        text: value.to_string(),
                        reply_tx: reply.clone(),
                    }))
                    .unwrap();
                };

                // The caller attaches to the owner's page (globally visible
                // via the shared registry) and gets its own session for it.
                send(
                    json!({
                        "id": 1,
                        "method": "Target.attachToTarget",
                        "params": {"targetId": owner_page, "flatten": true},
                    }),
                    &caller_tx,
                    &caller_reply_tx,
                );
                let mut session_id = None;
                for _ in 0..6 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            caller_reply_rx.recv(),
                        )
                        .await
                        .expect("attach response timeout")
                        .expect("attach response channel"),
                    )
                    .unwrap();
                    if value["id"] == 1 {
                        session_id = value["result"]["sessionId"].as_str().map(str::to_string);
                        break;
                    }
                }
                let session_id = session_id.expect("attachToTarget must return a session");

                // Drive the REMOTE session. Page.getFrameTree used to fail
                // with "No page for session" because the caller does not own
                // the page; it must now be forwarded to the owner, executed
                // there, and answered on the caller's channel.
                send(
                    json!({
                        "id": 2,
                        "method": "Page.getFrameTree",
                        "sessionId": session_id,
                        "params": {},
                    }),
                    &caller_tx,
                    &caller_reply_tx,
                );
                let mut saw_response = false;
                for _ in 0..8 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            caller_reply_rx.recv(),
                        )
                        .await
                        .expect("getFrameTree response timeout")
                        .expect("getFrameTree response channel"),
                    )
                    .unwrap();
                    if value["id"] == 2 {
                        assert!(
                            value.get("error").is_none(),
                            "remote getFrameTree must not error, got: {value}"
                        );
                        assert_eq!(
                            value["sessionId"], session_id,
                            "response must carry the caller's session id"
                        );
                        assert_eq!(
                            value["result"]["frameTree"]["frame"]["url"],
                            "about:blank"
                        );
                        saw_response = true;
                        break;
                    }
                }
                assert!(saw_response, "remote getFrameTree response must arrive");

                // Each processor holds a sender clone of its own rx channel,
                // so dropping the test's senders alone would never close the
                // channels. Wake the shared shutdown Notify instead.
                shutdown.notify_waiters();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), owner_processor)
                    .await
                    .expect("owner processor shutdown timeout")
                    .expect("owner processor task");
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), caller_processor)
                    .await
                    .expect("caller processor shutdown timeout")
                    .expect("caller processor task");
            })
            .await;
    }

    // The implicit browser target's session must never be treated as a remote
    // page: sessions["browser-session"] maps to the pseudo-target "browser",
    // which is not a registry page, so commands on it (Target.getTargets etc.)
    // must be served by the attaching connection itself.
    #[tokio::test(flavor = "current_thread")]
    async fn browser_session_commands_stay_local() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
                let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
                let (my_msg_tx, _my_msg_rx) = tokio::sync::mpsc::unbounded_channel();
                let default_context = crate::dispatch::CdpContext::new().default_context.clone();
                let processor = tokio::task::spawn_local(super::cdp_processor(
                    server_rx,
                    default_context,
                    shutdown,
                    crate::registry::TargetRegistry::default(),
                    my_msg_tx,
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
                ));

                server_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: reply_tx.clone(),
                    })
                    .unwrap();
                let _init = reply_rx.recv().await.expect("processor init");

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
                    "method": "Target.attachToBrowserTarget",
                    "params": {},
                }));
                for _ in 0..4 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(std::time::Duration::from_secs(2), reply_rx.recv())
                            .await
                            .expect("attach response timeout")
                            .expect("attach response channel"),
                    )
                    .unwrap();
                    if value["id"] == 1 {
                        break;
                    }
                }
                send(json!({
                    "id": 2,
                    "method": "Target.getTargets",
                    "sessionId": "browser-session",
                    "params": {},
                }));
                let mut saw_response = false;
                for _ in 0..6 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(std::time::Duration::from_secs(2), reply_rx.recv())
                            .await
                            .expect("getTargets response timeout")
                            .expect("getTargets response channel"),
                    )
                    .unwrap();
                    if value["id"] == 2 {
                        assert!(
                            value.get("result").and_then(|r| r.get("targetInfos")).is_some(),
                            "browser-session Target.getTargets must be served locally, got: {value}"
                        );
                        saw_response = true;
                        break;
                    }
                }
                assert!(saw_response, "browser-session response must arrive");

                drop(server_tx);
                tokio::time::timeout(std::time::Duration::from_secs(2), processor)
                    .await
                    .expect("processor shutdown timeout")
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
        let mut paused = HashMap::from([("request-1".to_string(), resolution_tx)]);
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
        pump_live_page_event_loop(&mut ctx).await.unwrap();
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
        pump_live_page_event_loop(&mut ctx).await.unwrap();
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
        pump_live_page_event_loop(&mut ctx).await.unwrap();
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

    // Pages survive the disconnect of the connection that created them: after
    // `ConnectionClosed` the page stays in the global registry, is still
    // drivable, and the processor exits only once the page is actually closed.
    #[tokio::test(flavor = "current_thread")]
    async fn orphaned_connection_keeps_pages_until_closed() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (server_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
                let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
                let default_context = crate::dispatch::CdpContext::new().default_context.clone();
                let registry = crate::registry::TargetRegistry::default();
                let remote_owners: super::RemoteOwners =
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
                let processor = tokio::task::spawn_local(super::cdp_processor(
                    server_rx,
                    default_context,
                    shutdown.clone(),
                    registry.clone(),
                    server_tx.clone(),
                    remote_owners.clone(),
                ));

                // The connection opens and its init page is registered.
                server_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: reply_tx.clone(),
                    })
                    .unwrap();
                let init: serde_json::Value = serde_json::from_str(
                    &tokio::time::timeout(std::time::Duration::from_secs(2), reply_rx.recv())
                        .await
                        .expect("init timeout")
                        .expect("init channel"),
                )
                .unwrap();
                let page_id = init["pageId"].as_str().unwrap().to_string();
                assert_eq!(registry.all().len(), 1, "page must be registered");

                // Client disconnects. The page must survive in the registry.
                server_tx.send(super::ServerMessage::ConnectionClosed).unwrap();
                tokio::task::yield_now().await;
                assert_eq!(
                    registry.all().len(),
                    1,
                    "page must persist after the creating connection disconnects"
                );
                assert_eq!(registry.all()[0].target_id, page_id);

                // A later connection can still drive the orphaned page.
                server_tx
                    .send(super::ServerMessage::Cdp(super::CdpMessage {
                        text: json!({
                            "id": 10,
                            "method": "Page.getFrameTree",
                            "sessionId": format!("{page_id}-session"),
                            "params": {},
                        })
                        .to_string(),
                        reply_tx: reply_tx.clone(),
                    }))
                    .unwrap();
                let mut saw_drive = false;
                for _ in 0..8 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("drive response timeout")
                        .expect("drive response channel"),
                    )
                    .unwrap();
                    if value["id"] == 10 {
                        assert!(
                            value.get("error").is_none(),
                            "orphaned page must answer commands, got: {value}"
                        );
                        saw_drive = true;
                        break;
                    }
                }
                assert!(saw_drive, "orphaned page must still answer commands");

                // Target.closeTarget tears the orphaned page down for real and
                // the owner processor exits once it owns no pages.
                server_tx
                    .send(super::ServerMessage::Cdp(super::CdpMessage {
                        text: json!({
                            "id": 11,
                            "method": "Target.closeTarget",
                            "params": {"targetId": page_id},
                        })
                        .to_string(),
                        reply_tx: reply_tx.clone(),
                    }))
                    .unwrap();
                let mut saw_close = false;
                for _ in 0..8 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            reply_rx.recv(),
                        )
                        .await
                        .expect("close response timeout")
                        .expect("close response channel"),
                    )
                    .unwrap();
                    if value["id"] == 11 {
                        saw_close = true;
                        break;
                    }
                }
                assert!(saw_close, "closeTarget must be answered");
                assert!(
                    registry.all().is_empty(),
                    "closed target must leave the registry"
                );
                let joined = tokio::time::timeout(std::time::Duration::from_secs(5), processor)
                    .await
                    .expect("orphaned processor must exit after its last page closes")
                    .expect("processor task");
                let _ = joined;
                shutdown.notify_waiters();
            })
            .await;
    }

    // Browser-level Target.closeTarget (no sessionId) on a REMOTE page must be
    // forwarded to the owning connection so the live Page is torn down for
    // real, not just tombstoned (which would leak the isolate on an owner that
    // never syncs again).
    #[tokio::test(flavor = "current_thread")]
    async fn remote_close_target_routes_to_owning_connection() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let registry = crate::registry::TargetRegistry::default();
                let remote_owners: super::RemoteOwners =
                    std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
                let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());

                // Owner connection: its init page is the remote target the
                // caller will close.
                let (owner_tx, owner_rx) = tokio::sync::mpsc::unbounded_channel();
                let (owner_reply_tx, mut owner_reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let owner_default = crate::dispatch::CdpContext::new().default_context.clone();
                let owner_processor = tokio::task::spawn_local(super::cdp_processor(
                    owner_rx,
                    owner_default,
                    shutdown.clone(),
                    registry.clone(),
                    owner_tx.clone(),
                    remote_owners.clone(),
                ));
                owner_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: owner_reply_tx.clone(),
                    })
                    .unwrap();
                let owner_init: serde_json::Value = serde_json::from_str(
                    &tokio::time::timeout(std::time::Duration::from_secs(2), owner_reply_rx.recv())
                        .await
                        .expect("owner init timeout")
                        .expect("owner init channel"),
                )
                .unwrap();
                let owner_page = owner_init["pageId"].as_str().unwrap().to_string();

                // Caller connection.
                let (caller_tx, caller_rx) = tokio::sync::mpsc::unbounded_channel();
                let (caller_reply_tx, mut caller_reply_rx) = tokio::sync::mpsc::unbounded_channel();
                let caller_default = crate::dispatch::CdpContext::new().default_context.clone();
                let caller_processor = tokio::task::spawn_local(super::cdp_processor(
                    caller_rx,
                    caller_default,
                    shutdown.clone(),
                    registry.clone(),
                    caller_tx.clone(),
                    remote_owners.clone(),
                ));
                caller_tx
                    .send(super::ServerMessage::NewConnection {
                        reply_tx: caller_reply_tx.clone(),
                    })
                    .unwrap();
                let _caller_init = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    caller_reply_rx.recv(),
                )
                .await
                .expect("caller init timeout")
                .expect("caller init channel");

                // Browser-level closeTarget on the owner's page, no sessionId.
                caller_tx
                    .send(super::ServerMessage::Cdp(super::CdpMessage {
                        text: json!({
                            "id": 20,
                            "method": "Target.closeTarget",
                            "params": {"targetId": owner_page},
                        })
                        .to_string(),
                        reply_tx: caller_reply_tx.clone(),
                    }))
                    .unwrap();
                let mut saw_close = false;
                for _ in 0..8 {
                    let value: serde_json::Value = serde_json::from_str(
                        &tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            caller_reply_rx.recv(),
                        )
                        .await
                        .expect("close response timeout")
                        .expect("close response channel"),
                    )
                    .unwrap();
                    if value["id"] == 20 {
                        assert!(
                            value.get("error").is_none(),
                            "remote closeTarget must succeed, got: {value}"
                        );
                        saw_close = true;
                        break;
                    }
                }
                assert!(saw_close, "remote closeTarget must be answered");

                // The owner sends the response only after `remove_page` ran
                // (dispatch completes before the reply is queued), so the
                // registry is already updated once we see the response.
                assert!(
                    !registry.all().iter().any(|t| t.target_id == owner_page),
                    "remotely closed target must leave the registry"
                );

                shutdown.notify_waiters();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), owner_processor)
                    .await
                    .expect("owner processor shutdown timeout")
                    .expect("owner processor task");
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), caller_processor)
                    .await
                    .expect("caller processor shutdown timeout")
                    .expect("caller processor task");
            })
            .await;
    }
}
