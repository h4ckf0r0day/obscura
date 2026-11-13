# AUDIT_09 — Infrastructure and DevOps

## 1. Docker

- Dockerfile: multi-stage Rust build, distroless/cc nonroot runtime. Builds only `obscura` and `obscura-worker` with `--features render`. Does not build worker-agent, scheduler, control-plane binary, MCP HTTP server. Distributed mode not shippable via image.
- Base image not pinned to digest (intentional per comment).
- Health check in compose: `kill -0 1` (process alive only), not application health.
- No resource limits in Dockerfile (compose has memory 2G, cpus 2).

## 2. Compose

- Services: obscura-supervisor, nats, prometheus. Missing: postgres, worker-agent, scheduler, control plane.
- Prometheus config mounted.
- NATS health via wget.
- Obscura health via kill -0 (weak).
- Hardcoded weak CDP token in compose.

## 3. Kubernetes

- `deploy/kubernetes/obscura-cluster.yaml` present; not deeply audited.

## 4. CI

- `scripts/ci/` Python tests (pr_policy, perf_smoke, compare_obstacle, vendor_policy, test_repair_v8_cache). Need run? Not executed.
- GitHub workflows not inspected in detail (`.github`).

## 5. Observability

- Custom metrics registry with Prometheus text exposition on /metrics, /health.
- No auth on /metrics (low risk internal).
- Relaxed atomic ordering (acceptable for metrics).
- Health endpoint reports sessions/workers but no readiness distinction.
- Tracing via tracing-subscriber env-filter.

## 6. Gaps

- Distributed components undeployable.
- Weak health checks.
- No alerting/runbooks.
- Metrics server binds? Need check bind address (maybe 0.0.0.0) — could expose. Not verified.
