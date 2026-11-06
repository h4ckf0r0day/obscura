# Process Failure E2E

## Status: BLOCKED (Docker daemon unstable, no multi-process harness)

### Verified
- DB-level fencing: 6 tests PASS
- Capacity reconciliation: 3 tests PASS
- Scheduler: 3 tests PASS

### Blocked
- Worker agent crash recovery E2E
- Process termination on lease loss
- Two-worker stale-result rejection
