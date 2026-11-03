# Observability

## Metrics

`MetricsServer` exposes `GET /metrics` (Prometheus text) and `GET /health` over TCP.

Bind address configurable (default `0.0.0.0:9100`).

## Required metrics

obscura_worker_processes_total, obscura_worker_processes_healthy, obscura_worker_processes_failed,
obscura_worker_restarts_total, obscura_worker_active_requests, obscura_sessions_active,
obscura_sessions_total, obscura_sessions_failed_total, obscura_worker_requests_total,
obscura_worker_request_errors_total, obscura_worker_request_duration_seconds,
obscura_navigation_duration_seconds, obscura_eval_duration_seconds,
obscura_security_blocks_total, obscura_watchdog_terminations_total.

## Prometheus

Scrape target: `obscura-supervisor:9100`, path `/metrics`.
