# AUDIT_10 — Database and Data Integrity

## 1. Schema/migrations

- Migrations embedded in control-plane `migrate()` (sqlx). Tables created at runtime.
- Unique constraint `tenants_name_key` causes test collisions (tests use fixed names).
- Need inspect migration SQL for FKs, indexes, constraints. (Not fully extracted; embedded in Rust.)

## 2. Queries

- Parameterized (sqlx) — no SQL injection.
- `claim_pending_job` selects across all tenants (global) with SKIP LOCKED.
- `recover_abandoned_executions` reclaims dispatched/running jobs with expired leases or unavailable workers; updates to pending. Could double-dispatch if lease still valid? Guarded by lease expiry.
- `reconcile_worker_capacity` recomputes active_jobs from jobs status. Good.
- `reconcile_stuck_jobs`: need inspect. (Lines 896-918.) 

## 3. Integrity concerns

- Idempotency key uniqueness not enforced at DB level? submit_job checks application-level first; race could insert duplicate idempotency key. Need unique constraint on (tenant_id, idempotency_key). Not verified present.
- Lease upsert ON CONFLICT updates fencing_token but keeps old lease_id? Actually on conflict updates owner/fencing/expires; lease_id column not updated (so stored lease_id stale on conflict). But conflict path only when expired deleted first? Expired rows deleted before check; existing non-expired returns error. So insert path for new/expired. On conflict (race) could stale lease_id. Minor.
- `complete_job` doesn't verify attempt_id matches or status transition; could mark already-completed job completed again. Fencing check mitigates cross-owner but same token replay possible.
- `fail_job` resets to pending for retry but doesn't increment? attempt_count already incremented at dispatch. Good.
- Timestamps use DB now() and app Utc::now() mixed.

## 4. Pagination

- `list_jobs` limit param; need check offset. Not audited.

## 5. Concurrency

- FOR UPDATE SKIP LOCKED used in claim. Good.
- Worker capacity updates not atomic with job status in some paths? mark_dispatched increments active_jobs in same tx. Good.
- complete_job decrements active_jobs via subquery. Good.

## 6. Test issues

- Fixed tenant names cause duplicate key failures (test isolation).
- Need unique names in tests (some already use Uuid, some don't).
