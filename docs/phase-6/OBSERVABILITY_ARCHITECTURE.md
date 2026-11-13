# Phase 6 Observability Architecture

- Metrics server in `crates/obscura-telemetry` exposes /metrics and /health.
- `/metrics` now supports optional Bearer auth via `OBSCURA_METRICS_TOKEN`.
- Health endpoint reports uptime, active sessions, worker count.
- Required improvements: readiness vs liveness, dependency health, auth by default in production.
