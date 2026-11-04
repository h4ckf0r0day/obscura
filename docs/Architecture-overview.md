Obscura is a workspace of ten crates.

```
obscura-cli       CLI entry point. fetch, serve, scrape, mcp.
obscura-cdp       Chrome DevTools Protocol server. WebSocket, dispatch, domain handlers.
obscura-browser   Page type, navigation, lifecycle events.
obscura-js        V8 runtime via deno_core. bootstrap.js + Rust ops.
obscura-dom       DOM tree implementation.
obscura-net       HTTP client, stealth client, cookie jar, robots cache, tracker blocklist.
obscura-ssrf      Shared URL and IP-address validation for request policies.
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

Page-owned layout and paint never fetch resources synchronously. Missing
resources load through the page transport, with URL blocking checked both
for navigation/screenshot warmup and for later renderer misses. The DOM-only
`Page::screenshot` fallback (for example after `Page::suspend_js`) also performs
no network requests: external images and fonts absent from its cache remain
unavailable. Resume the page and prepare its resources before capture when
those assets are required. Standalone `obscura-render` callers retain their
existing synchronous-loader behavior.

The relevant paths are `Page::screenshot_with_animation_sample` and
`Page::render_resource_candidates` in `crates/obscura-browser/src/page.rs`,
`ObscuraJsRuntime::take_render_resource_requests` in
`crates/obscura-js/src/runtime.rs`, and `RenderResourceCache` in
`crates/obscura-render/src/paint.rs`.

## CDP execution

Each CDP connection runs page work on its own OS thread and current-thread
Tokio runtime. Socket I/O uses a separate thread. Connections can execute in
parallel; `--max-connections` bounds live connections per worker process.

Each page runtime owns a V8 isolate. `CdpContext::v8_lock` serializes V8 work
within one connection so handlers do not interleave isolate entry. It is not a
process-wide lock. Long operations use `process_with_interception` in
`server.rs` so the connection can keep processing interception messages.

Targets and sessions belong to the connection that created them. A separate
browser WebSocket does not attach to another connection's pages. Within one
connection, additional flattened sessions can observe the same target.

## Child frames

`iframe srcdoc` uses a native child realm and document, not a copy in the
parent DOM. Its URL is `about:srcdoc`; relative URLs inherit the creator's
document base, and unsandboxed Web Storage uses the creator's origin.
`srcdoc` takes precedence over `src`, including an empty value. Parser input,
connected `innerHTML`, property/attribute changes and removal use the same
loading path. Replaced or detached documents are retired before stale loads
can run. The iframe's `load` event follows classic author scripts.
Navigation runs as a posted task, after microtasks. Parsing uses srcdoc
standards mode without requiring a doctype and honors script-disabled
`noscript` content.

Sandboxed `srcdoc` enforces opaque document access and script disabling,
including inherited restrictions on nested frames. This does not implement
every HTML sandbox policy. Frame module scripts remain unsupported.
Child-frame CDP evaluation routing and iframe content compositing remain
separate compatibility gaps; loading a document does not establish support
for Playwright frame locators or iframe screenshot fidelity.

## Robustness

`obscura-js/runtime.rs` provides a V8 termination watchdog (`arm_watchdog`, `run_event_loop_bounded`) that terminates the isolate from a separate thread when synchronous work overruns a budget, because `tokio::time::timeout` cannot preempt synchronous V8. It bounds the post-load settle, the navigation event-loop pumps, and `--eval`. The complete script phase is bounded by `OBSCURA_SCRIPT_DEADLINE_MS`; enhancement modules have a shorter per-module graph-loading/evaluation budget controlled by `OBSCURA_MODULE_BUDGET_MS`, while modules mounting an empty SPA shell receive the full script deadline. `obscura-js/cdp_watchdog.rs` is a watchdog the dispatcher arms around CDP commands, to release the connection's V8 lock after an execution timeout (tunable via `OBSCURA_CDP_COMMAND_TIMEOUT_MS`). `op_dom` is wrapped in `catch_unwind` so a DOM-op panic degrades to a null result instead of aborting the process through V8's FFI frame, and `obscura-dom/tree.rs` rejects cyclic reparenting that would make tree walks loop forever. Scripted `fetch()`/XHR and module network requests are timeout-bounded (`OBSCURA_FETCH_TIMEOUT_MS`), and the one-shot `fetch` CLI has a process-level hard deadline as a final backstop.

## JS bridge

`obscura-js/js/bootstrap.js` provides the browser globals: `document`, `window`, `navigator`, `location`, observers, fetch, indexedDB, etc.

`obscura-js/src/ops.rs` registers Rust ops that the bootstrap calls into:

```js
// bootstrap.js helper; the reference node determines the parent.
_dom('insert_before', newNid, refNid);
```

Adding a Web API usually means:

1. JS shim in `bootstrap.js` that exposes the API surface.
2. Rust op in `ops.rs` that performs the side effect (DOM mutation, fetch, crypto).
3. Register the op in `build_extension()`.

Worked example: [Adding a CDP method or Web API](Adding-a-CDP-method-or-Web-API.md).

## Classic Web Workers

The JavaScript shim executes each classic Worker source once and retains its
message handlers and lexical state. Bare `onmessage` assignments target the
worker scope, and messages posted before the source loads are queued until
initialization finishes. Terminating a worker discards pending messages.

Workers remain emulated within the page runtime, not separate V8 isolates or
OS threads. This is not a complete WorkerGlobalScope implementation.

## CDP session model

Each CDP client connection gets attached to one or more targets.
Managed sessions use `"{targetId}-session"`. Explicit `Target.attachToTarget`
calls return distinct session IDs. Treat those IDs as opaque and send the
returned value as `sessionId` on subsequent commands.

Targets are created by `Target.createTarget`. Closing the WebSocket ends that connection's sessions and page runtimes.
The server continues accepting other connections.

## Lifecycle

Lifecycle events are emitted by `obscura-browser/lifecycle.rs` as the page transitions:

```
init → commit → domcontentloaded → load → networkidle2 → networkidle0
```

`waitUntil` on `Page.navigate` blocks until the requested level is reached. The Puppeteer / Playwright `goto` resolves on the matching `Page.lifecycleEvent` client-side.

## Storage

`--storage-dir` persists cookies to `cookies.json` and `localStorage` to `localStorage/<origin>-<digest>.json`: read on process start, written after each navigation that changed something and on graceful shutdown. `sessionStorage` is origin-keyed in memory only, per tab.

## Stealth

`--stealth` swaps the default `reqwest` client for `obscura-net/wreq_client.rs`, which presents a real browser's TLS ClientHello, ALPN, and cipher order (a consistent Chrome fingerprint, not a randomized one) so the TLS layer matches the User-Agent and JS surfaces. It also applies the bundled tracker blocklist before any request leaves the process. Scripted `fetch()`/XHR go through the same stealth client, so subresource requests carry the same fingerprint as the navigation. `--stealth` is a global CLI flag that applies to `fetch`, `serve`, `scrape`, and `mcp`.

## Workspace conventions

- One crate per layer. Cross-crate calls go through the layer above, not sideways.
- All async is `tokio` with a `LocalSet` because V8 is `!Send`.
- All DOM ops go through `op_dom` to keep the JS/Rust boundary narrow.
