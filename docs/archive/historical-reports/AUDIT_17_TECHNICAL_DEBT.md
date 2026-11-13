# AUDIT_17 — Technical Debt and Project Health

## 1. Dead code

- `obscura-browser/src/page.rs`: 9 unused CSS/font helpers.
- `obscura-worker-agent`: unused `hb_handle`.
- `obscura-cli`: unused `deferred_eval_output`.

## 2. Duplicate/abandoned

- Multiple historical report files (V0.x, PHASE*, F5_*) — documentation debt.
- Many branches (60+) — branch sprawl; not all merged.

## 3. Incomplete features

- Distributed MCP not in Docker image.
- Scheduler lacks binary.
- obscura-isolation minimal.
- WebSocket implementation not verified genuine.
- Font loading helpers dead code → font support possibly incomplete.

## 4. Hardcoded config

- Compose weak token.
- Default DB URL with password in code (dev default).
- Port defaults scattered.

## 5. Complexity

- Monolithic files (runtime.rs 26k lines, dom.rs 953 KB).
- Mixed concerns in control-plane single file (4k lines).

## 6. Missing tests

- SSRF crate no unit tests? (0 reported).
- No chaos testing.
- No load/perf in CI beyond scripts.

## 7. Operational debt

- Health check weak (kill -0).
- No readiness/liveness distinction.
- No DLQ.
- Metrics no auth.
- No runbooks.

## 8. Priority

See final report remediation roadmap.
