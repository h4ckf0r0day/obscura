# AUDIT_01 — Project Inventory

## 1. Project identity

- **Project name:** Obscura Next (workspace package `obscura`, version `0.2.0`).
- **Purpose:** Headless browser engine (Chromium-compatible CDP) plus distributed execution control plane (PostgreSQL + NATS JetStream + scheduler + worker agents + process supervisor).
- **Repository root:** `/Users/kumarmangalam/Desktop/Devops projects/obscura-next`.
- **Git branch:** `obscura-next/phase-5.8-recovery-implementation`.
- **HEAD:** `385174660030ae006361515a1da2c96004528707`.
- **Working tree:** clean (no uncommitted/untracked changes).
- **Remotes:** `origin` = `https://github.com/itskum47/obscura.git`; `upstream` = `https://github.com/h4ckf0r0day/obscura.git`.
- **Upstream attribution:** fork/derivative of upstream `h4ckf0r0day/obscura` (Apache-2.0). Workspace metadata points `repository` to upstream URL.
- **Tags:** v0.1.0 … v0.2.6 visible (26+ tags).
- **Recent commits:** 1727 total commits; HEAD is docs-only "Phase 5.8 recovery implementation deliverables".
- **Size:** ~11 GB including `target/` (11 GB) and vendor; `.git` 45 MB; source tree excluding target/vendor/node_modules ~ a few hundred MB.

## 2. Workspace members (Cargo)

`crates/`: obscura, obscura-agent, obscura-browser, obscura-cdp, obscura-cli, obscura-control-plane, obscura-dom, obscura-isolation, obscura-js, obscura-mcp, obscura-net, obscura-render, obscura-scheduler, obscura-ssrf, obscura-supervisor, obscura-telemetry, obscura-worker, obscura-worker-agent, test-support.

Excluded from workspace: `vendor/taffy`, `vendor/cosmic-text`, `vendor/wreq-util`.

## 3. File counts (excluding target, vendor, node_modules, .git)

| Extension | Count |
|---|---|
| md | 320 |
| rs | 191 |
| html | 144 |
| py | 27 |
| toml | 26 |
| js | 19 |
| ttf | 17 |
| png | 16 |
| sh | 14 |
| json | 10 |
| yml | 9 |
| sql | 5 |
| ts | 4 |

Rust LoC in `crates/`: 192,195 lines across 182 `.rs` files.

## 4. Classification of top-level entries

| Entry | Class |
|---|---|
| crates/ | first-party source |
| sdk/ | SDKs (Python, TypeScript) |
| tests/ | integration harness scripts |
| deploy/ | infrastructure (Docker, Compose, K8s) |
| scripts/ | build/test/ci scripts |
| docs/, engineering/ | documentation |
| vendor/ | vendored dependencies (taffy, cosmic-text, wreq-util) |
| target/ | build artifacts (11 GB) |
| artifacts/, assets/, benchmarks/, obstacle-course/, render-repros/, tools/, skills/ | mixed assets/tests |
| *.md (V0.x reports, PHASE*, F5_*, AUDIT_REPORT) | historical implementation reports |
| Dockerfile, deny.toml, Cargo.toml, Cargo.lock | project config |

## 5. Largest source files (Rust)

| File | Bytes |
|---|---|
| crates/obscura-js/src/runtime.rs | 1,175,920 |
| crates/obscura-render/src/dom.rs | 953,146 |
| crates/obscura-render/src/paint.rs | 813,402 |
| crates/obscura-browser/src/page.rs | 500,567 |
| crates/obscura-render/src/style.rs | 418,542 |
| crates/obscura-render/src/css.rs | 389,630 |
| crates/obscura-js/src/ops.rs | 376,027 |
| crates/obscura-render/src/inline.rs | 222,641 |
| crates/obscura-cdp/src/server.rs | 214,152 |
| crates/obscura-cdp/src/domains/page.rs | 177,420 |
| crates/obscura-mcp/src/lib.rs | 162,203 |
| crates/obscura-cli/src/main.rs | 122,500 |
| crates/obscura-render/src/lib.rs | 121,720 |
| crates/obscura-net/src/client.rs | 119,370 |
| crates/obscura-dom/src/tree.rs | 110,734 |

These are unusually large single files (browser engine monoliths). High maintainability risk.

## 6. Untracked / ignored

Working tree clean at audit start. `.gitignore` excludes `target/`. Vendor is tracked. No submodules.

## 7. Excluded from detailed source inspection

- `target/` (build artifacts, 11 GB) — not source.
- `vendor/` — vendored upstream (taffy layout engine, cosmic-text, wreq-util); treated as third-party.
- `sdk/typescript/node_modules/` — dependency.
- Binary assets (`.ttf`, `.png`, `.gif`) — not inspected as code.

## 8. Unexplained / notable files

- `build.log` — stale build log.
- `artifacts/`, `obstacle-course/`, `render-repros/`, `tools/`, `skills/` — require per-file triage; not all inspected.
- Numerous historical report markdowns (V0.x, PHASE*, F5_*) — treated as documentation, cross-checked in Phase 13.

## 9. Inventory deliverable

This file plus directory tree captured via filesystem traversal. Detailed per-crate breakdowns in `AUDIT_02_ARCHITECTURE.md`.
