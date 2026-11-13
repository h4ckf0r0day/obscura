# AUDIT_03 — Rust Engineering

## 1. Build/analysis results

- `cargo check --workspace`: PASS (warnings only).
- `cargo fmt --check`: FAIL (formatting drift, e.g. crates/obscura/src/browser.rs import ordering, cookie.rs).
- `cargo clippy --workspace --all-targets`: FAIL — 9 errors:
  - `crates/obscura-js/src/import_map.rs:235` `never_loop` (loop that returns on first iteration).
  - `crates/obscura-js/src/runtime.rs` ×8 `unused_io_amount` (`.read(...).unwrap()` ignoring byte count) at lines ~22401, 23135, 24908, 24939, plus 4 more.
- Warnings: hundreds (dead_code in page.rs ~9 functions; deprecated Atomic::fetch_update; unused variables in worker-agent, scheduler, cli).

## 2. Unsafe code inventory

81 `unsafe` occurrences across crates. Key sites:

| File | Lines | Purpose | Safety invariant documented? |
|---|---|---|---|
| crates/obscura-js/src/runtime.rs | 474-584, 754, 887 | V8 isolate enter/exit, raw pointers | Partially |
| crates/obscura-js/src/ops.rs | 6768 | raw pointer | ? |
| crates/obscura-supervisor/src/lib.rs | 147-158, 415, 433-438 | setpgid, getppid, kill, prctl | Partially |
| crates/obscura-cli/src/main.rs | 416-665, 1140 | process/signal | Partially |
| crates/obscura-cdp/src/server.rs | 741, 749, 768 | mallopt | No (allocator tuning) |
| crates/obscura-browser/src/idle_deadline.rs | 98-223 | dispatch_source (macOS) FFI | Partially |
| crates/obscura-render/src/style.rs | 4173 | GridCalcExpression raw ptr | ? |
| crates/test-support/allocations.rs | 22-31 | global allocator | Test only |

Many unsafe blocks lack SAFETY comments (not verified exhaustively). V8 thread-affinity: runtime objects are !Send; code must not move isolate across tasks. Audit of every spawn crossing isolate needed (see AUDIT_04).

## 3. unwrap/expect usage

- `.unwrap()`: 5,636 occurrences.
- `.expect()`: 1,373 occurrences.
- `panic!`: 75.
- Many in test code, but production paths also use unwrap on env vars (`OBSCURA_SERVICE_BOOTSTRAP_SECRET`), DB rows, etc. A missing env var panics the worker agent (acceptable startup) but runtime unwrap on row columns can panic on schema drift.

## 4. Async/concurrency concerns

- Locks: `parking_lot::Mutex`, `tokio::sync::Mutex`, `std::sync::Mutex`, `RwLock`. Several `std::sync::Mutex` used in async contexts (e.g. `job_handles: Arc<StdMutex<HashMap>>` in worker-agent) — held briefly; okay if no await while locked. Need verify no `.lock().unwrap()` held across `.await`. Not fully audited.
- `tokio::spawn` detached tasks: heartbeat in worker-agent (`_hb_handle` unused, warning), scheduler reconciliation loop, stdout reader tasks in supervisor. Detached tasks not tracked for shutdown.
- Channel capacity 128 in supervisor writer; backpressure possible but not monitored.
- `JoinHandle` drops in some places (detached).

## 5. Dead code

- `crates/obscura-browser/src/page.rs`: ~9 unused helper functions (css_resource_urls, push_css_url, etc.) — dead code warnings.
- `obscura-worker-agent`: unused variable `hb_handle`.
- `obscura-cli`: unused `deferred_eval_output`.
- `obscura-cdp`: deprecated `fetch_update`.

## 6. Large modules / complexity

- `runtime.rs` 26,867 lines, 921 functions — extreme. `ops.rs` 376 KB. `dom.rs` (render) 953 KB. Maintainability and review risk.
- Single-file modules prevent effective code review; many unrelated concerns co-located.

## 7. Feature-gated gaps

- `render` feature gates large code paths; `distributed` feature gates MCP distributed tools. Combinations (render + distributed) not exercised in CI (Docker builds only `--features render`).

## 8. Error handling consistency

- Mixed `anyhow`, `thiserror`, `ControlPlaneError`, `ObscuraNetError`, `SupervisorError`. Conversion via `?` generally. Some `.unwrap()` on fallible ops in production (env vars, DB rows).
- Silent error suppression: `let _ = msg.ack().await;`, `let _ = cp.fail_job(...)` in worker-agent — failures swallowed, can leave inconsistent state.

## 9. Integer/overflow

- Fencing tokens use `i64` from DB sequence; fine.
- Atomic counters u64; okay.
- No obvious overflow except unbounded collections (outbox, job_events) — DB-backed.

## 10. Conclusion

Rust core compiles but clippy fails; formatting drift; heavy unwrap usage; large monolithic files; detached tasks. Quality score: 3/5.
