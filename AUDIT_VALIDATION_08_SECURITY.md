# AUDIT VALIDATION 08 — SECURITY

## Boundaries inspected
Auth, authorization, principal construction, credential verification, secret storage, constant-time comparisons, service identity, tenant authorization, MCP authorization, HTTP transport, SQL construction, command execution, file access, deserialization, protocol validation, request limits, resource exhaustion, logging, env config, metrics exposure.

## Findings
- F-08: validate_url file:// redirect gap (see 03).
- F-07: no OS sandbox (see 07).
- Service identity weakening: uncommitted change to `service_principal` accepts default "testsecret" when env var unset/empty. This allows service calls with "testsecret" in dev/test, but if deployed without env var, any caller knowing default can impersonate service. Risk: High in production if env not set. Recommend require non-empty secret and fail closed.
- Metrics server: no auth (F-15) — internal exposure. Low/Medium.
- Compose token: weak example token (F-16) — Low.
- SQL: parameterized queries used; no string concatenation observed in inspected paths.
- SSRF: strong policy; env_allows_private_network opt-in. Good.
- Constant-time comparisons: service secret uses string equality (not constant-time). Low (local).
- Tenant authorization: `authorize_tenant` enforced; internal service principal bypasses tenant checks by design. OK.
- Deserialization: serde_json; no unsafe deserialization observed.
- File access: gated by allow_file_access. Good.
- Logging: no secrets observed in logs; debug logs may include URLs.

## Status: PARTIALLY CONFIRMED. Security posture 3/5; production service-identity default is a real risk.
