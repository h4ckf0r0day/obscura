# NATS_DELIVERY_ACTUAL_LINUX_EXPERIMENT_REMAINING_BLOCKERS

## Status: PARTIALLY IMPLEMENTED

### Verified
- DB-level fencing: 6/6 tests PASS (real PostgreSQL)
- Scheduler: 3/3 tests PASS (real PostgreSQL + NATS)
- Supervisor process lifecycle: 4/4 tests PASS (no Docker)
- Worker agent E2E: PASS (single worker, job completed)
- Multi-worker coexistence: VERIFIED (2 agents, job completed)
- Capacity reconciliation: 3/3 tests PASS (real PG)
- Job-scoped handle registry: IMPLEMENTED (HashMap in worker agent)
- Process group creation: IMPLEMENTED (setpgid in pre_exec)
- Process group termination: IMPLEMENTED (kill(-pgid, SIGTERM/SIGKILL))
- Linux PDEATHSIG: IMPLEMENTED (pre_exec, Linux only)
- PDEATHSIG race handling: IMPLEMENTED (getppid check after registration)

### Not Implemented / Not Verified
- Worker-agent crash recovery (not tested)
- NATS redelivery/recovery (not tested)
- Multi-process E2E (blocked by Docker/harness)
- Process-level termination on lease loss (not verified)

### Tests
- cargo check --workspace: PASS
