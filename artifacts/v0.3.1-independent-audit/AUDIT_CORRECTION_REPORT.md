# Audit Correction Report — v0.3.1

## What Was Inconsistent in the Previous Audit
- Executive summary reported 13 findings; master register had 15. Severity counts didn't match.
- Finding IDs used inconsistently between top-five section and detailed register.
- Some findings were duplicated or split across sections.

## What Was Corrected
- Canonical finding register created with 15 unique IDs (F1-F15).
- Severity totals reconciled: P0=0, P1=5, P2=5, P3=3, P4=2. Total=15.
- Top-five section now references correct IDs (F1-F5).
- Claims vs. Reality matrix updated with consistent verdicts.
- Test evidence matrix created with historical vs. current distinction.

## Claims Downgraded
- F2 (Authentication): P0 → P1 (no network API exists; not remotely exploitable today).
- F1 (Worker agent): P0 → P1 (missing feature, not data-loss in current code).
- F6 (NatsConsumer): P1 → P2 (functional but inefficient).
- F7 (Capacity): P1 → P2 (can drift but not confirmed corruption).
- F8 (Heartbeat): P1 → P2 (filter exists, enforcement incomplete).
- F9 (Tenant isolation): P1 → P2 (query-level only, no auth surface).
- F10 (SSRF): P2 → P3 (literal guard exists; comprehensive suite missing).
- F11 (Metrics): P2 → P3 (registered but not all updated).
- F12 (Docker health): P2 → P3 (process liveness only).
- F13 (Dedup window): P2 → P3 (headers set, window not configured).

## Findings Consolidated
- F1 (worker consumer) and F5 (fencing not wired) kept separate (different root causes).
- F2 (auth) and F3 (authorization) kept separate (different concerns).
- F14 and F15 kept as separate P4 findings.

## What Remains Uncertain
- Whether NatsConsumer stream recreation causes actual message loss (not tested under load).
- Whether capacity counters actually drift in practice (no failure test).
- Whether scheduler reconciliation would recover stuck jobs (no crash test).
- Whether stale worker could overwrite result (no adversarial test, though token validation exists).

## Repository Changes
No source code, migrations, tests, or configuration were modified. Only audit documentation files were created/updated.
