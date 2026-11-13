# AUDIT_04 — Browser Engine and Runtime

## 1. Lifecycle

- Browser/context/page creation in `obscura-browser` (context.rs, page.rs 500 KB).
- DOM init, JS init, network init per page.
- Shutdown: page close, context close, browser drop. `crates/obscura/tests/concurrent_isolate_teardown.rs` exercises teardown.
- Resource leaks: `css_resource_urls` etc. dead code; font/CSS resource loading partially implemented.

## 2. DOM correctness

- `obscura-dom/src/tree.rs` (110 KB), selector.rs (72 KB), serialize.rs, tree_sink.rs.
- Shadow DOM, frames: child frame tests show 3 failures (a_shadow_dom_child_frame_stays_alive, a_static_child_frame_runs_its_own_script, navigating_blank_releases_child_realms) — real defects in child frame script execution.
- Node identity, mutation observers partially.

## 3. JavaScript runtime (V8/deno_core)

- `obscura-js/src/runtime.rs` 1.17 MB monolith. Isolate lifecycle, execution contexts, module loading, timers, microtasks.
- Thread-affinity: V8 isolate !Send; code uses `IsolateHandle`. Risk areas where async tasks touch isolate must be audited individually; not fully verified.
- `import_map.rs:235` clippy `never_loop` — likely a prefix-resolution bug (loop returns on first match).

## 4. Rendering

- `obscura-render` crate: CSS (390 KB), style (419 KB), layout (taffy), paint (813 KB), inline (223 KB), dom (953 KB).
- 18 test failures in obscura-render (layout_test). Need investigate; likely layout edge cases.
- Screenshots/geometry coordinate systems: not independently verified; potential mismatch.

## 5. Network

- HTTP/HTTPS via reqwest + wreq (stealth). TLS rustls. Cookies, redirects, compression, proxy, SOCKS.
- SSRF guard shared (obscura-ssrf) with DNS resolution check.
- WebSocket: need verify genuine implementation vs shim. Search indicates tokio-tungstenite dependency. Partial audit; not confirmed end-to-end.
- Connection pooling via reqwest default.

## 6. Stubs / partial implementations

- `window.scrollTo/scrollBy/scroll` previously no-op stubs (fixed per comment).
- `navigator` eventtarget stubs.
- Many "compatibility" shims in runtime.rs.
- `css_resource_urls` and related font helpers are dead code — font loading may be incomplete.

## 7. Test failures

- obscura-render: 18 failures.
- obscura: 3 child-frame failures.
- obscura-net: 1 failure.
- obscura-js: 1 failure.
- obscura-browser: 1 failure.

See AUDIT_12.
