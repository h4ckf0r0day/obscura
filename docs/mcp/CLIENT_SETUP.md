# Obscura MCP Client Setup

## Claude Desktop
```json
{
  "mcpServers": {
    "obscura": {
      "command": "obscura",
      "args": ["mcp"],
      "env": { "OBSCURA_MCP_TOKEN": "your-token-here" }
    }
  }
}
```

## OpenCode / Cursor / Cline
Similar configuration with command and args.

## HTTP Transport
```bash
obscura mcp --transport http --port 3000
```
Connect via Streamable HTTP at http://127.0.0.1:3000/mcp
