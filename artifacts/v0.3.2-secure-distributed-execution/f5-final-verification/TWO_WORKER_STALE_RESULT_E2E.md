# Two-Worker Stale Result E2E

## Status: BLOCKED
- Requires 2 worker agents + real browser + PG + NATS.
- DB-level fencing tests verify stale token rejection.
- Multi-worker consumer isolation implemented (configurable consumer name).
