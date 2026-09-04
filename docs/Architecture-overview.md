Obscura is a workspace of nine crates.

```
obscura-cli       CLI entry point. fetch, serve, scrape, mcp.
obscura-cdp       Chrome DevTools Protocol server. WebSocket, dispatch, domain handlers.
obscura-browser   Page type, navigation, lifecycle events.
obscura-js        V8 runtime via deno_core. bootstrap.js + Rust ops.
obscura-dom       DOM tree implementation.
obscura-net       HTTP client, stealth client, cookie jar, robots cache, tracker blocklist.
obscura-mcp       Model Context Protocol server.
obscura-render    CSS cascade, retained layout, text shaping, and CPU paint.
obscura           Embeddable Rust library API (Browser, Page, Element, CookieStore).
```

## Request flow

A `Page.navigate` from a CDP client:

```
CDP client (Puppeteer)
        │ WebSocket frame
        ▼
obscura-cdp/server.rs           accept, route by sessionId
        │
        ▼
obscura-cdp/dispatch.rs         method router, acquires v8_lock
        │
        ▼
obscura-cdp/domains/page.rs     Page.navigate handler
        │
        ▼
obscura-browser/page.rs         navigate_with_wait
        │
        ├──► obscura-net/client.rs        HTTP fetch
        │
        ├──► obscura-dom/tree.rs          parse HTML into the tree
        │
        └──► obscura-js/runtime.rs        run inline scripts
                  │
                  └──► bootstrap.js + ops.rs    DOM bindings
```

The dispatcher emits CDP events (`Network.requestWillBeSent`, `Page.frameNavigated`, `Page.lifecycleEvent`) back to the client through the same WebSocket.

## Rendering flow

`obscura-render` consumes the shared DOM and computed style state. Taffy
provides the flex/grid foundation; Obscura adds browser formatting behavior,
text shaping, intrinsic replaced-element sizing, retained geometry, scrolling,
and CPU-backed paint. `obscura-js` exposes renderer-owned geometry to DOM APIs,
`obscura-browser` prepares resources and owns capture, and `obscura-cdp` maps
screenshots, screencast frames, and raster PDF output onto CDP.

Layout is retained between captures and invalidated by relevant DOM, style,
viewport, scroll, animation, font, and resource changes. The same geometry
therefore drives browser APIs and paint instead of maintaining separate
measurement and screenshot models.

## V8 ownership

Each CDP connection owns a dedicated processor thread. Its page runtimes stay
on that thread and only one page runtime is active at a time. Different
connections therefore cannot enter each other's V8 isolates.

`obscura_js::v8_lock::global()` is a `tokio::sync::Mutex` that serializes V8 work. A handler that wants to run JS must acquire the lock first:

```rust
let _guard = obscura_js::v8_lock::global().lock().await;
page.evaluate(expr).await
```

The dispatcher routes navigation through `process_with_interception` in
`server.rs`, which keeps the task's join handle and continues receiving
WebSocket control messages without entering V8 concurrently. Commands that
would activate another page are deferred in a bounded queue.

An internal, feature-gated `NavigationControl` registers the current runtime's
thread-safe V8 isolate handle. Target close, context disposal, connection loss,
and shutdown set a sticky cancellation flag and terminate synchronous V8 work
when necessary. The owner thread then unwinds the navigation future normally,
clears V8 termination, awaits the task, and drops the page. A canceled page is
never reinserted.

## Robustness

One page cannot hang or crash the process. `obscura-js/runtime.rs` provides a V8 termination watchdog (`arm_watchdog`, `run_event_loop_bounded`) that terminates the isolate from a separate thread when synchronous work overruns a budget, because `tokio::time::timeout` cannot preempt synchronous V8. It bounds the post-load settle, the navigation event-loop pumps, and `--eval`. The complete script phase is bounded by `OBSCURA_SCRIPT_DEADLINE_MS`; enhancement modules have a shorter per-module graph-loading/evaluation budget controlled by `OBSCURA_MODULE_BUDGET_MS`, while modules mounting an empty SPA shell receive the full script deadline. `obscura-js/cdp_watchdog.rs` is a single shared watchdog the dispatcher arms around every CDP command, so a runaway page cannot hold the V8 lock and wedge other sessions (tunable via `OBSCURA_CDP_COMMAND_TIMEOUT_MS`). `op_dom` is wrapped in `catch_unwind` so a DOM-op panic degrades to a null result instead of aborting the process through V8's FFI frame, and `obscura-dom/tree.rs` rejects cyclic reparenting that would make tree walks loop forever. Scripted `fetch()`/XHR and module network requests are timeout-bounded (`OBSCURA_FETCH_TIMEOUT_MS`), and the one-shot `fetch` CLI has a process-level hard deadline as a final backstop.

## JS bridge

`obscura-js/js/bootstrap.js` provides the browser globals: `document`, `window`, `navigator`, `location`, observers, fetch, indexedDB, etc.

`obscura-js/src/ops.rs` registers Rust ops that the bootstrap calls into:

```js
Deno.core.ops.op_dom('insert_before', parentNid, refNid, newNid);
```

Adding a Web API usually means:

1. JS shim in `bootstrap.js` that exposes the API surface.
2. Rust op in `ops.rs` that performs the side effect (DOM mutation, fetch, crypto).
3. Register the op in `build_extension()`.

Worked example: [Adding a CDP method or Web API](Adding-a-CDP-method-or-Web-API.md).

## CDP session model

Each CDP client connection gets attached to one or more targets.
Session IDs are `"{targetId}-session"`. The dispatcher routes by `sessionId` in the incoming frame to the right `Page`.

Targets are created by `Target.createTarget`. Closing the WebSocket is
connection-local teardown: in-flight navigation is canceled, paused requests
are failed, and that connection's pages, sessions, loaders, frame state,
screencasts, streams, pending events, and deferred commands are dropped.
Other connections are unaffected.

## Lifecycle

Lifecycle events are emitted by `obscura-browser/lifecycle.rs` as the page transitions:

```
init → commit → domcontentloaded → load → networkidle2 → networkidle0
```

Puppeteer and Playwright do not send a server-side `waitUntil` with raw
`Page.navigate`. The WebSocket server therefore returns the navigation result
at DOMContentLoaded, keeps ownership of the live page runtime, and streams the
later load transition. Same-page commands may run during
that continuation. Commands which would activate another page are held in a
bounded per-connection queue until the active page reaches load or its bounded
load-script deadline ends, because suspending the single V8 isolate
earlier would otherwise discard timers, fetches, and dynamic-script jobs.
After load, a wake-driven drain may retain that ownership for at most one
second so work queued by a load handler can publish its network/runtime events
before another target replaces the active isolate.

Load is emitted only after load-delaying scripts and their Network/Runtime
events have been drained. Lifecycle events are emitted once per document and remain
routed to the session that initiated the navigation. In-process CDP and MCP do
not own an autonomous WebSocket pump, so they preserve their historical
fully-loaded return contract instead of exposing an unowned DCL continuation.
Puppeteer / Playwright `goto` resolves client-side when its requested lifecycle
event arrives.

## Storage

`--storage-dir` persists cookies (`cookies.json`) and localStorage (`localStorage/<origin>.json`). Reads on process start, writes on every navigation and on graceful shutdown.

## Stealth

`--stealth` swaps the default `reqwest` client for `obscura-net/wreq_client.rs`, which presents a real browser's TLS ClientHello, ALPN, and cipher order (a consistent Chrome fingerprint, not a randomized one) so the TLS layer matches the User-Agent and JS surfaces. It also applies the bundled tracker blocklist before any request leaves the process. Scripted `fetch()`/XHR go through the same stealth client, so subresource requests carry the same fingerprint as the navigation. `--stealth` is a global CLI flag that applies to `fetch`, `serve`, `scrape`, and `mcp`.

## Workspace conventions

- One crate per layer. Cross-crate calls go through the layer above, not sideways.
- All async is `tokio` with a `LocalSet` because V8 is `!Send`.
- All DOM ops go through `op_dom` to keep the JS/Rust boundary narrow.
