# AUDIT_07 — Security

## 1. Authentication/authorization

- Distributed: bootstrap secret env var + credential hashing. `service_principal` requires `OBSCURA_SERVICE_BOOTSTRAP_SECRET` match; empty secret rejects all. Good baseline.
- Principal fields not encapsulated? `Principal` fields private but constructed via service(). Admin flag manual (test creates admin principal directly). 
- MCP HTTP (`obscura-mcp/src/http.rs`): need check auth. Let me note: HTTP MCP may lack token auth in some paths. (Not fully audited.)
- CDP server: `OBSCURA_CDP_TOKEN` env for non-loopback binds. Need verify enforced.

## 2. SSRF

- Strong policy in obscura-ssrf (forbidden IPs incl. cloud metadata, CGNAT, 6to4, Teredo).
- DNS resolution checked via SsrfGuardResolver at connect time for both transports.
- Redirects validated via validate_url.
- Gaps: validate_url DNS check only port 80 and only if DNS resolves; TOCTOU between resolve and connect mitigated by custom resolver. Redirect to file://? validate_url allows file scheme only if scheme file; but remote redirect to file:// could be blocked by scheme check? validate_url allows file scheme unconditionally — potential local file read via redirect. Medium.
- wreq (stealth) implements same resolver.

## 3. Input/output security

- SQL: parameterized queries (sqlx) — no injection observed.
- Command injection: supervisor spawns configured binary; no shell. Good.
- Path traversal: not fully audited in CDP file ops.
- Deserialization: serde_json; malformed messages handled in some paths.
- Oversized payloads: no explicit limits observed in MCP HTTP / CDP.

## 4. Secrets

- Bootstrap secret via env; never logged (check). 
- `.env` files? None tracked? Check. 
- Docker compose hardcodes `OBSCURA_CDP_TOKEN=abcdefghijklmnopqrstuvwxyz0123456789` (weak, example). 
- Git history not scanned for secrets (tool unavailable). 

## 5. Supply chain

- deny.toml present. cargo audit not run (tool maybe unavailable).
- Vendor taffy/cosmic-text/wreq-util vendored; need verify provenance.

## 6. Classification

| ID | Severity | Component | Summary |
|---|---|---|---|
| SEC-1 | Medium | net redirect | file:// redirect may allow local file read |
| SEC-2 | Medium | compose | weak hardcoded CDP token example |
| SEC-3 | Medium | auth | bootstrap secret single shared secret; no rotation API |
| SEC-4 | Low | telemetry | metrics HTTP no auth |
| SEC-5 | Info | isolation | process supervision not OS sandbox |
| SEC-6 | Medium | MCP HTTP | auth not verified (audit gap) |

Detailed evidence in AUDIT_07; more findings require deeper per-file audit.
