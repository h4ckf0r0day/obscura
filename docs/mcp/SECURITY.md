# Obscura MCP Security

## Authentication
- Bearer token via OBSCURA_MCP_TOKEN (min 32 chars). Required for HTTP transport.
- STDIO: local trust model (no network boundary).
- Origin allowlist via OBSCURA_MCP_ALLOWED_ORIGINS.

## Authorization
- MCP currently uses local BrowserState (no tenant isolation) for browser tools.
- Distributed tools authenticate via OBSCURA_MCP_CREDENTIAL_ID/SECRET env vars.
- Tenant identity derived from authenticated Principal (not caller-supplied).
- Cross-tenant access rejected by authorize_tenant().

## SSRF
- Inherited from obscura-net: SsrfGuardResolver validates DNS + IP.
- validate_url checks literal IPs and localhost domains.
- Redirect validation on every hop (max 20).

## Secrets
- Tokens never logged. Credentials never returned in responses.
