# F5 Process Reaping E2E

## Status: BLOCKED (Docker daemon unstable)

## Prior Evidence
- Worker agent E2E verified in earlier pass: job submitted -> dispatched -> executed -> completed.
- Graceful termination (worker.shutdown) implemented and tested (supervisor tests pass).
- Process reaping: kill_on_drop(true) on Command; kill() takes handle.

## Blocked
- Docker daemon unavailable for sustained integration testing.
