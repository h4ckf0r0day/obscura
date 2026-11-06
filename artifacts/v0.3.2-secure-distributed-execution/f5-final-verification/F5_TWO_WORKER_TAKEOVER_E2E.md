# F5 Two-Worker Takeover E2E

## Status: BLOCKED (requires 2 worker agents with unique consumer names)

## Evidence
- Single-worker E2E verified: job completed.
- Two-worker takeover: NOT TESTED (worker agent uses hardcoded consumer name "worker-agent"; second instance would conflict).
- DB-level fencing: 6 tests PASS (stale token rejected, concurrent acquisition, etc.)

## Recommendation
- Make consumer name configurable per worker agent (env var) to support multiple agents.
