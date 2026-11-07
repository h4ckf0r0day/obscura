# Obscura MCP Configuration

## Environment Variables
- OBSCURA_MCP_TOKEN: Bearer token (min 32 chars). Required for HTTP.
- OBSCURA_MCP_ALLOWED_ORIGINS: Comma-separated allowed origins.
- OBSCURA_PROXY: Proxy URL.
- OBSCURA_STEALTH: Enable stealth mode (0/1).
- OBSCURA_USER_AGENT: Custom user agent.

## Transport
- STDIO: default, no config needed.
- HTTP: --transport http --port 3000 --host 127.0.0.1

## Timeouts
- Default request timeout: 30s.
- Configurable via tool arguments (timeout parameter).
