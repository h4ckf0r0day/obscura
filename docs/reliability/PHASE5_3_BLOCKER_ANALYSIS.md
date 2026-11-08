# Phase 5.3 Blocker Analysis

## Investigation A: Worker-Agent SIGKILL

### Source Inspection
- Worker agent: `crates/obscura-worker-agent/src/main.rs`
- Supervisor spawn: `crates/obscura-supervisor/src/lib.rs:135`
- Process group: `setpgid(0,0)` in pre_exec (line 149)
- kill_on_drop: line 143
- PR_SET_PDEATHSIG: pre_exec, Linux only (line 159)

### Finding
The worker agent uses `pick_least_loaded()` at execution time (line 82). The handle is stored in a local variable. No job-scoped registry persists across the agent's lifetime.

On SIGKILL to the worker agent:
- The Rust process dies immediately.
- `kill_on_drop` never runs (no Drop executed on SIGKILL).
- Children are in a new process group (setpgid), so they are NOT killed by parent death.
- PR_SET_PDEATHSIG (Linux only) sends SIGKILL to the direct child, but not to grandchildren.
- The child process may survive and become orphaned.

### Classification: ARCHITECTURE LIMITATION
Process groups isolate children from parent death. PDEATHSIG protects only the direct child on Linux. No external supervisor exists to clean up.

### Minimal Correction
Add an external process supervisor or use Linux cgroups for process-tree containment. This is a significant architectural change.

## Investigation B: Lease Expiry and Scheduler Takeover

### Source Inspection
- Lease acquisition: `crates/obscura-control-plane/src/lib.rs:922`
- Lease renewal: `crates/obscura-control-plane/src/lib.rs:967`
- Scheduler reconcile: `crates/obscura-scheduler/src/lib.rs:163`
- reconcile_stuck_jobs: `crates/obscura-control-plane/src/lib.rs:873`

### Finding
- `reconcile()` only calls `reconcile_stuck_jobs()` which resets scheduled/dispatched jobs older than 5 min to pending.
- `reconcile_stuck_jobs()` filters by lease expiry (checks session_leases table).
- But the scheduler's `run()` loop only calls `schedule_one()` on each NATS message, not periodically.
- There is no background reconciliation loop running by default.

### Classification: IMPLEMENTATION GAP
Recovery logic exists but is not invoked automatically. The scheduler only reconciles when NATS messages arrive.

### Minimal Correction
Start `run_reconciliation_loop()` in the scheduler's `run()` method or as a separate task.

## Investigation C: NATS Redelivery

### Source Inspection
- NatsConsumer: `crates/obscura-control-plane/src/lib.rs:537-557`
- next_message(): creates new stream per call
- Ack policy: Explicit
- MaxDeliver: 5
- AckWait: default (not configured)

### Finding
- Each `next_message()` call creates a new `messages()` stream.
- This is inefficient but functional for low throughput.
- Redelivery after AckWait expiry should work (JetStream default AckWait ~30s).
- Duplicate delivery is handled by worker_id filtering (non-target workers ack and skip).

### Classification: IMPLEMENTATION (inefficient but functional)

### Minimal Correction
Store persistent stream handle; configure explicit AckWait.

## Investigation D: Worker B Takeover

### Source Inspection
- Worker agent: `crates/obscura-worker-agent/src/main.rs`
- Scheduler: `crates/obscura-scheduler/src/lib.rs`
- Control plane: `crates/obscura-control-plane/src/lib.rs`

### Finding
- Worker B can register and receive dispatches.
- But the scheduler's reconciliation doesn't automatically reassign expired jobs.
- Worker B would only receive the job if the scheduler publishes a new dispatch (which requires reconcile to run).

### Classification: IMPLEMENTATION GAP
Recovery dispatch not automatically triggered.

### Minimal Correction
Start reconciliation loop in scheduler; it will reclaim expired jobs and schedule them.

## Root Cause Summary

The fundamental blocker is that **recovery logic exists but is not wired into the operational runtime**. The scheduler's `run()` method doesn't start the reconciliation loop. The worker agent's heartbeat task doesn't check for unavailability. The process lifecycle doesn't have external supervision.

The implementation is complete at the method level but not at the integration level.

## Blocker Classification

| Blocker | Classification | Evidence |
|---|---|---|
| Process termination on lease loss unverified | IMPLEMENTATION + TEST HARNESS | Code exists (shutdown + kill) but no multi-process test |
| Two-worker stale-result rejection unverified | TEST HARNESS | DB-level fencing verified; process-level not tested |
| Full adversarial E2E unverified | TEST HARNESS | Single-worker E2E verified; multi-process not tested |
| Worker-agent crash recovery unverified | ARCHITECTURE + TEST HARNESS | No external supervisor; children may orphan on SIGKILL |
| NATS redelivery unverified | TEST HARNESS | Consumer exists; no redelivery test |
| Parent-death protection unverified | INFRASTRUCTURE (Linux) | PR_SET_PDEATHSIG added but untested (running on macOS) |
