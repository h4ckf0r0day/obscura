# AUDIT VALIDATION 09 — TEST FAILURES

## Commands run
| Command | Exit | Passed | Failed | Skipped | Notes |
|---|---:|---:|---:|---:|---|
| `cargo test -p obscura --test child_frame_scripts` | 1 | 8 | 3 | 0 | F-01 |
| `cargo test -p obscura-render` | 1 | 297 | 18 | 1 | F-02 |
| `cargo test -p obscura-control-plane` | 0 | 18 | 0 | 0 | incl. redelivery + recovery |
| `cargo test -p obscura-scheduler` | 0 | 4 | 0 | 0 | crash takeover passes |
| `cargo clippy -p obscura-js --lib` | 1 | - | 1 error | - | import_map never loops |
| `cargo clippy -p obscura-js --tests` | 1 | - | 8 errors | - | unused_io_amount |
| `cargo fmt --check` | 1 | - | 96 files | - | formatting drift |
| `cargo test -p obscura-control-plane nats_redelivery` | 0 | 2 | 0 | 0 | F-05 fixed |

## Environmental
- Postgres on 5432, NATS on 4222 available via Docker.
- CDP/CLI tests not run (timeouts) — not verified.
- SDK E2E not run.

## Skipped/blocked
- cargo-deny/cargo-audit unavailable.
- Some tests require OBSCURA_ALLOW_PRIVATE_NETWORK=1 (set in tests).

## Test quality observations
- Fixed tenant names changed to UUID-suffixed in working tree (F-18 mitigation).
- Redelivery test now compiles and passes.
- F5 adversarial tests pass.
- Render/child-frame failures remain.

## Status: CONFIRMED (F-05 fixed; F-01, F-02 remain).
