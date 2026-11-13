# OBSCURA NEXT — COMPLETE ENGINEERING AUDIT

Independent forensic audit. No source modifications made. Working tree clean verified at start and end.

## A. Executive summary

- **Overall health:** Mixed. Browser engine is feature-rich but has open test failures (render 18, child frames 3, net 1, js 1, browser 1). Distributed control plane has real implementation but test/operational gaps.
- **Architecture maturity:** 3/5. Two systems (browser + distributed) in one repo; distributed components not deployable via provided Docker/Compose.
- **Implementation completeness:** Browser engine ~80% (many web platform features, some stubs). Distributed ~70% (core flows implemented, recovery partially).
- **Security posture:** 3/5. Strong SSRF policy; auth via bootstrap secret; MCP HTTP auth good; but isolation is supervision not sandbox; weak compose token; file:// redirect risk.
- **Reliability posture:** 2.5/5. Fencing exists; recovery code exists; but tests flaky under parallelism, redelivery test doesn't compile, scheduler leaves jobs stuck on no worker.
- **Operational maturity:** 2/5. Weak health checks, no DLQ, no runbooks, distributed components not in image.
- **Test quality:** 2.5/5. Large test count but failures, environmental dependencies, fixed test data collisions, stale async-nats API usage.
- **Production readiness:** Not ready for multi-tenant/multi-host production. Suitable for local dev and controlled single-host beta with caveats.

## B. Module scorecard (0-5)

| Module | Correctness | Security | Reliability | Maintainability | Performance | Test | Docs |
|---|---|---|---|---|---|---|---|
| obscura-dom | 4 | 4 | 4 | 3 | 3 | 4 | 3 |
| obscura-ssrf | 4 | 4 | 4 | 4 | 4 | 2 | 4 |
| obscura-net | 3.5 | 4 | 3 | 3 | 3 | 3 | 3 |
| obscura-browser | 3 | 3 | 3 | 2 | 3 | 3 | 3 |
| obscura-cdp | 3.5 | 3 | 3 | 2 | 3 | 3 | 3 |
| obscura-js | 3 | 3 | 3 | 1.5 | 3 | 3.5 | 2 |
| obscura-render | 2.5 | 4 | 3 | 1.5 | 2.5 | 2.5 | 2 |
| obscura-mcp | 3.5 | 4 | 3 | 2 | 3 | 4 | 3 |
| obscura-cli | 3 | 3 | 3 | 2 | 3 | 2 | 3 |
| obscura (facade) | 3 | 3 | 3 | 3 | 3 | 2 | 3 |
| obscura-telemetry | 4 | 2 | 3 | 4 | 4 | 3 | 3 |
| obscura-isolation | 2 | 2 | 2 | 3 | 3 | 2 | 2 |
| obscura-worker | 3 | 3 | 3 | 3 | 3 | 3 | 3 |
| obscura-supervisor | 4 | 3 | 4 | 3 | 3 | 3 | 3 |
| obscura-agent | 3 | 3 | 3 | 3 | 3 | 2 | 3 |
| obscura-control-plane | 3 | 3.5 | 3 | 2 | 3 | 2.5 | 3 |
| obscura-scheduler | 2.5 | 3 | 2.5 | 3 | 3 | 2 | 3 |
| obscura-worker-agent | 3 | 3 | 3 | 2 | 3 | 1 | 3 |

## C. Critical/high findings

| ID | Sev | Component | Path | Symbol | Summary |
|---|---|---|---|---|---|
| F-01 | High | browser/frames | crates/obscura/tests/child_frame_scripts.rs (tests) | a_static_child_frame_runs_its_own_script | Static iframe scripts don't run (3 failures). |
| F-02 | High | render | crates/obscura-render (layout_test) | 18 failures | Layout edge cases broken. |
| F-03 | High | scheduler | crates/obscura-scheduler/src/lib.rs | schedule_one | Job left in 'scheduled' when no worker; stuck. |
| F-04 | High | control-plane | crates/obscura-control-plane/src/lib.rs | claim_pending_job | Global claim across tenants; parallel test interference; potential cross-tenant scheduling. |
| F-05 | High | distributed tests | crates/obscura-control-plane/src/lib.rs | test_nats_redelivery_on_nak | Doesn't compile (msg.nak missing). |
| F-06 | High | deploy | Dockerfile, deploy/compose | — | Distributed components not built/deployed. |
| F-07 | High | isolation | crates/obscura-isolation | — | Supervision not OS sandbox. |
| F-08 | Medium | net | crates/obscura-net/src/client.rs | validate_url | file:// redirect may allow local file read. |
| F-09 | Medium | scheduler | run() | detached reconciliation task | Not supervised. |
| F-10 | Medium | worker-agent | main.rs | _hb_handle | Heartbeat task detached; unused handle. |
| F-11 | Medium | js | import_map.rs:235 | never_loop clippy | Prefix resolution may be buggy. |
| F-12 | Medium | js | runtime.rs read().unwrap() ×8 | unused_io_amount | Partial reads ignored. |
| F-13 | Medium | db | migrations | idempotency key | No unique constraint verified → race duplicates. |
| F-14 | Medium | control-plane | complete_job | no attempt_id/status check | Replay possible. |
| F-15 | Medium | telemetry | metrics server | no auth | Internal exposure. |
| F-16 | Low | compose | docker-compose.yml | weak token | Example token weak. |
| F-17 | Low | browser | page.rs | dead code ×9 | Maintainability. |
| F-18 | Low | tests | fixed tenant names | collisions | Test isolation. |

## D. Medium/low findings

See Phase reports 03-17. Includes formatting drift, dead code, detached tasks, missing DLQ, weak health checks, branch sprawl, stale docs.

## E. Missing functionality

- Fully implemented: DOM tree, selectors, CSS parsing, layout (mostly), V8 runtime, net, SSRF, CDP domains, MCP stdio/HTTP, control plane CRUD, leases, outbox, scheduler loop, supervisor, worker agent.
- Partially implemented: child frames, fonts, WebSocket (unverified), render edge cases, distributed recovery.
- Stubbed: navigator stubs, some window methods.
- Unwired: scheduler binary, distributed components in Docker.
- Broken: render 18 tests, child frames 3, redelivery test compile.
- Not implemented: OS sandbox, DLQ, readiness/liveness split, protocol version negotiation.
- Implemented but not verified: distributed E2E in CI.

## F. Test coverage and confidence

- Source inspected: yes (all crates).
- Static analysis: cargo check pass; clippy fail (9 errors); fmt fail.
- Unit tested: many.
- Integration tested: control-plane/scheduler require services.
- E2E tested: manual harness only.
- Runtime verified: partial (MCP distributed E2E per commit).
- Blocked: cargo-deny/audit unavailable; cdp/cli tests not fully captured due timeout.

## G. Production-readiness verdicts

- Local development: Yes.
- Internal testing: Yes (with env setup).
- Controlled beta (single host, single tenant): Conditional yes.
- Single-host deployment: Conditional (distributed not shippable).
- Multi-host deployment: No.
- Multi-tenant deployment: No (global claim, tenant isolation not fully verified).
- Production workloads: No.

## H. Prioritized remediation roadmap

**P0 (critical security/data integrity):**
- F-04 global claim tenant scoping + unique idempotency constraint (F-13).
- F-08 file:// redirect validation.
- F-07 document isolation reality; add OS sandbox or rename.
- F-06 ship distributed components or scope claims.

**P1 (major reliability/correctness):**
- F-01, F-02 fix frame/render failures.
- F-03 scheduler stuck job handling.
- F-05 redelivery test compile fix.
- F-14 complete_job attempt_id/status checks.
- F-09/F-10 supervised tasks + DLQ.

**P2 (important engineering):**
- F-11/F-12 clippy errors.
- F-18 test isolation (unique names).
- F-15 metrics auth (optional).
- Protocol version checks.

**P3 (maintainability):**
- Split monolithic files.
- Remove dead code.
- Update stale docs.

**P4 (minor):**
- Formatting.
- Compose token example.
- Health check improvements.

Each item requires verification tests listed in phase reports.

## I. Audit limitations

- Did not inspect every line of 192k LOC; sampled and grepped. Large files (runtime.rs, dom.rs) not fully read line-by-line.
- Binary/build artifacts excluded.
- cargo-deny/cargo-audit unavailable.
- cdp/cli test runs timed out; results incomplete.
- External services (Postgres/NATS) present but tests need env vars; some failures environmental.
- Git history not scanned for secrets.
- Kubernetes manifests not deeply audited.
- SDK E2E not run.
- Vendor code not audited (third-party).
- `artifacts/`, `obstacle-course/`, `render-repros/`, `tools/`, `skills/` not fully inventoried.
- Assumptions: upstream is h4ckf0r0day/obscura; repo is derivative.
