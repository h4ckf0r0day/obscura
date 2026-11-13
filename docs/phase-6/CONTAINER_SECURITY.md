# Phase 6 Container Security

- Non-root users in runtime images.
- No hardcoded credentials; required env vars fail compose if missing.
- Minimal runtime deps; build tools only in builder stage.
- CDP token mandatory for non-loopback exposure.
- Metrics token optional but recommended.
