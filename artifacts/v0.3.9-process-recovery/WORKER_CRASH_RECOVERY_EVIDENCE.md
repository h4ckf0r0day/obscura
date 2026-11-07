# WORKER_CRASH_RECOVERY_EVIDENCE

## Status: PARTIALLY IMPLEMENTED

### Verified
- DB-level fencing: 6/6 tests PASS (real PostgreSQL)
- Scheduler: 3/3 tests PASS (real PostgreSQL + NATS)
- Supervisor process lifecycle: 4/4 tests PASS (no Docker)
- Worker agent E2E: PASS (single worker, job completed)
- Multi-worker coexistence: VERIFIED (2 agents, job completed)
- Capacity reconciliation: 3/3 tests PASS (real PG)
- Heartbeat runtime: IMPLEMENTED (periodic 5s task in worker agent)
- Lease renewal: IMPLEMENTED (background task, 5s interval, 30s lease)
- Graceful termination on lease loss: IMPLEMENTED (shutdown with grace period)

### Not Implemented / Not Verified
- Worker-agent crash recovery (not tested)
- NATS redelivery/recovery (not tested)
- Multi-process E2E (blocked by Docker/harness)
- Process-level termination on lease loss (not verified with real process)

### Tests
- cargo check --workspace: PASS
