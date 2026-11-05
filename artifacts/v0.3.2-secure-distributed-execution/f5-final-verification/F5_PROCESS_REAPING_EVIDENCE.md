# F5 Process Reaping Evidence

## Implementation
- Worker agent calls worker.shutdown(5s) on lease loss.
- shutdown() sends shutdown command, waits grace period, then kills.
- kill_on_drop(true) on Command ensures reap on drop.

## Tests
- Blocked: Docker daemon down. Process-level tests cannot run against real PG+NATS.
- DB-level fencing tests added (stale token rejected, concurrent acquisition, expiry, stale completion, duplicate dispatch).

## Status
BLOCKED for process-level verification (Docker unavailable).
