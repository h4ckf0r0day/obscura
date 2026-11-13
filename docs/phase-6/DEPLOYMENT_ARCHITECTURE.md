# Phase 6 Deployment Architecture

## Services
- PostgreSQL (authoritative job ledger)
- NATS JetStream (messaging)
- Control-plane (API/auth)
- Scheduler (`obscura-scheduler` binary, new)
- Worker-agent (`obscura-worker-agent`)
- Browser runtime (`obscura-runtime-worker`)
- Obscura supervisor (`obscura`)

## Scheduler binary
- New: `crates/obscura-scheduler/src/bin/obscura-scheduler.rs`
- Validates `DATABASE_URL`, `NATS_URL`, `OBSCURA_SERVICE_BOOTSTRAP_SECRET`.
- Runs reconciliation loop and scheduling loop; handles Ctrl-C.

## Compose changes
- Removed hardcoded weak CDP token; requires env var.
- TODO: add Postgres, scheduler, worker-agent services; remove 9100 host exposure or protect metrics.

## Health checks
- Current: `kill -0` only.
- Required: separate liveness/readiness/dependency checks.
