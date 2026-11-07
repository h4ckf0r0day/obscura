# Obscura MCP Architecture

## Overview
Obscura MCP is a Model Context Protocol server exposing browser automation tools. It supports STDIO and Streamable HTTP transports.

## Components
- `BrowserState`: Manages tabs, pages, and browser context.
- `dispatch()`: Routes JSON-RPC methods to handlers.
- `handle_tool_call()`: Executes individual tools.
- Transport: STDIO (newline-delimited JSON) or HTTP (Streamable HTTP).

## Integration with Control Plane
MCP currently uses `BrowserState` (local browser) directly. Distributed control-plane integration (submit_job, get_job, etc.) is a future enhancement. MCP does not currently bypass tenant authorization because no control plane is wired.

## Security
- Bearer token auth (via OBSCURA_MCP_TOKEN env var, min 32 chars).
- Origin allowlist (via OBSCURA_MCP_ALLOWED_ORIGINS).
- SSRF protections inherited from obscura-net.
