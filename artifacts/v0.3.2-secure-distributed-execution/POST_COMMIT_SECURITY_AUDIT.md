# Post-Commit Security Audit — v0.3.2 (f59b60a)

## Audited Commit
f59b60a0e9544b3210b09ed6a0ce7eb373c2091a

## Working Tree
Clean.

## WP1 — Authentication & Authorization Bypass Audit

### Caller Matrix

| Operation | Caller | Principal Supplied? | Authorization Enforced? | Trust Boundary |
|---|---|---|---|---|
| submit_job | control-plane tests | None | No (bypassed) | Library (no trust boundary) |
| submit_job | scheduler tests | None | No | Library |
| submit_job | phase3 tests | None | No | Library |
| submit_job | worker agent | None | No | Library |
| get_job | control-plane tests | None | No | Library |
| get_job | scheduler tests | None | No | Library |
| list_jobs | control-plane tests | None | None | Library |
| cancel_job | control-plane tests | None | No | Library |
| cancel_job_phase4 | scheduler | None | No | Library |
| submit_job (internal idempotency) | control-plane | None | No | Internal |

### Assessment
- **ALL callers pass `None` for Principal.** No caller supplies a principal.
- `authorize_tenant` is defined but never called by any operation.
- `None` bypasses authorization entirely (the `if let Some(p)` guard skips the check).
- An untrusted caller can supply an arbitrary tenant_id and access that tenant's data.
- No worker identity authentication exists.
- No administrative permission checks exist.
- No external API exists; control plane is a library.

### Classification: BYPASSABLE

F2 and F3 remain OPEN. Authentication and authorization are NOT enforced.

## WP2 — Fencing Audit

### Execution Lifecycle (Actual)

1. Scheduler calls `claim_pending_job()` → job status = scheduled.
2. Scheduler calls `select_worker()` → picks least-loaded worker.
3. Scheduler calls `mark_dispatched(...)` → generates fencing_token via `fencing_token_seq`, stores in `jobs.fencing_token` and `job_attempts.fencing_token`, status = dispatched.
4. Scheduler publishes `DispatchMessage` (includes fencing_token) to NATS subject `obscura.jobs.dispatch`.
5. Worker agent consumes message, checks `dispatch.worker_id == worker_id`.
6. Worker agent creates session, sends Navigate command to supervised worker.
7. Worker agent calls `complete_job(job_id, tenant_id, attempt_id, fencing_token, result)`.
8. `complete_job` validates `fencing_token == jobs.fencing_token` in a transaction; rejects if mismatch.

### Answers
1. Does worker acquire lease before execution? **No.** Worker agent does not call `acquire_lease`. Lease methods exist but are not wired into the agent.
2. Does it renew during execution? **No.**
3. Does execution stop after lease loss? **No.** No lease is held.
4. Can stale worker continue? **Yes.** No lease expiry check in agent.
5. Does newer attempt get higher token? **Yes.** `fencing_token_seq` is monotonic.
6. Can older worker persist result? **No (if token differs).** Token validation rejects mismatched tokens. But if the same token is reused (no new dispatch), an old worker could replay.
7. Are token and attempt checked transactionally? **Yes.** `complete_job` does `SELECT ... FOR UPDATE` then compares token in same transaction.
8. Can duplicate dispatches create competing executions? **Potentially.** If the same dispatch message is redelivered, the worker agent re-executes (no dedup by dispatch_id). But `mark_dispatched` only succeeds if status='scheduled', so duplicate scheduling is prevented. However, the worker agent itself has no dedup guard.

### Sequence Diagram (Actual)

```
Scheduler                          Worker Agent                    PostgreSQL
   |                                    |                             |
   |-- claim_pending_job() ------------->|                             |
   |                                    |                             |
   |-- mark_dispatched() -------------->| (generates fencing_token)    |
   |                                    |                             |
   |-- publish DispatchMessage (NATS) -->|                             |
   |                                    |                             |
   |                                    |-- consumes dispatch          |
   |                                    |-- dispatch.worker_id check   |
   |                                    |-- create session            |
   |                                    |-- supervisor.dispatch(nav)  |
   |                                    |-- complete_job(token) ----->|
   |                                    |                             |-- SELECT FOR UPDATE
   |                                    |                             |-- token == current? YES
   |                                    |                             |-- UPDATE completed
```

### Assessment: PARTIALLY ENFORCED

Fencing token validation exists at the DB level, but lease lifecycle is not wired into worker execution. Stale-worker rejection has not been tested with a real stale worker.

## WP3 — Security Test Audit

| Test | Exists? | Executed? | Result |
|---|---|---|---|
| Missing principal | No | No | N/A |
| Invalid principal | No | No | N/A |
| Cross-tenant read | No | No | N/A |
| Cross-tenant cancellation | No | No | N/A |
| Worker impersonation | No | No | N/A |
| Stale fencing token | No | No | N/A |
| Concurrent lease acquisition | No | No | N/A |
| Lease expiration | No | No | N/A |
| Stale result rejection | No | No | N/A |
| Duplicate dispatch | No | No | N/A |

**No security tests exist for the new v0.3.2 auth/fencing functionality.**

## WP4 — Static Verification

- `cargo fmt --all -- --check`: PASS (with nightly feature warnings, non-blocking)
- `cargo check --workspace`: PASS
- `cargo clippy -p obscura-control-plane -p obscura-scheduler -p obscura-worker-agent`: PASS (warnings only in obscura-js, pre-existing)
- `cargo test --workspace`: NOT RUN (Docker daemon down during this audit)
- Integration tests (PG+NATS): BLOCKED (Docker daemon down)

## WP5 — Findings and Verdict

### Remaining Bypasses
1. **Auth bypass:** All callers pass `None` for Principal. `authorize_tenant` is never called. Any caller can access any tenant's data by supplying the tenant_id.
2. **Fencing gap:** Lease lifecycle not wired into worker agent. Token is generated and validated, but no lease is acquired/renewed/expired. A stale worker could continue execution (though result persistence would be rejected if token differs).
3. **NatsConsumer:** Creates new stream per `next_message()` call. Inefficient but functional for low throughput.
4. **Capacity accounting:** `active_jobs` incremented/decremented on all paths but no reconciliation on restart. Can drift.
5. **SSRF:** Only literal-IP check in `submit_job`. No DNS resolution, redirect, or IPv6-mapped checks.

### Source Locations
- Auth: `crates/obscura-control-plane/src/lib.rs` (Principal struct ~line 850, authorize_tenant ~line 860)
- Fencing: `crates/obscura-control-plane/src/lib.rs` (mark_dispatched ~line 735, complete_job ~line 770, fail_job ~line 795)
- NatsConsumer: `crates/obscura-control-plane/src/lib.rs:503-535`
- Capacity: `crates/obscura-control-plane/src/lib.rs` (mark_dispatched, complete_job, fail_job, cancel_job)
- SSRF: `crates/obscura-control-plane/src/lib.rs` (submit_job, ~line 100)

### Recommended Corrective Actions
1. **Immediate:** Enforce auth by requiring Principal at all call sites. Make `None` not allowed in production.
2. **Immediate:** Wire `acquire_lease` into worker agent before execution.
3. **High:** Implement scheduler crash recovery (reclaim stuck jobs).
4. **High:** Fix NatsConsumer to reuse stream.
5. **Medium:** Add comprehensive SSRF test suite.
6. **Medium:** Add crash-injection tests.

### Verdict on F2, F3, F5
- **F2 (Authentication):** REMAINS OPEN. `None` bypasses all authorization. No caller supplies a principal.
- **F3 (Authorization):** REMAINS OPEN. `authorize_tenant` is never called.
- **F5 (Fencing):** PARTIALLY CLOSED. Token validation exists at DB level, but lease lifecycle not wired into worker agent. Stale-worker rejection not tested.

### Next Smallest Corrective Task
Implement a `require_auth` enforcement mode where `None` is rejected (not just skipped). This makes the auth boundary real without breaking existing tests (tests can pass `Some(test_principal)`).

## Final Assessment

The v0.3.2 changes added auth/fencing primitives and wired token validation into result persistence, but the enforcement is **bypassable** because all callers pass `None`. The security properties are **not actually enforced** in the current codebase.

**Verdict: NOT READY for security-sensitive deployment.** The architecture is in place but enforcement is incomplete.
