# F5 Full Adversarial E2E

## Status: PARTIALLY VERIFIED

## Verified
- Full pipeline (single worker): PostgreSQL -> outbox -> NATS -> scheduler -> worker agent -> supervisor -> browser -> result persistence. PASS.
- Multi-scheduler: 5 jobs split across 2 schedulers, no duplicate claims. PASS.
- DB-level fencing: 6 tests PASS.

## Blocked
- Two-worker takeover E2E (consumer name collision).
- Process termination on lease loss (no multi-process harness).
- Worker crash recovery.
