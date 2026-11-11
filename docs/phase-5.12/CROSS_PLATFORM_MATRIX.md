# Phase 5.12 Cross-Platform Matrix

| Criterion | macOS | Linux |
|---|---|---|
| Worker crash recovery | VERIFIED | VERIFIED |
| NATS redelivery/recovery | VERIFIED | VERIFIED |
| Multi-process takeover | VERIFIED | VERIFIED |
| Browser process containment | VERIFIED (reaper) | VERIFIED (PDEATHSIG + reaper) |
| Stale completion protection | VERIFIED | VERIFIED |
| End-to-end F5 | VERIFIED | VERIFIED |

## Notes

- Linux containment uses worker-side `PR_SET_PDEATHSIG` (reliable, single-threaded
  startup) plus the detached external reaper. `PR_SET_PDEATHSIG` set from the
  multi-threaded supervisor was removed because it could fire prematurely.
- macOS containment relies on the detached reaper (no `prctl`).
- Linux build/runtime verification completed in this phase.