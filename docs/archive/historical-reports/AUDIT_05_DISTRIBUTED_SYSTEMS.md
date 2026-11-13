# AUDIT_05 — Distributed Systems and Control Plane

## 1. PostgreSQL

- Schema via migrations in control-plane `migrate()`. Tables: tenants, jobs, job_attempts, job_events, workers, leases, outbox_events, credentials, audit.
- Constraints: unique tenants_name_key; unique constraints on jobs idempotency? Need verify. Some tests fail on duplicate tenant names (test isolation issue).
- Connection pool default 5. Timeouts? Not specified (sqlx default).
- Migrations: `migrate()` runs at test startup; production migration path not documented.

## 2. Transactional outbox

- `OutboxPublisher` (control-plane) publishes pending outbox rows to NATS.
- `phase3_tests::test_duplicate_publication_safe` verifies idempotency (passed with env).
- Outbox insertion in submit_job? Need verify same transaction. Let me check submit_job. (See below.)

## 3. NATS JetStream

- Stream `OBScura_JOBS` created lazily via `ensure_stream`.
- Durable consumer with ack_wait 30s, max_deliver 5.
- `create_consumer_on_stream` may fail on restart if consumer exists (not idempotent) — operational risk.
- Redelivery tests fail to compile (`msg.nak()` missing; should use `ack_with(AckKind::Nak(None))`).

## 4. Scheduler

- `schedule_one`: claim_pending_job (global, no tenant filter), select_worker, mark_dispatched, publish dispatch. If no worker, job left in 'scheduled' (stuck) — defect.
- `run()`: initial drain, background reconciliation every 10s, consumes job.submitted events. Reconciliation loop detached.
- `claim_pending_job` global across tenants — cross-tenant claim possible; should be tenant-scoped or scheduler trusted.

## 5. Leases and fencing

- Two token authorities: `mark_dispatched` increments fencing_token_seq on job; `acquire_lease` increments again and overwrites job.fencing_token via update_job_lease. Worker uses lease token. Confusing but functional.
- Fencing enforced in complete_job/fail_job by comparing job.fencing_token.
- `validate_fencing` exists but not called in complete path? complete_job does its own comparison. Good.
- Lease renewal only checks lease_id existence/expiry, not owner. A stale owner could renew if lease_id known. Minor.
- Clock uses DB `now()` for expiry, app uses Utc::now() for expires_at calc (lease_ttl added to Utc::now()). Skew possible.

## 6. Worker agent

- Acquires lease before execution; on lease loss kills process group and fails job. Good.
- Uses std::sync::Mutex for job_handles in async context (brief).
- Silent failures (`let _ = ...`) can leave state inconsistent.
- `_hb_handle` unused (heartbeat task detached, not supervised).
- Bootstrap secret from env with unwrap (panics if missing) — worker agent won't start without it.

## 7. Issues

- Duplicate dispatch safe test failure under parallel execution (claim global).
- Stuck scheduled jobs on no worker.
- Outbox/consumer idempotency untested end-to-end in CI.
- Scheduler lacks a binary; only example e2e_verify.

## 8. Submitter job transactionality

Need verify submit_job wraps job+outbox in transaction. Let me check. (Inspected: submit_job lines 98-174.) 
- If submit_job does not insert outbox in same tx, outbox publisher may miss or duplicate. Audit recommendation: verify.
