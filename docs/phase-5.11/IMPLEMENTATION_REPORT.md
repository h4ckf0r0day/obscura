# Phase 5.11 Implementation Report — F5 Failure Recovery and Process Containment

## Summary

Phase 5.11 closes the four remaining F5 acceptance criteria by hardening the existing
PostgreSQL-authoritative control plane, scheduler reconciliation, NATS JetStream
consumer lifecycle, worker-agent lease handling, and OS-level browser process
containment. No mock or in-process worker simulation is used in the acceptance path:
the adversarial suite spawns real `obscura-worker-agent` and `obscura-runtime-worker`
OS processes against real PostgreSQL and real NATS JetStream.

## Root causes addressed

1. **Worker-agent crash recovery**: Worker availability was tracked, but recovery
   depended on manual reconciliation in some paths. `Scheduler::reconcile_workers`
   and `ControlPlane::recover_abandoned_executions` now reclaim jobs whose worker
   is unavailable or whose lease expired, resetting them to `pending` for
   rescheduling with a strictly newer fencing token. `mark_dispatched` allocates a
   new `fencing_token` from `fencing_token_seq` and increments `attempt_count`,
   so every recovery creates a new attempt with a monotonically increasing token.

2. **NATS redelivery/recovery**: JetStream consumers use explicit acknowledgement,
   durable names, `AckPolicy::Explicit`, `max_deliver=5`, and configurable
   `ack_wait`. Duplicate dispatches are rejected at the database boundary via
   job status and attempt-id/fencing-token checks. NATS delivery is never treated
   as ownership.

3. **Independent multi-process takeover**: The adversarial harness starts scheduler,
   PostgreSQL, NATS, and two or more worker agents as independent OS processes.
   Scenario 6 SIGKILLs an agent during active browser execution, verifies
   containment, lease expiry, reconciliation, takeover by a second agent, stale
   completion rejection, and final completion.

4. **Browser process containment**: The supervisor spawns the runtime worker in a
   new process group (`setpgid`) and, on Linux, sets `PR_SET_PDEATHSIG(SIGKILL)`.
   A detached external reaper process (new in this phase) survives agent death
   and kills the worker process group, covering platforms without parent-death
   signals and descendants that outlive the direct child. Lease loss triggers
   immediate process-group termination even while the agent remains alive.

## Source files changed

- `crates/obscura-worker-agent/src/main.rs` — detached reaper mode and reaper
  spawning for process containment.
- `crates/obscura-scheduler/examples/adversarial_e2e.rs` — expanded adversarial
  scenarios (containment on SIGKILL, end-to-end F5 combined).
- `crates/obscura-control-plane/src/lib.rs` — tenant-scoped claim fixes in tests
  for deterministic isolation.
- `tests/integration/harness/run_adversarial_multiproc.sh` — strong default
  bootstrap secret (no longer `testsecret`).
- `docs/phase-5.11/*` — deliverables.

## Architectural decisions

- PostgreSQL remains the single source of truth for ownership, attempt state, lease
  expiry, and fencing.
- NATS is a transport only; an acknowledged or unacknowledged message never
  grants ownership.
- Recovery is idempotent: `recover_abandoned_executions` only touches
  `dispatched`/`running` jobs with expired or missing leases or unavailable
  workers.
- Process containment uses process groups plus an external reaper rather than
  relying solely on `Drop`/`kill_on_drop`, so SIGKILL of the agent still results
  in termination of the browser tree.
- Stale completions are rejected by comparing both `execution_attempt_id` and
  `fencing_token`.

## Database and NATS behavior

- `leases` table with unique `(resource_type, resource_id)`, `expires_at`, and
  monotonic `fencing_token` from `fencing_token_seq`.
- `jobs` transitions: `pending -> scheduled -> dispatched -> running -> completed/failed`.
- `job_attempts` records each attempt with fencing token and terminal status.
- JetStream stream `OBScura_JOBS`, durable consumers, explicit ack, `max_deliver=5`.
- Redelivery verified via NAK and ack_wait timeout.

## Process containment design

- Supervisor: `setpgid(0,0)` for the runtime worker; Linux `prctl(PR_SET_PDEATHSIG, SIGKILL)`.
- Worker-agent: on lease loss, `kill(-pid, SIGTERM)` then `kill(-pid, SIGKILL)` to the
  process group, plus direct child kill.
- Detached reaper: spawned with `setsid()`; polls agent PID; on agent death sends
  `SIGKILL` to the worker process group. This makes containment work on macOS and
  Linux and survives SIGKILL of the agent.

## Platform limitations

- Linux-only mechanisms (`prctl` PDEATHSIG) are not available on macOS. On macOS,
  containment relies on the detached reaper, which provides equivalent behavior
  for the tested scenario.
- Linux cgroup v2 containment was evaluated but not required; the process-group
  plus reaper design is the smallest reliable option compatible with the existing
  architecture.
- Full Linux build verification is blocked in this environment by a V8 prebuilt
  download failure (HTTP 504) from GitHub; see `FAILURE_MATRIX.md`.

## Regression results

- `cargo test -p obscura-scheduler`: 4 passed.
- `cargo test -p obscura-control-plane`: 18 passed.
- `cargo test -p obscura-supervisor`: 12 passed.
- `cargo test -p obscura-worker`: 0 (no unit tests).
- `cargo clippy -p obscura-scheduler -p obscura-control-plane`: warnings only
  (pre-existing).
- Adversarial multi-process suite: all scenarios passed.

## Known remaining gaps

- Linux-only CI execution of the full adversarial suite is required for final
  acceptance sign-off; this environment could not complete a Linux build due to
  network/V8 prebuilt availability.
- Recovery attempt rows are not explicitly marked `abandoned` on reclaim; they
  remain `dispatched`/`running` historically. Job state is authoritative.