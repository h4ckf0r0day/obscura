# Phase 5.5 Runtime Verification

## Scheduler Runtime Verification

### Finding: Reconciliation NOT wired into run()

`Scheduler::run()` (line 189) does NOT call `reconcile()` or `reconcile_workers()`. It only:
1. Ensures stream.
2. Creates consumer.
3. Drains pending jobs.
4. Loops on NATS messages, scheduling pending jobs.

There is no background reconciliation task. `run_reconciliation_loop()` exists but is never invoked by `run()`.

**Classification: IMPLEMENTED BUT NOT RUNTIME VERIFIED (reconciliation exists as method but not called in operational path)**

### Worker Lease-Loss Verification

The worker agent calls `worker.shutdown(5s)` on lease loss (line 154). This sends SIGTERM to process group, waits 500ms, then SIGKILL. But:
- No test verifies this path executes.
- No test verifies child process termination on lease loss.

**Classification: IMPLEMENTED BUT NOT RUNTIME VERIFIED**

### Process Registry Verification
- Job-scoped registry exists (HashMap in worker agent).
- Used during execution and cleanup.
- No test verifies registry cleanup on panic or error paths.

**Classification: IMPLEMENTED BUT NOT RUNTIME VERIFIED**

### NATS Consumer Lifecycle
- `next_message()` creates new stream per call.
- No persistent stream storage.
- Ack policy: Explicit.
- MaxDeliver: 5.
- AckWait: default (not configured).

**Classification: IMPLEMENTED BUT INEFFICIENT**

### Tests Executed
- cargo check --workspace: PASS
- obscura-control-plane: 6/6 PASS (real PG)
- obscura-scheduler: 3/3 PASS (real PG+NATS)
- obscura-supervisor: 4/4 PASS (no Docker)

### Tests NOT Executed
- Scheduler reconciliation runtime: NOT TESTED (no test starts run() and verifies reconciliation)
- Worker lease-loss: NOT TESTED
- Process termination on lease loss: NOT TESTED
- NATS redelivery: NOT TESTED

### Conclusion
Phase 5.4 claimed runtime integration but `run()` does not invoke reconciliation. The worker agent's lease-loss path exists but is untested. F5 remains PARTIALLY RESOLVED.
