# AUDIT VALIDATION 10 — ROOT CAUSE GRAPH

## Confirmed root causes
1. **RC-1 Frame lifecycle incomplete** (F-01): static/shadow iframe realms not queued → 3 test failures; JS execution in child frames absent.
2. **RC-2 Flex/table/inline layout edge cases** (F-02): anonymous flex items not wrapped; table track resolution; inline continuation → 18 render failures.
3. **RC-3 Scheduler stuck job (no worker)** (F-03): mitigated by claim EXISTS check + release_job_to_pending.
4. **RC-4 Tenant claim scope** (F-04): global claim by design but must preserve worker tenant matching; mitigated.
5. **RC-5 Service identity default secret** (security): default "testsecret" fail-open.
6. **RC-6 File:// redirect validation inconsistency** (F-08): validate_url permits file:// without opt-in.
7. **RC-7 No OS sandbox** (F-07): supervision only.
8. **RC-8 No scheduler binary / compose gaps** (F-06): distributed not deployable.
9. **RC-9 Reconcile_stuck_jobs not in run loop** (F-09 residual): scheduled stuck jobs not reclaimed periodically.
10. **RC-10 Clippy import_map prefix selection** (F-11): first-match instead of longest-match.
11. **RC-11 Partial reads ignored** (F-12): read() result discarded.
12. **RC-12 Idempotency race returns error** (F-13 residual): not duplicate, but UX.
13. **RC-13 Completion missing attempt_id/status guard** (F-14 residual).

## Dependency graph
- RC-3 + RC-4 + RC-9 + RC-12 + RC-13 share distributed control plane root (job state machine).
- RC-1 alone drives F-01 and related messaging tests (already passing for dynamic frames).
- RC-2 alone drives 18 render failures.
- RC-6 + RC-7 share security boundary (network/file access).

## Remediation order
P0: RC-5 (service secret fail-closed), RC-6 (validate_url file://), RC-4/RC-3 verification.
P1: RC-1 (frames), RC-2 (render), RC-9 (reconcile_stuck in loop), RC-13 (completion guards).
P2: RC-10, RC-11 (clippy), RC-12 (idempotency race).
P3: RC-8 (deployment binaries/compose), RC-7 (sandbox or scope claims).
P4: formatting (96 files).
