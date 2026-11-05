# Test Evidence — v0.3.1 Independent Audit

| Test/Command | Area | Result | Executed During This Audit? | Historical Source | Evidence |
|---|---|---|---|---|---|
| cargo check --workspace | Compilation | PASS | Yes | Current run | Terminal output |
| cargo test -p obscura-control-plane | PG integration | PASS (3/3) | Yes (prior) | Terminal output | Historical + current |
| cargo test -p obscura-scheduler | PG+NATS integration | PASS (3/3) | Yes (prior) | Terminal output | Historical + current |
| cargo build -p obscura-worker-agent | Build | PASS | Yes | Terminal output | Current |
| Worker agent E2E | Distributed execution | PASS (1) | Yes | Terminal output | Current |
| cargo fmt --all -- --check | Formatting | NOT RUN | No | — | Not executed |
| cargo clippy --workspace | Linting | NOT RUN | No | — | Not executed |
| Browser obstacle course | Browser | HISTORICAL PASS | No | v0.2.0 report | 33/33 (historical) |
| Security tests | Security | HISTORICAL PASS | No | v0.2.0 report | 5/5 (historical) |
| Python SDK E2E | SDK | HISTORICAL PASS | No | v0.2.0 report | 3/3 (historical) |
| TypeScript SDK E2E | SDK | HISTORICAL PASS | No | v0.2.0 report | 1/1 (historical) |
| Docker smoke test | Deployment | HISTORICAL PASS | No | v0.2.0 report | Passed (historical) |
| Fencing adversarial tests | Security | BLOCKED | No | — | No test exists |
| Authorization tests | Security | BLOCKED | No | — | No auth to test |
| Crash recovery tests | Reliability | BLOCKED | No | — | No crash-injection tests |
| SSRF comprehensive tests | Security | BLOCKED | No | — | No comprehensive suite |
| Multi-machine tests | Deployment | BLOCKED | No | — | Single host only |
