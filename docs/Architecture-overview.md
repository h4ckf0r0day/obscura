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

Grid and table track sizing stops redistribution when floating-point rounding
leaves both the track increases and remaining space unchanged. Reaching a
track limit still counts as progress even when subtracting its increase rounds
away, so valid redistribution continues without an arbitrary iteration cap.

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

Connection threads reserve the same 512 MiB native stack address range as the
CLI, with memory committed on demand. This protects recursive native rendering
from the small default thread stack. HTML parsing also flattens excessive
nesting like Chromium: 511 nested divs in a document and 512 in a fragment,
without discarding elements or text. This parser rule does not cap DOM mutations.
The last parser parent/child depths are cached to avoid walking every ancestor
on ordinary insertions; DOM mutations invalidate that cache. The
`render-repros/deep-parser-layout.html` fixture covers retained content and the
Chromium-compatible fragment depth.
The performance smoke gate includes balanced HTML trees at depths 4, 32, 128,
and 300 so the depth guard cannot hide a parser throughput regression.

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
CDP `DOM.describeNode` and `DOM.getOuterHTML` resolve remote node handles in
their native owning document, including handles obtained through the parent.
The document element carries that document's protocol frame ID, so Playwright
can discover an element handle's owner frame. Retired frame handles fail
instead of reading a same-numbered node in the main document.
Serialized iframe owner elements identify their live child frame in
`DOM.describeNode`, `DOM.getDocument`, and `DOM.setChildNodes`. Native ownership
records distinguish nested documents and omit retired or detached contexts,
allowing clients to resolve iframe elements to their child frames.
Native browsing-context records also back `parent`, `top`, indexed and named
cross-origin sibling access, and `frames.length`. They exist before a fetched
document arrives, so a pending frame never aliases an unrelated retired realm.
Navigation preserves the context's position and remote Window identity;
removal and reinsertion create a new context at the end of connection order.
Named cross-origin access requires both the owner's current `name` attribute
and the context's retained name to match. Missing or forbidden cross-origin
properties throw `SecurityError`. Queued messages retain their source context
even when its document is retired before delivery.
Live child frames advertise their default CDP execution contexts.
`Runtime.evaluate` and `Runtime.callFunctionOn`, including awaited calls,
enter the frame's actual V8 realm and use its document. Remote objects remain
in the page's central handle store, while result metadata is read in the
owning realm. Object handles and their property handles retain the realm
of the evaluation that produced them, not the object's creation realm.
Calls reject retired realms and unknown or released handles rather than
falling back to the main document.
Pending evaluations reject retired frame contexts instead of reading a
replacement document. Named utility contexts still alias their
frame's realm rather than providing separate JavaScript worlds.
Iframe content compositing remains a separate compatibility gap; evaluating
a child document does not establish iframe screenshot fidelity.

## Robustness

`obscura-js/runtime.rs` provides a V8 termination watchdog (`arm_watchdog`, `run_event_loop_bounded`) that terminates the isolate from a separate thread when synchronous work overruns a budget, because `tokio::time::timeout` cannot preempt synchronous V8. It bounds the post-load settle, the navigation event-loop pumps, and `--eval`. The complete script phase is bounded by `OBSCURA_SCRIPT_DEADLINE_MS`; enhancement modules have a shorter per-module graph-loading/evaluation budget controlled by `OBSCURA_MODULE_BUDGET_MS`, while modules mounting an empty SPA shell receive the full script deadline. `obscura-js/cdp_watchdog.rs` is a watchdog the dispatcher arms around CDP commands, to release the connection's V8 lock after an execution timeout (tunable via `OBSCURA_CDP_COMMAND_TIMEOUT_MS`). `op_dom` is wrapped in `catch_unwind` so a DOM-op panic degrades to a null result instead of aborting the process through V8's FFI frame, and `obscura-dom/tree.rs` rejects cyclic reparenting that would make tree walks loop forever. Scripted `fetch()`/XHR and module network requests are timeout-bounded (`OBSCURA_FETCH_TIMEOUT_MS`), and the one-shot `fetch` CLI has a process-level hard deadline as a final backstop.

## JS bridge

`obscura-js/js/bootstrap.js` provides the browser globals: `document`, `window`, `navigator`, `location`, observers, fetch, indexedDB, etc.

`DOM.setFileInputFiles` requires the existing `--allow-file-access` opt-in,
validates the target and file-path array before reading local files, and reports
selection failures as CDP errors. Remote handles retain their owning frame
rather than being reduced to root-DOM node numbers. The owning realm creates
the File objects and emits trusted input/change events; non-multiple inputs
select only the first file. In-memory DataTransfer payloads remain a separate API.

Each `serve` client connection gets an independent copy-on-write localStorage
snapshot of the startup profile and a fresh IndexedDB store. Untouched entries
remain shared even when another key in that origin changes. Only the separate
persistence context owns the profile directory: connection changes merge by
changed key at close, so navigation cannot overwrite another connection's
profile writes. New connections do not inherit other live clients' values.
Storage-budget failures during persistence are logged. Embeddable persistent
BrowserContext copies continue to share their profile stores.

The noninteractive dialog defaults dismiss `confirm()` (`false`), cancel
`prompt()` (`null`), and let `alert()` return without blocking. These functions
do not emit client-controlled CDP dialogs. `Page.handleJavaScriptDialog`
continues to resolve the separate beforeunload confirmation from `Page.close`.

`HTMLTableElement.rows` is a live, read-only HTMLCollection. Native traversal
orders header rows first, body/direct rows next, and footer rows last, excluding
nested tables and foreign namespaces. Reads reuse the collection until a DOM
mutation invalidates its row snapshot. Section `rows`, row `cells`, and table
`tBodies` share this cached, owning-realm collection implementation. Native
indices exclude nested tables and foreign namespaces. Table section access,
row/cell insertion and deletion, and `createTBody` preserve their actual DOM
parents and invalidate retained collections through ordinary DOM mutations.
This also supports Playwright's implicit table-header role calculation.
These collections retain the existing `rows` write-rejection behavior: indexed
and supported named assignments/redefinitions are rejected without changing
the collection. Chromium instead reports success for ignored writes, including
ignored nonconfigurable redefinitions; that reflection difference remains.
Wrapper classification keeps canonical HTML names on a single scalar native
read; prefixed or foreign names include namespace/local-name metadata in that
same read. Table construction and parser/wrapper workloads are covered by the
public performance smoke gate.

Select and option wrappers have distinct HTML interfaces. Select `length`,
`item`, `namedItem`, `remove`, `add`, value lookup, and option `index` use native
option ownership: nested HTML selects, datalists, and option subtrees do not
contribute options to the outer select. Numeric insertion targets retain their
actual optgroup parent. Length assignments above 100,000 are rejected with a
warning rather than attempting unbounded allocation. These APIs also support
borrowed methods on child-realm nodes. Select collections still use the existing
snapshot HTMLCollection implementation. Numeric select access uses V8 indexed
handlers, not a Proxy around ordinary DOM properties. Fresh selects allocate the
native wrapper directly; custom-element upgrades retain that same wrapper.
Select-only constructor state is isolated from ordinary HTML interface constructors.
The private derived constructor initializes the predeclared node slot in JavaScript
without allocating a discarded JavaScript element or setting each node through V8's C API.
Indexed reads reuse option
snapshots keyed by the native form mutation generation. Assignments replace or
remove the actual option and fill gaps with blank options. Non-null assignments
past the same 100,000 option limit throw RangeError. Supported indices remain
configurable and writable in reflection, while deletion is rejected and
redefinition uses the platform indexed setter. Named getters and existing select
methods retain their ordinary lookup paths.

CDP `DOM.querySelector` and `DOM.querySelectorAll` search descendants of the
requested node, including explicit shadow roots, without crossing into sibling
or nested shadow trees. Native selector scoping also handles `:scope`. Omitted
or zero node IDs retain the legacy document fallback; invalid node IDs and
invalid selectors return protocol errors rather than empty successful results.

Previously aliased HTML interfaces now use the same native wrapper factory as
table/select interfaces. Parsed, created and cloned HTML nodes receive distinct
interface prototypes and constructors; interface prototype changes do not affect
unrelated tags. Namespace/local-name checks avoid assigning those interfaces to
foreign or differently cased names. Existing specialized interfaces retain their
behavior. The generic `HTMLElement`/`HTMLUnknownElement` hierarchy is separate work.

`window.origin` (including the bare `origin` identifier) reports the owning
document's origin, including inherited blank-frame origins. Page assignment
replaces this property without changing the document's actual origin.

Form ownership and listed-control collection use a native, tree-scoped traversal.
Native ID and control snapshots are invalidated by ownership-relevant attribute
and tree mutations, not unrelated class/style/value updates,
so repeated reads do not rescan the document. A native generation also invalidates
owning-realm ID and owner snapshots through V8-allocated shared uint32 backing
storage, without an FFI call per cached getter. Atomic writes and weak observers
preserve the DOM tree's thread-move support and stop updates when the document
retires; replacing a runtime's DOM rebinds the buffer and advances its epoch.
Exporting a DOM also invalidates the runtime's cached getters and retires its
subscriber until a document is installed again.
Collection indices wrap nodes lazily, so reading length does not
construct every external control. Uncached owners use a panic-safe numeric op.
Connected controls' explicit `form` attributes resolve the first matching ID in
tree order, including non-form blockers. Disconnected controls use ancestry.
Parser associations preserve controls outside misnested forms
until ownership is reset by attribute or tree mutation. Collections and borrowed
getters stay in the control's owning document. Submission includes external
controls, live enabled select choices and the activated button's entry. JavaScript
and CDP clicks use the same owner, while `submit()` still bypasses the cancelable
event and `requestSubmit()` does not. Collections retain the existing snapshot
HTMLCollection behavior; remaining select state semantics are separate work.
The performance smoke gate covers collection reads, scaled owner getters and
unrelated-attribute mutation workloads alongside the general DOM scenarios.

Document-initiated navigation retains its owning URL for request metadata in
both HTTP transports, including interception continuations and native POSTs.
The default referrer policy strips fragments and cross-origin paths and omits
HTTPS-to-HTTP referrers; downgraded POSTs send `Origin: null`. Cross-origin
redirect returns retain origin/site taint and cannot restore stripped referrers.
Unsafe cross-site navigation does not
send Strict or Lax cookies. Direct API navigation has no document initiator.

Short, bounded PBKDF2 derivations retain a native fast path. Larger derivations
run on bounded blocking workers rather than the page thread. Dropping an
awaiting future cancels that background work; the worker retains its admission
permit until it exits. Limits are 10 million iterations, 1 MiB of output, and
1 billion iterations multiplied by hash-sized output blocks. Zero-iteration
or over-limit derivations reject with `OperationError`.

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

Storage objects use their original prototype to distinguish platform members from
stored names. Replacing or deleting `window.Storage` does not break either store;
changes to the original prototype still take effect.

## Stealth

`--stealth` swaps the default `reqwest` client for `obscura-net/wreq_client.rs`, which presents a real browser's TLS ClientHello, ALPN, and cipher order (a consistent Chrome fingerprint, not a randomized one) so the TLS layer matches the User-Agent and JS surfaces. It also applies the bundled tracker blocklist before any request leaves the process. Scripted `fetch()`/XHR go through the same stealth client, so subresource requests carry the same fingerprint as the navigation. `--stealth` is a global CLI flag that applies to `fetch`, `serve`, `scrape`, and `mcp`.

## Workspace conventions

- One crate per layer. Cross-crate calls go through the layer above, not sideways.
- All async is `tokio` with a `LocalSet` because V8 is `!Send`.
- All DOM ops go through `op_dom` to keep the JS/Rust boundary narrow.
