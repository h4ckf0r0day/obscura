# AUDIT VALIDATION 06 — DEPLOYMENT

## Dockerfile (root)
- Builds only `obscura` and `obscura-worker` binaries. Does NOT build scheduler, worker-agent, control-plane (library). No Postgres/NATS in image.

## Dockerfile.worker
- Builds obscura-cli and obscura-runtime-worker. Copies `obscura` and `obscura-runtime-worker`. No scheduler/worker-agent/control-plane.

## docker-compose.yml
- Services: obscura-supervisor, nats, prometheus. No postgres, no scheduler, no worker-agent, no control-plane.
- Health check: `kill -0 1` (process alive only, not readiness).
- NATS has healthcheck; postgres absent.

## Capability assessment
| Capability | Status |
|---|---|
| Source implementation exists | Yes (scheduler lib, worker-agent bin, control-plane lib) |
| Binary builds | Partial: worker-agent builds; scheduler/control-plane are libs (no bin) |
| Container includes binary | No for scheduler/worker-agent/control-plane |
| Container starts successfully | Browser supervisor yes; distributed no |
| Service becomes ready | Browser yes; distributed not wired |
| End-to-end distributed job completes | Only manual harness (tests/integration/harness/run_adversarial_multiproc.sh) |

## Gaps
- No scheduler executable; no control-plane service binary.
- Compose lacks Postgres and worker-agent and scheduler.
- Health checks are liveness only (kill -0), no readiness/endpoint.
- No DLQ, no runbooks.
- Weak example token in compose (low severity).

## Status: CONFIRMED (F-06). Deployment of distributed architecture not supported by provided manifests.
