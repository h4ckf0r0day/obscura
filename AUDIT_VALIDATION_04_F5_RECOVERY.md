# AUDIT VALIDATION 04 — F5 RECOVERY

## Git history
- Phase 5.5 commit 9a672b9: docs only (reconciliation not wired).
- Phase 5.6 commit 56eb2ab: `feat: wire reconciliation loop into scheduler run()` (+10 lines scheduler).
- Phase 5.7 commit d183653: docs only.
- Phase 5.8 commit 3851746: docs only.
- Current working tree: uncommitted source changes in control-plane, scheduler, isolation, worker-agent implementing recovery hardening. These are NOT committed. They constitute the actual Phase 5.8 implementation.

Conclusion: Phase 5.8 changed source code in the working tree (uncommitted), not in any commit. Documentation commits alone did not alter runtime.

## Operational execution path
Client → Control Plane → PostgreSQL → Outbox → NATS → Scheduler → Worker Agent → Supervisor → Browser Process → Result Persistence.

## Call graph (production entry points)
- Scheduler binary: NONE. `obscura-scheduler` is a library crate; no `[[bin]]`. There is no production scheduler process entry point in the repo. `run()` reachable only via tests/examples. This is a deployment gap (F-06).
- `Scheduler::run()` (lib):
  - ensure stream
  - create consumer
  - initial drain: `schedule_one` loop
  - spawn background task: loop `reconcile_workers()` every 10s
  - consumer loop: on `job.submitted`, `schedule_one` loop; ack; periodic `schedule_one`
- `reconcile_workers()` → `expire_stale_workers(10)` → `recover_abandoned_executions()` → `reconcile_worker_capacity()` → `schedule_one` loop
- Worker agent `main()`:
  - register worker, heartbeat task (5s default configurable)
  - supervisor start
  - NATS consumer loop on `obscura.jobs.dispatch`
  - on dispatch for this worker: acquire lease, mark dispatched, pick supervisor worker, register job handle, spawn renewal task (lease_renew secs), execute, on completion/failure call complete_job/fail_job, lease lost → kill process group + fail_job
- `complete_job` / `fail_job`: fencing token check (FOR UPDATE).

## Functions implemented but not operationally connected
1. `Scheduler::reconcile()` (calls `reconcile_stuck_jobs`) is NOT called by `run()`. Thus `reconcile_stuck_jobs` (reclaims scheduled >10s / dispatched stuck) is not invoked in the production loop. Gap.
2. `run_reconciliation_loop()` method exists but `run()` inlines its own loop; method unused.
3. `recover_abandoned_executions` and `reconcile_stuck_jobs` overlap; only the former runs periodically.

## Mandatory scenario analysis
1. Worker A receives dispatch and starts execution. ✓
2. Worker A acknowledges NATS message. ✓ (msg.ack after parse)
3. Worker A SIGKILL: heartbeat stops; lease stops renewing; lease expires.
4. Scheduler detects expired lease via `recover_abandoned_executions` (lease expired) or worker unavailable. ✓ (10s loop)
5. Worker B receives new attempt: reconcile loop reschedules to available worker B with higher fencing token. Test `test_scheduler_worker_crash_takeover` verifies. ✓
6. Worker A stale completion rejected: fencing token mismatch. Test verifies. ✓
7. Worker B completes successfully. Test verifies. ✓

Other scenarios:
- NATS consumer restart before ack: message redelivered; scheduler processes duplicate dispatch? Scheduler's consumer acks after parse; if restart before ack, redelivery triggers another schedule attempt; claim returns None (job no longer pending) → safe. Test `test_nats_redelivery_on_unacked_timeout` passes.
- NATS consumer restart after ack: no redelivery; scheduler loop continues via periodic schedule_one. OK.
- Scheduler restart during active execution: leases continue; new scheduler drains pending; reconciliation loop recovers abandoned on lease expiry. OK but stuck scheduled jobs not reclaimed (gap above).
- Worker crash during execution: as above.
- Lease loss while agent alive: renewal failure sends loss signal; kills process group; fail_job with retryable=true. ✓
- Browser descendants surviving agent death: supervisor uses process group + killpg SIGTERM/SIGKILL on lease loss; PDEATHSIG on Linux only. On macOS, descendants may survive agent death (no PDEATHSIG). Partial.
- Concurrent reconciliation: `FOR UPDATE`/SKIP LOCKED not used in recovery UPDATEs; concurrent runs could double-process but updates are idempotent-ish (status filters). Risk of duplicate recovery dispatch low but not formally serialized.
- Duplicate recovery dispatch: two schedulers both run reconciliation; `SKIP LOCKED` in claim prevents duplicate claim; recovery UPDATEs filter by status. Acceptable.

## Message redelivery vs ownership transfer
- Redelivery: NATS JetStream redelivers unacked messages; scheduler acks and ignores already-claimed jobs.
- Ownership transfer: scheduler-driven via lease expiry + recover_abandoned + reschedule with new fencing token.

## Status: PARTIALLY CONFIRMED (core recovery implemented and tested; gaps: no scheduler binary, reconcile_stuck_jobs not in run loop, macOS PDEATHSIG absent).
