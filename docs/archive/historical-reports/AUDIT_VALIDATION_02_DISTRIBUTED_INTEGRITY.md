# AUDIT VALIDATION 02 — DISTRIBUTED INTEGRITY

## F-03 Scheduler stuck jobs — PARTIALLY CONFIRMED / LARGELY FIXED in working tree

### Source
- `crates/obscura-scheduler/src/lib.rs`: `schedule_one` / `schedule_one_for_tenant`
- `crates/obscura-control-plane/src/lib.rs`: `claim_pending_job_for_tenant`, `release_job_to_pending`, `select_worker` (scheduler)

### Evidence
Working tree (uncommitted) modified `schedule_one_for_tenant`:
- claim query now includes `EXISTS (healthy worker with capacity and tenant match)` so no claim when no eligible worker.
- on `select_worker` failure, calls `release_job_to_pending(job.job_id)`.
- `claim_pending_job_for_tenant(tenant_id: Option<Uuid>)` replaces global claim.

### Original defect
At HEAD (committed), `schedule_one` claimed a pending job unconditionally, then `select_worker` could fail leaving job in `scheduled` (or claimed but undispatched). Reconciliation only reclaimed after 5 minutes.

### Current state
- With no eligible worker, claim returns None → job stays `pending`. Not stuck in `scheduled`.
- If worker disappears between claim and dispatch, job released to `pending`.
- `reconcile_stuck_jobs` reclaims `scheduled` >10s and `dispatched` with expired/missing lease.
- `recover_abandoned_executions` reclaims `dispatched`/`running` jobs on unavailable workers or expired leases.

### Residual risk
- `release_job_to_pending` only updates if status='scheduled'; if already transitioned, no-op (acceptable).
- Claim+dispatch not atomic; NATS publish failure after mark_dispatched could leave dispatched job without message. Reconciliation + redelivery partially covers.
- `schedule_one` global path uses tenant filter None; OK for global scheduler.

### Status: PARTIALLY CONFIRMED (original defect real at HEAD; substantially mitigated in uncommitted working tree). Acceptance tests: `test_scheduler_worker_crash_takeover`, `test_multi_scheduler_no_duplicate_claim` pass.

## F-04 Tenant-scoped job claiming — PARTIALLY CONFIRMED

### Source
- `claim_pending_job` / `claim_pending_job_for_tenant`
- scheduler `select_worker`

### Evidence
- Global claim (`claim_pending_job` → `claim_pending_job_for_tenant(None)`) allows a global scheduler to claim across tenants. This is arguably legitimate for a shared scheduler.
- Each claim is tenant-scoped when caller passes tenant; capacity check ensures worker tenant match or global worker (tenant_id NULL).
- `authorize_tenant` enforced on API calls; internal paths use `Principal::service("internal")`.

### Cross-tenant scenario
A global scheduler claims tenant A job and dispatches to a worker registered for tenant A or a global worker. Job remains tenant A. No data leakage observed in code paths. However, a global worker (tenant_id NULL) can process any tenant's job; acceptable if intended, but must be documented. Tests use unique tenant names to avoid collisions.

### Status: PARTIALLY CONFIRMED (global claiming is by design; isolation preserved but relies on worker tenant assignment + authorization).

## F-13 Idempotency uniqueness — FALSE POSITIVE (schema already enforces)

### Source
- `submit_job` (control-plane)
- DB constraint `jobs_tenant_id_idempotency_key_key UNIQUE (tenant_id, idempotency_key)`

### Evidence
- Unique constraint is tenant-scoped, matching intended rule.
- `submit_job` checks existing key then inserts; race can cause unique violation error rather than returning existing, but cannot create duplicate jobs.
- Concurrent submissions: one succeeds, other errors (Conflict) instead of returning existing job. Partial UX issue, not data integrity.

### Status: FALSE POSITIVE for duplicate-job claim; PARTIALLY CONFIRMED for race returning error instead of existing. Recommended: handle unique violation by re-reading existing job.

## F-14 Completion validation — PARTIALLY CONFIRMED

### Source
- `complete_job`, `fail_job` (control-plane)

### Evidence
- `complete_job` validates fencing token equality against current job token (FOR UPDATE). Old attempt with stale token rejected (test `test_stale_completion_rejected_after_new_owner` passes).
- However, `attempt_id` parameter is not validated against `jobs.execution_attempt_id`; completion updates `job_attempts` by attempt_id without verifying ownership.
- No status check: completing an already-completed job with same token re-runs completion (idempotency/replay possible if token reused; tokens are monotonic newval so unlikely).
- `fail_job` similarly token-checked but no attempt_id/status validation.

### Stale completion scenario
Worker A completes with old fencing token → rejected. Worker B completes with new token → accepted. Test passes.

### Residual risk
- Missing attempt_id binding and terminal-state guard. Recommend adding `WHERE status IN ('dispatched','running')` and attempt_id match.

### Status: PARTIALLY CONFIRMED.
