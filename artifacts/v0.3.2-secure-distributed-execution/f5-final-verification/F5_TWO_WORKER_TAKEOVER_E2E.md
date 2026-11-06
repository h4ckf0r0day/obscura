# F5 Two-Worker Takeover E2E

## Status: BLOCKED (Docker daemon unstable)

## DB-Level Evidence (Verified)
- test_stale_completion_rejected_after_new_owner: PASS (6/6 control-plane tests)
- test_concurrent_lease_acquisition: PASS
- test_duplicate_dispatch_safe: PASS
- Stale token rejection: verified at DB level.

## Process-Level Evidence
- BLOCKED: Requires 2 worker agents + real browser + PG + NATS.
