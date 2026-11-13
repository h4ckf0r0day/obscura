# AUDIT_11 — Performance and Resources

## 1. Bottlenecks identified (static)

- Monolithic files (runtime.rs 1.17 MB) increase compile times.
- Repeated serialization in IPC (JSON per request) — worker IPC overhead.
- Scheduler `run()` busy-loops `schedule_one` in multiple places.
- Outbox publisher batch size 10; okay.
- Metrics using Atomic relaxed; low overhead.
- V8 isolate creation per page; potential memory growth; teardown tests exist.
- Rendering 18 test failures may indicate layout inefficiency but not measured.

## 2. Resource leaks

- Detached tasks (heartbeat, reconciliation) not tracked; on shutdown may linger.
- Process children: kill_on_drop ensures cleanup on drop, but detached spawn tasks may keep references.
- File descriptors: metrics server, CDP accept loops; accept_thread_survives_silent_connections test.

## 3. No benchmark numbers claimed

No new benchmarks run. Existing BENCHMARKS_V0.2.0.md documents older results. Performance claims in reports not independently verified.

## 4. Recommendations

- Add bounded channels/backpressure monitoring.
- Track JoinHandles for shutdown.
- Profile V8 isolate memory under load.
- Run existing perf_smoke.py in CI.
