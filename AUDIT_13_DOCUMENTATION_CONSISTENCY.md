# AUDIT_13 — Documentation and Implementation Consistency

## 1. Findings

- README/ARCHITECTURE claim distributed production readiness; actual deploy artifacts lack distributed components.
- V0.3.x/F5 reports claim test counts (e.g., "all passing") that conflict with current failures (render 18, child frames 3).
- Obstacle course and benchmark docs may be stale.
- `AUDIT_REPORT.md`, `V0.3.0_ULTIMATE_ENGINEERING_AUDIT_REPORT.md` etc. are historical; not reconciled with current code.
- Environment variable names: `OBSCURA_SERVICE_BOOTSTRAP_SECRET`, `OBSCURA_MCP_TOKEN`, `OBSCURA_CDP_TOKEN`, `OBSCURA_ALLOW_PRIVATE_NETWORK`, `OBSCURA_STORAGE_DIR`, `OBSCURA_WORKER_BIN` — consistent in code.
- Ports: CDP default 9222, metrics 9100, MCP HTTP? check. Postgres 5432, NATS 4222.

## 2. Misleading claims

- "process isolation" named but implementation is supervision.
- Many phase reports declare closure; audit shows open defects.

## 3. Recommendation

- Add a single source-of-truth status document; mark features implemented/partial/stubbed.
- Update README deploy section to reflect distributed components missing from Docker.
