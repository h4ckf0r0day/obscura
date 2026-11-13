# AUDIT_12 — Testing and Quality

## 1. Test inventory

- Unit/integration tests inline in crates (#[cfg(test)]) + tests/ directories.
- 58 test files in crates/obscura-cdp/tests etc.
- SDK tests (Python, TS).

## 2. Baseline run (with env vars set)

| Crate | Result |
|---|---|
| obscura-dom | 92+2 pass |
| obscura-ssrf | 0 tests (no unit tests? module has tests?) |
| obscura-net | 132 pass, 1 fail |
| obscura-browser | 155 pass, 1 fail |
| obscura-cdp | (timed out; not fully captured) |
| obscura-js | 673 pass, 1 fail |
| obscura-mcp | 44+ pass |
| obscura-render | 297 pass, 18 fail, 1 ignored |
| obscura-cli | (timed out) |
| obscura | 0+2 pass, 3 fail (child_frame_scripts) |
| obscura-telemetry | 4 pass |
| obscura-isolation | 4 pass |
| obscura-worker | 4 pass |
| obscura-supervisor | 12 pass |
| obscura-agent | 2 pass |
| obscura-control-plane | 9 pass, 6 fail (env-dependent, test-isolation) |
| obscura-scheduler | 0 pass, 3 fail (env-dependent) |
| obscura-worker-agent | 0 tests |

## 3. Failures (real vs environmental)

- obscura-render 18: layout edge cases (need triage).
- obscura child_frame_scripts 3: shadow/static iframe script execution.
- obscura-net 1, obscura-browser 1, obscura-js 1: need triage.
- control-plane 6: 2 capacity (state transition), 2 f5 adversarial (parallel claim), 1 security (duplicate tenant name), 1 ssrf (duplicate tenant name).
- scheduler 3: require DB (panicked at line 58/248 etc. — likely DB connection/secret).

## 4. Misleading/weak tests

- Tests use fixed tenant names → duplicate key failures on re-run (test isolation defect).
- `test_nats_redelivery_on_nak` doesn't compile (`msg.nak()` missing) → test code stale vs async-nats API.
- Many distributed tests require external services (Postgres, NATS) and bootstrap secret; not run in default CI maybe.
- Tests that skip when worker binary missing (supervisor) — silently pass.
- Tests manually invoke internal functions rather than production lifecycle in some cases.

## 5. Missing coverage

- SSRF crate has no unit tests? (0 tests reported) — but net has resolver tests.
- Failure/restart/recovery scenarios partially tested.
- Concurrency/adversarial tests exist but flaky under parallelism.
- No chaos/fault injection in CI.

## 6. F5 reliability requirements

Previous F5 reports claim closure; audit finds redelivery test doesn't compile and distributed tests fail under parallelism. F5 not fully verified by this audit.
