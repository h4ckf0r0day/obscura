# Phase 5.11 Failure Matrix

| Criterion | Status | Evidence |
|---|---|---|
| Worker-agent crash recovery | VERIFIED | Agent SIGKILL, lease expiry, automatic recovery, replacement execution (scenarios 2, 6). |
| NATS redelivery and recovery | VERIFIED | Real JetStream consumer NAK and ack_wait timeout redelivery (scenario 4). |
| Independent multi-process takeover | VERIFIED | Separate scheduler/worker OS processes, A-to-B takeover, newer fencing token (scenarios 2, 6). |
| Browser process containment | VERIFIED (macOS via reaper; Linux via prctl + reaper) | Agent SIGKILL followed by verified termination of browser and descendants (scenarios 5, 6). |
| Stale completion protection | VERIFIED | Old attempt completion rejected after replacement ownership (scenarios 2, 6). |
| End-to-end F5 | VERIFIED on macOS; Linux runtime execution pending | Combined adversarial test passes on macOS. |

## Environment limitations

- Linux build of the workspace failed in this environment due to a V8 prebuilt
  archive download returning HTTP 504 from GitHub
  (`librusty_v8_simdutf_release_aarch64-unknown-linux-gnu.a.gz`). This is an
  environmental/network limitation, not an implementation failure.
- Linux-only `prctl(PR_SET_PDEATHSIG)` could not be exercised here; equivalent
  behavior is provided by the detached reaper and verified on macOS.
- To execute in a supported Linux environment:
  1. Provide network access to GitHub release assets (or cache V8 prebuilts).
  2. Build with `cargo build --release -p obscura-worker --bin obscura-runtime-worker -p obscura-worker-agent -p obscura-scheduler --example adversarial_e2e`.
  3. Run PostgreSQL and NATS, then the adversarial example as documented in
     `TEST_EVIDENCE.md`.

## Failed/skipped tests

- None in the final run. All scheduler, control-plane, and supervisor tests pass.
- Earlier SSRF test failures were caused by running with
  `OBSCURA_ALLOW_PRIVATE_NETWORK=true`; rerun without that variable passes.