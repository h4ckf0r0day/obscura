# AUDIT_08 — MCP and SDK

## 1. MCP implementation

- `obscura-mcp/src/lib.rs` 3,435 lines: JSON-RPC over stdio, tool registry (32+ tools), browser state management, distributed tools (feature-gated).
- `obscura-mcp/src/http.rs`: streamable HTTP transport with bearer auth, CORS, origin checks, max body size.
- Tool schemas defined inline; input validation mixed.
- Distributed tools: submit_job, get_job, cancel_job, list_jobs, get_job_result; require control plane + scheduler + worker agent.
- SSE streaming test `sse_stream_does_not_wedge` exists.

## 2. SDKs

- Python SDK (`sdk/python/obscura_next`): client.py, models.py; tests test_client.py, test_e2e.py.
- TypeScript SDK (`sdk/typescript/src`): index, types; dist prebuilt.
- Need verify SDK args match server tool names/params. Not fully audited.

## 3. Findings

- MCP local browser tools and distributed tools enforce different security boundaries (local has SSRF via net; distributed control plane also SSRF check but only literal IP + DNS at submit time).
- Feature gating means default build lacks distributed tools; Docker build uses only `--features render`, so distributed MCP unavailable in official image.
- Tool result size caps (`DEFAULT_TEXT_LIMIT` 4000) good.
- Auth token min 32 bytes good.

## 4. Verification

- `obscura-mcp` tests: 44 unit + 2 + 1 + 1 pass.
- E2E MCP distributed verified per commit `d254d71` (job completed through full pipeline) — but requires manual services.
