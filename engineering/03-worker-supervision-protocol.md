# Worker Supervision & IPC Wire Protocol

## 1. Supervisor Architecture

The Obscura Next Supervisor (`obscura-supervisor`) implements an OTP-style supervision tree ensuring high-availability browser operations:

```
                      [Supervisor]
                     /      |     \
                    ▼       ▼      ▼
                [Worker 1] [Worker 2] [Worker N]
                    │       │      │
                (Pipe/UDS) (Pipe/UDS) (Pipe/UDS)
```

### Self-Healing & Health Monitoring
- **Heartbeat Checks**: Periodic liveness probes (`{"cmd":"ping"}` -> `{"ok":true,"result":"pong"}`). Unresponsive workers exceeding the heartbeat timeout (default: 5000ms) are sent `SIGTERM`, followed by `SIGKILL`.
- **Crash Recovery & Exponential Backoff**: Unexpected worker exits trigger automated restarts with jittered backoff (100ms -> 500ms -> 2000ms -> max 10s) to prevent crash loops.
- **Worker Recycling Policies**:
  - `max_connections`: Recycle worker after $N$ sessions to prevent memory fragmentation.
  - `max_rss_mb`: Recycle when resident set size exceeds threshold.
  - `max_age`: Recycle workers periodically (e.g. after 1 hour).
- **Graceful Draining**: Replaced workers stop accepting new sessions, finish active workloads, and shut down cleanly.

---

## 2. IPC Wire Protocol Specification

The worker communicates over framed JSON-RPC / newline-delimited JSON.

### Commands

#### `session_create`
Allocates a new isolated session within the worker.
```json
{
  "id": "req-1",
  "cmd": "session_create",
  "params": {
    "session_id": "sess-abc-123",
    "proxy_url": "http://user:pass@proxy.corp:8080",
    "stealth": true,
    "user_agent": "Mozilla/5.0 ...",
    "allow_file_access": false,
    "allow_private_network": false
  }
}
```

#### `navigate`
Navigates the session's active page.
```json
{
  "id": "req-2",
  "cmd": "navigate",
  "params": {
    "session_id": "sess-abc-123",
    "url": "https://example.com",
    "wait_until": "domcontentloaded",
    "timeout_ms": 30000
  }
}
```

#### `evaluate`
Evaluates a JavaScript expression within the session isolate.
```json
{
  "id": "req-3",
  "cmd": "evaluate",
  "params": {
    "session_id": "sess-abc-123",
    "expression": "document.title",
    "await_promise": true,
    "timeout_ms": 5000
  }
}
```

#### `action` (Agent-Native)
Executes a high-level autonomous agent action.
```json
{
  "id": "req-4",
  "cmd": "action",
  "params": {
    "session_id": "sess-abc-123",
    "action": "click",
    "selector": "button.submit-order"
  }
}
```

#### `dump`
Extracts page state in structured formats.
```json
{
  "id": "req-5",
  "cmd": "dump",
  "params": {
    "session_id": "sess-abc-123",
    "format": "markdown"
  }
}
```

#### `session_destroy`
Tears down the session and releases isolate memory.
```json
{
  "id": "req-6",
  "cmd": "session_destroy",
  "params": {
    "session_id": "sess-abc-123"
  }
}
```

### Responses
All responses follow a consistent schema:
```json
{
  "id": "req-2",
  "ok": true,
  "result": {
    "title": "Example Domain",
    "url": "https://example.com/",
    "status": 200,
    "elapsed_ms": 42.1
  },
  "error": null
}
```
In case of error:
```json
{
  "id": "req-2",
  "ok": false,
  "result": null,
  "error": {
    "code": "NAVIGATION_TIMEOUT",
    "message": "Navigation timed out after 30000ms"
  }
}
```
