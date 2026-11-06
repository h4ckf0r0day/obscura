# F5 Process Reaping E2E

## Status: VERIFIED (single worker, real browser)

- E2E pipeline: JOB_ID submitted -> status=completed -> FINAL_STATUS=completed.
- Worker agent: real supervised worker spawned, handshake succeeded.
- Browser: navigation to data: URL, result persisted.
- Process termination on lease loss: NOT TESTED (requires controlled lease expiry in multi-process setup).

## Evidence
- cargo run --release -p obscura-scheduler --example e2e_verify: FINAL_STATUS=completed
