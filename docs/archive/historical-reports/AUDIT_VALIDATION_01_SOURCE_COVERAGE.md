# AUDIT VALIDATION 01 — SOURCE COVERAGE

## Baseline
- Repository: `/Users/kumarmangalam/Desktop/Devops projects/obscura-next`
- Branch: `obscura-next/phase-5.8-recovery-implementation`
- HEAD: `385174660030ae006361515a1da2c96004528707`
- Working tree: modified (uncommitted Phase 5.8 recovery changes) + untracked audit files/examples
- Git remotes: origin (itskum47/obscura), upstream (h4ckf0r0day/obscura)

## First-party inventory (excluding vendor/, target/, .git)
| Language | Files | Lines |
|---|---:|---:|
| Rust (.rs) | 192 | 195,671 |
| TypeScript (.ts) | 138 | (not fully counted) |
| JavaScript (.js) | 19 | |
| Python (.py) | 27 | |
| SQL (.sql) | 5 | |
| Shell (.sh) | 15 | |
| TOML (.toml) | 26 | |
| YAML/YML | 11 | |
| Markdown (.md) | 340 | |
| Dockerfile | 1 | |

Rust LOC breakdown (approx): crates/ 192 files. Largest: obscura-js runtime.rs (~25k), obscura-render dom.rs (~21k), obscura-browser page.rs (~11k), obscura-net client.rs (~3k), control-plane lib.rs (~1.7k).

## Inspection status
- Rust crates: all 19 workspace crates inspected at symbol level; key files read in detail: control-plane/src/lib.rs (full), scheduler/src/lib.rs (full), net/src/client.rs (validate_url + redirect), isolation/src/lib.rs, supervisor/src/lib.rs (spawn), browser/src/page.rs (navigation/frames), worker-agent/src/main.rs.
- SQL: migrations inspected via DB schema (\d jobs) and source SQL strings.
- Docker/Compose: Dockerfile, Dockerfile.worker, docker-compose.yml inspected.
- Tests: child_frame_scripts, render, control-plane, scheduler executed.

## Not fully inspected
- Full line-by-line of obscura-js/runtime.rs (25k) — sampled sections.
- Full obscura-render/dom.rs (21k) — sampled.
- TypeScript SDK (sdk/) — not audited line-by-line.
- Kubernetes manifests (deploy/kubernetes) — not deeply audited.
- artifacts/, obstacle-course/, render-repros/, tools/, skills/ — not fully inventoried.
- Vendor code excluded (third-party).

## Coverage estimate
- First-party Rust files inspected (symbol-level): 192/192 = 100% of files, but line-level deep inspection ~35-45% of LOC.
- SQL: 5/5 files inspected (schema).
- Infra: Dockerfiles + compose inspected.
- TS/JS/Python: not systematically audited.

## Third-party/vendor
- vendor/taffy, vendor/cosmic-text, vendor/wreq-util excluded from first-party audit. Dependency/security implications noted separately (cargo-deny unavailable).

## Audit confidence limitation
Reported coverage cannot claim 100% line-level inspection. Large files sampled; exact line references provided only where directly read.
