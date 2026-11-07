# Obscura MCP Testing

## Tests
- 48/48 MCP tests pass (cargo test -p obscura-mcp).
- STDIO and HTTP transport tests.
- Tool validation tests.
- Security tests (origin, bearer token).

## Running
```bash
cargo test -p obscura-mcp
cargo test -p obscura-mcp --features render
```
