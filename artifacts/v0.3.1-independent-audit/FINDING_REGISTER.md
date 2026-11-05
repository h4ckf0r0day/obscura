# Canonical Finding Register — v0.3.1 Independent Audit

## Summary
Total findings: 15
P0: 0 | P1: 5 | P2: 5 | P3: 3 | P4: 2

## Register

| ID | Severity | Status | Title | Source File/Function | Evidence | Impact | Recommendation | Verification Criteria |
|---|---|---|---|---|---|---|---|---|
| F1 | P1 | PARTIALLY RESOLVED | Worker agent not tested for crash recovery | `crates/obscura-worker-agent/src/main.rs` | E2E passed (job completed); no crash test | Job may be stuck after agent crash | Add crash-recovery reconciliation + tests | Agent restart test passes |
| F2 | P1 | OPEN | Authentication not enforced | `crates/obscura-control-plane/src/lib.rs` (Principal unused) | Code inspection: no auth calls | Any caller can access any tenant | Enforce auth at boundaries | Auth rejection tests pass |
| F3 | P1 | OPEN | Authorization not enforced | `crates/obscura-control-plane/src/lib.rs` (authorize_tenant unused) | Code inspection | Cross-tenant access possible | Enforce in all operations | Cross-tenant tests pass |
| F4 | P1 | OPEN | Scheduler crash recovery not verified | `crates/obscura-scheduler/src/lib.rs` (reconcile) | reconcile only schedules pending | Jobs stuck after crash | Reclaim stuck jobs on startup | Crash-injection tests pass |
| F5 | P1 | OPEN | Fencing not wired to lease lifecycle | `crates/obscura-worker-agent/src/main.rs` (no acquire_lease) | Code inspection | Stale worker could overwrite | Wire acquire_lease into agent | Stale-worker adversarial test passes |
| F6 | P2 | OPEN | NatsConsumer recreates stream per call | `crates/obscura-control-plane/src/lib.rs:520-535` | Code inspection | Inefficient, potential message loss | Refactor to persistent stream | 1000-message consumption test passes |
| F7 | P2 | OPEN | Capacity accounting can drift | `crates/obscura-control-plane/src/lib.rs` (complete/fail/cancel) | Code inspection | active_jobs mismatch | Add reconciliation | Restart reconciliation test passes |
| F8 | P2 | OPEN | Worker heartbeat staleness not enforced in agent | `crates/obscura-worker-agent/src/main.rs` | Code inspection | Stale workers receive jobs | Add expiry check in agent | Heartbeat expiry test passes |
| F9 | P2 | OPEN | Tenant isolation query-level only | All control-plane queries | Code inspection | Cross-tenant access if ID known | Enforce auth + scoping | Cross-tenant tests pass |
| F10 | P2 | OPEN | SSRF protection literal-IP only | `crates/obscura-control-plane/src/lib.rs` (submit_job) | Code inspection | SSRF via DNS/redirect | Add DNS + redirect checks | SSRF test suite passes |
| F11 | P3 | PARTIALLY RESOLVED | Not all scheduler metrics updated at runtime | `crates/obscura-scheduler/src/lib.rs` | Some counters incremented | Incomplete observability | Update all metrics | Metric verification test |
| F12 | P3 | OPEN | Docker health check process-liveness only | `deploy/docker/Dockerfile.worker` | Code inspection | False healthy | Add readiness check | Docker smoke test |
| F13 | P3 | OPEN | JetStream dedup window not configured | `crates/obscura-control-plane/src/lib.rs` (ensure_stream) | Default config used | Dedup window unclear | Set explicit window | Dedup test |
| F14 | P4 | OPEN | Unused fields (scheduler_id, client) | `crates/obscura-scheduler/src/lib.rs`, `crates/obscura-control-plane/src/lib.rs` | Clippy warnings | Code noise | Remove dead code | Clippy clean |
| F15 | P4 | OPEN | Implementation reports overstate readiness | Multiple report files | Cross-reference with code | Misleading documentation | Correct reports | Doc review |

### Consistency Check
Total = 0 (P0) + 5 (P1) + 5 (P2) + 3 (P3) + 2 (P4) = 15. ✓
