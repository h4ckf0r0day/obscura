# Phase 5.13 Baseline Freeze Report

## Summary

Phase 5.12 F5 implementation remains intact. Phase 5.13 replaces the timing-based
100 ms startup delay with deterministic child-readiness synchronization and
freezes the verified baseline.

## Changes

- `crates/obscura-worker/src/bin/worker_main.rs`: emits `{"ready":true}` after
  installing `PR_SET_PDEATHSIG` and before entering the request loop.
- `crates/obscura-supervisor/src/lib.rs`: waits for the readiness line with a
  5 s timeout before proceeding; removes the 100 ms sleep.

## Verification

- Linux aarch64 adversarial suite: all scenarios passed.
- Supervisor tests: 12 passed on Linux.
- macOS build: succeeded.