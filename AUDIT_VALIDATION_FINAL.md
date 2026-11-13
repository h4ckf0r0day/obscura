# AUDIT VALIDATION FINAL — OBSCURA NEXT

## A. Confirmed defects
- F-01 Child frames (3 tests) — CONFIRMED.
- F-02 Rendering (18 tests) — CONFIRMED.
- F-06 Deployment distributed — CONFIRMED.
- F-07 Process isolation (supervision not sandbox) — CONFIRMED.
- F-08 validate_url file:// inconsistency — PARTIALLY CONFIRMED.
- F-11 Clippy import_map — CONFIRMED.
- F-12 Clippy unused_io_amount (8) — CONFIRMED.
- Formatting (96 files) — CONFIRMED.

## B. False positives / already mitigated
- F-13 Duplicate jobs: FALSE POSITIVE (unique constraint tenant-scoped); race returns error not duplicate.
- F-05 Redelivery test compile: ALREADY FIXED (tests compile & pass).
- F-03 Stuck jobs: largely fixed in working tree (claim guard + release).
- F-04 Global claim: by design with tenant matching; mitigated.
- F-09 Reconciliation wired: ALREADY FIXED (run() spawns loop) but reconcile_stuck_jobs not called.

## C. Inconclusive
- F-14 Completion: fencing protects staleness, but attempt_id/status guards missing; needs targeted test.
- Network/JS/Browser single failures: not individually reproduced; require separate runs.

## D. Security risks (prioritized)
1. Service secret default "testsecret" (fail-open) — High.
2. No OS sandbox (F-07) — High for untrusted content.
3. validate_url file:// redirect inconsistency (F-08) — Medium.
4. Metrics unauth (F-15) — Low/Medium.
5. Weak compose token (F-16) — Low.

## E. Data integrity risks
- Idempotency race returns error not existing job (UX).
- complete_job lacks attempt_id/status guard (replay risk low due to monotonic tokens).
- Recovery UPDATEs not serialized (concurrent reconcile) — low risk due to status filters.

## F. Reliability risks
- No scheduler binary → distributed not runnable as service.
- reconcile_stuck_jobs not in run loop → scheduled stuck jobs may linger (though claim guard prevents new stuck).
- macOS lacks PDEATHSIG → descendant leak on agent death.
- Heartbeat/renewal tasks detached but functional.

## G. Browser correctness risks
- Child frames (static/shadow) not loaded → F-01.
- Layout edge cases → F-02.
- Subresource security gates present but validate_url inconsistency.

## H. Deployment gaps
- No scheduler/worker-agent/control-plane in images/compose.
- No Postgres in compose.
- Health checks liveness only.

## I. Remediation order
See AUDIT_VALIDATION_10_ROOT_CAUSE_GRAPH.md (P0→P4).

## J. Audit confidence
- Rust files inspected (symbol-level): 192/192 (100% files), line-level deep ~40%.
- SQL: schema inspected.
- Infra: Dockerfiles + compose inspected.
- TS/JS/Python: not systematically audited.
- Tests run: child_frame (8/3), render (297/18), control-plane (18/0), scheduler (4/0), clippy (9 errors), fmt (96 files).
- Services available: Postgres 5432, NATS 4222.
- Not run: CDP/CLI tests, SDK E2E, cargo-deny/audit, k8s manifests.
- Coverage not 100% line-level; large files sampled.

## Baseline preservation
No source modifications made during this investigation. Working tree state preserved (uncommitted changes remain untouched). Final git status should match initial (modified + untracked as recorded).
