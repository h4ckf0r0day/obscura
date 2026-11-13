# AUDIT_02 — Architecture and System Design

## 1. High-level architecture

Obscura Next is two systems in one repository:

1. **Browser engine (single-host):** HTML/CSS/layout/paint + V8 JS runtime + network + CDP server + MCP server + CLI. Crates: obscura-dom, obscura-render, obscura-js, obscura-net, obscura-browser, obscura-cdp, obscura-mcp, obscura-cli, obscura.
2. **Distributed execution control plane:** PostgreSQL-backed jobs/leases/workers/outbox + NATS JetStream + scheduler + worker-agent + supervisor + process isolation. Crates: obscura-control-plane, obscura-scheduler, obscura-worker-agent, obscura-worker, obscura-supervisor, obscura-isolation, obscura-telemetry, obscura-ssrf.

## 2. Runtime entry points

- `obscura` binary (crates/obscura-cli/src/main.rs, 122 KB): CLI `serve`, `run`, MCP, worker commands.
- `obscura-runtime-worker` (obscura-worker): child process IPC worker.
- `obscura-worker-agent`: distributed agent consuming NATS dispatch, driving supervisor/browser.
- `obscura` scheduler binary? scheduler is a library (crates/obscura-scheduler) with `run()` loop; invoked via examples (e2e_verify). No dedicated scheduler binary found — operational gap.

## 3. Dependency graph (workspace)

```
obscura (facade) -> obscura-browser, obscura-net, obscura-dom
obscura-browser -> obscura-dom, obscura-net, obscura-js, obscura-render
obscura-js -> obscura-dom, obscura-net (render feature)
obscura-cdp -> obscura-browser, obscura-js, obscura-dom, obscura-net
obscura-mcp -> obscura-browser, obscura-cdp (feature-gated distributed)
obscura-cli -> obscura, obscura-browser, obscura-cdp, obscura-mcp, obscura-net, obscura-js, obscura-render
obscura-control-plane -> sqlx, async-nats, obscura-ssrf? (no), uuid
obscura-scheduler -> obscura-control-plane, obscura-telemetry
obscura-worker-agent -> obscura-control-plane, obscura-scheduler, obscura-supervisor, obscura-worker
obscura-supervisor -> obscura-worker, obscura-telemetry
obscura-net -> obscura-ssrf
obscura-render -> obscura-ssrf, taffy
```

No circular dependencies observed at crate level. However large modules (runtime.rs 26k lines) create tight internal coupling.

## 4. Distributed execution lifecycle (claimed)

Client → MCP/API → ControlPlane (Postgres) → Outbox → NATS → Scheduler → WorkerAgent → Supervisor → Browser Runtime (child) → Result persistence.

Verified transitions in source:
- submit_job (control plane) writes job + outbox event in same transaction? Check: `submit_job` at line 98. Need verify transactional. (See AUDIT_05/10.)
- OutboxPublisher.publish_pending() reads unpublished outbox and publishes to NATS.
- Scheduler.run() consumes `job.submitted`? Actually consumes `obscura.jobs.>` and triggers schedule_one on job.submitted. Also initial drain.
- schedule_one: claim_pending_job (SKIP LOCKED), select_worker, mark_dispatched, publish dispatch event.
- WorkerAgent consumes dispatch events, acquires lease, runs browser via supervisor, complete/fail job.

## 5. Threading/async model

- Tokio full runtime.
- V8 isolate (deno_core JsRuntime) is !Send; code uses Rc<RefCell> and thread-local-ish state. runtime.rs carefully manages isolate entry. Risk: moving runtime objects across tasks (see Phase 3).
- Process supervision uses tokio::process::Child with piped stdio and dedicated reader/writer tasks.

## 6. Global state

- `env_allows_private_network()` reads env var.
- `OBSCURA_SERVICE_BOOTSTRAP_SECRET` env-based auth.
- Metrics registry global (obscura_telemetry::global_metrics).
- Storage dir via env `OBSCURA_STORAGE_DIR`.

## 7. Security boundaries

- Browser/MCP run in-process (no sandbox beyond OS).
- SSRF guard in obscura-ssrf shared by net + render.
- Distributed auth via bootstrap secret + credential hashing (SHA-256? check).
- Tenant isolation via tenant_id on all job queries (mostly).

## 8. Incomplete/feature-gated areas

- Distributed MCP tools feature-gated (`distributed` feature).
- Render feature-gated (`render`).
- Scheduler has no binary entry point (library only).
- Docker image builds only obscura + obscura-worker (not distributed components).
- Compose lacks Postgres/worker-agent/scheduler.

## 9. Architecture claims vs reality

Many V0.x reports claim production readiness; audit finds test failures, compile (clippy) errors, missing deploy artifacts, and lease/fencing dual-token complexity. See final report.
