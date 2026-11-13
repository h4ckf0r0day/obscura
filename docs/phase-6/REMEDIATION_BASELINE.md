# Phase 6 Remediation Baseline

## Repository verification

- Branch: `release/f5-reliability-baseline`
- HEAD: `d5a41cddb4088b88275daa838dcc53408f05bec4`
- Remotes: origin `itskum47/obscura`, upstream `h4ckf0r0day/obscura`
- Working tree: many modified tracked files (uncommitted) + 36 untracked audit reports; preserved.

## Finding classification (from Phase 5 audits)

| ID | Finding | Classification | Notes |
|---|---|---|---|
| F-01 | Child frames (static/shadow) not loaded (3 tests) | CONFIRMED | Reproduced per validation report. |
| F-02 | Rendering failures (18 tests) | CONFIRMED | Layout edge cases. |
| F-03 | Stuck jobs | ALREADY FIXED | Claim guard + release in working tree. |
| F-04 | Global claim | PARTIALLY CONFIRMED | By design with tenant matching; mitigated. |
| F-05 | Redelivery test compile | ALREADY FIXED | Tests compile & pass. |
| F-06 | Deployment distributed | CONFIRMED | No scheduler/worker-agent/control-plane in images. |
| F-07 | Process isolation (supervision not sandbox) | CONFIRMED | In-process guard only. |
| F-08 | validate_url file:// inconsistency | PARTIALLY CONFIRMED | Redirect path only. |
| F-09 | Reconciliation wired | ALREADY FIXED | run() spawns loop; reconcile_stuck_jobs called. |
| F-11 | Clippy import_map | CONFIRMED | |
| F-12 | Clippy unused_io_amount (8) | CONFIRMED | |
| F-13 | Duplicate jobs | FALSE POSITIVE | Unique constraint tenant-scoped. |
| F-14 | Completion fencing guards | INCONCLUSIVE | attempt_id/status guards missing. |
| F-15 | Metrics unauth | CONFIRMED | Low/Medium. |
| F-16 | Weak compose token | CONFIRMED | Low. |
| SEC-1 | Service secret default "testsecret" | ALREADY FIXED | Fails closed in current tree. |
| SEC-2 | No OS sandbox | CONFIRMED | High for untrusted content. |
| NET-1 | SSRF crate no tests | CONFIRMED | 0 tests. |
| DEP-1 | No scheduler binary | CONFIRMED | Example only. |
| HYG-1 | Duplicate obstacle-course | CONFIRMED | Identical copies. |
| HYG-2 | Vendor Cargo.toml.orig | CONFIRMED | Leftover packaging files. |
| VER-1 | Workspace version 0.2.0 vs v0.3.x | CONFIRMED | Needs alignment. |

## Already-resolved (preserve)
- Service bootstrap secret fails closed.
- Reconciliation loop wired into scheduler run().
- Child claim tenant scoping.
- F5 process containment baseline.

## Unverified / blocked
- Clippy `-D warnings` full pass (not yet run to completion; many lints).
- cargo-deny/cargo-audit (tools not installed).
- SDK E2E (no service deployment).
- Child-frame and render test counts (not re-run in this environment).

## Baseline rule
No source modifications were made for this classification; working tree preserved.
