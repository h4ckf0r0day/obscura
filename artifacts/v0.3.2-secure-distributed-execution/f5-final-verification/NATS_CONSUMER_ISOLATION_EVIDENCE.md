# NATS Consumer Isolation Evidence

## Implementation
- Consumer name configurable per worker agent.
- Each worker creates independent durable consumer.
- Filtering by dispatch.worker_id prevents duplicate execution.
- Build: cargo build -p obscura-worker-agent PASS.

## Tests
- BLOCKED: Two-worker E2E requires Docker (PG+NATS) + 2 worker agents + browser.
- DB-level fencing: 6 tests PASS.
