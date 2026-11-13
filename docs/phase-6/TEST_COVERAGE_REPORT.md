# Phase 6 Test Coverage Report

## Added
- `obscura-ssrf`: 6 unit tests (loopback, private, metadata, mapped IPv6, public, env opt-in).
- Telemetry metrics auth path (unit test not added; manual).

## Gaps
- SSRF redirect-path tests.
- Scheduler/control-plane integration tests with real Postgres/NATS.
- SDK E2E.
- Clippy -D warnings not yet green.
