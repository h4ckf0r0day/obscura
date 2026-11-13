# AUDIT_15 — API and Protocol Contracts

## 1. Contracts

- JSON-RPC 2.0 for MCP.
- Worker IPC: WorkerRequest/WorkerResponse JSON lines (version 1.0).
- DispatchMessage version "1.0".
- EventEnvelope with schema_version (currently 1).
- Control plane REST? MCP tools wrap.

## 2. Findings

- No protocol version negotiation; consumers assume 1.0.
- `DispatchMessage` has `version` field but not validated on receive.
- EventEnvelope schema_version not validated.
- Old jobs/messages may be processed after upgrade without migration logic — risk.
- MCP tool schemas inline; no generated schema; drift possible between TS/Python SDKs and server.

## 3. Backward/forward compat

- Not verified. Recommend adding version checks and rejecting unknown major versions.
