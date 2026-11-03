# Obscura Next Worker Protocol v1.0

Canonical framed JSON protocol used between the supervisor/SDKs and `obscura-runtime-worker`.

## Transport

- One JSON object per line over stdin (request) and stdout (response).
- Diagnostics go to stderr; stdout carries only protocol messages.
- Request/response correlation by `id`.

## Request

```json
{
  "id": "req-1",
  "version": "1.0",
  "cmd": "navigate",
  "session_id": "s1",
  "url": "https://example.com",
  "waitUntil": "load"
}
```

Fields:
- `id` (string, optional): correlation id; generated if omitted.
- `version` (string, default "1.0"): protocol version.
- `cmd` (string): operation.
- `session_id` (string): required for session-scoped commands.

## Response

```json
{
  "id": "req-1",
  "version": "1.0",
  "ok": true,
  "result": { "title": "Example", "url": "https://example.com" }
}
```

On failure:
```json
{
  "id": "req-1",
  "version": "1.0",
  "ok": false,
  "error": { "code": "SESSION_NOT_FOUND", "message": "Session 's1' not found" }
}
```

## Operations

Session: `session_create` (alias `create_session`), `session_destroy` (alias `destroy_session`), `session_status`.

Browser: `navigate`, `snapshot`, `evaluate`, `click`, `fill`, `type`, `screenshot`, `title`, `dump`.

Runtime: `ping`, `health`, `shutdown`.

## Error codes

`INVALID_JSON`, `SESSION_NOT_FOUND`, `SESSION_ALREADY_EXISTS`, `SECURITY_BLOCK`, `BROWSER_ERROR`, `EVAL_ERROR`, `CLICK_ERROR`, `FILL_ERROR`, `TYPE_ERROR`, `UNSUPPORTED_FEATURE`, `EXECUTION_ERROR`, `TIMEOUT`.

## Version mismatch

Responses carry `version`. Clients should reject mismatched major versions.
