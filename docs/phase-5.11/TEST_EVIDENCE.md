# Phase 5.11 Test Evidence

## Environment

- OS: macOS Darwin 27.2.0 (arm64) for local execution; Linux verification blocked
  (see FAILURE_MATRIX.md).
- PostgreSQL: docker `postgres:16-alpine` on 127.0.0.1:5432.
- NATS: docker `nats:2.10-alpine -js` on 127.0.0.1:4222.
- Bootstrap secret: `phase5-11-integration-secret` (not the rejected `testsecret`).

## Exact commands

```bash
# Build
cargo build --release -p obscura-worker --bin obscura-runtime-worker
cargo build --release -p obscura-worker-agent
cargo build --release -p obscura-scheduler --example adversarial_e2e

# Adversarial suite
OBSCURA_ALLOW_PRIVATE_NETWORK="true" \
OBSCURA_SERVICE_BOOTSTRAP_SECRET="phase5-11-integration-secret" \
DATABASE_URL="postgres://postgres:obscura@127.0.0.1:5432/obscura" \
NATS_URL="nats://127.0.0.1:4222" \
cargo run --release -p obscura-scheduler --example adversarial_e2e

# Unit/integration tests
OBSCURA_SERVICE_BOOTSTRAP_SECRET="phase5-11-integration-secret" \
DATABASE_URL="postgres://postgres:obscura@127.0.0.1:5432/obscura" \
NATS_URL="nats://127.0.0.1:4222" \
cargo test -p obscura-scheduler

OBSCURA_SERVICE_BOOTSTRAP_SECRET="phase5-11-integration-secret" \
DATABASE_URL="postgres://postgres:obscura@127.0.0.1:5432/obscura" \
NATS_URL="nats://127.0.0.1:4222" \
cargo test -p obscura-control-plane

cargo test -p obscura-supervisor
cargo test -p obscura-worker
```

## Test counts

- `obscura-scheduler`: 4 passed.
- `obscura-control-plane`: 18 passed.
- `obscura-supervisor`: 12 passed.
- `obscura-worker`: 0 tests.
- Adversarial multi-process suite: all scenarios passed.

## Actual process and ownership evidence (from /tmp/adv_final.log)

- Worker Agent A: `b649d234-84ff-4338-abc7-d52050903286`
- Worker Agent B: `60ce4e41-80ee-47de-ba09-5fa4bc6fc324`
- Worker Agent C (containment): `e3670f86-d6e3-425b-81a0-cb6863c4e0f9`, child PID `27582`
- Job 3 (takeover): `ffd5ffbe-947d-480d-9265-f0d3f03c8726`
- Original fencing token: `471`
- Replacement fencing token: `472` (strictly newer)
- Stale completion result: `Conflict("stale fencing token")` (rejected)
- Scenario 6 combined: passed (SIGKILL during active execution, containment, lease
  expiry, reconciliation, takeover, stale rejection, single completion).

## NATS evidence

- Stream: `OBScura_JOBS`, subject `obscura.jobs.>`.
- Durable consumers with explicit ack, `max_deliver=5`.
- Scenario 4 verified redelivery on NAK and after ack_wait timeout.
- Message identity via `Nats-Msg-Id` header (event_id).

## Process-tree evidence

- Child browser worker PID `27582` alive during execution.
- After SIGKILL of agent, `libc::kill(pid, 0)` returned `-1` with `errno=ESRCH`.
- Containment verified without relying on graceful shutdown.

## Final PostgreSQL state (representative)

- Recovered job returns to `pending` then `completed` under the replacement worker.
- `job_attempts` records both the failed/original attempt and the successful
  replacement attempt.
- Stale attempt completion rejected; final result persisted exactly once.