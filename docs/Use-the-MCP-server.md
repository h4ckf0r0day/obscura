`obscura mcp` exposes obscura as a Model Context Protocol server so MCP-capable clients (Claude Desktop, Claude Code, etc.) can drive it.

## Run

Stdio (default, for direct client integration):

```bash
obscura mcp
```

HTTP (for remote or shared use):

```bash
obscura mcp --http --port 3000
```

The HTTP transport binds `127.0.0.1` by default. Bind all interfaces with `--host` for a container or sidecar deployment:

```bash
obscura mcp --http --host 0.0.0.0 --port 3000
```

With stealth and proxy:

```bash
obscura mcp --stealth --proxy http://proxy.example.com:8080
```

## Security

The HTTP transport has no built-in auth, so anyone who can reach the port can drive the browser. Two guards ship for the HTTP transport:

- **Origin allowlist.** Set `OBSCURA_MCP_ALLOWED_ORIGINS` to a comma-separated list of allowed `Origin` values. When set, a browser request from an unlisted origin is refused with `403` before it can drive the server, which blocks a malicious page from POSTing to a loopback MCP port. Native, non-browser clients send no `Origin` and are always allowed. Unset (the default) keeps the permissive behavior.
- **Body cap.** A single request body is capped at 16 MiB, so an unauthenticated caller cannot force a large allocation with an oversized `Content-Length`.

```bash
OBSCURA_MCP_ALLOWED_ORIGINS="https://app.example.com" obscura mcp --http --host 0.0.0.0
```

When you expose the HTTP transport beyond loopback, set the allowlist and put it behind a reverse proxy or network isolation that enforces auth.

## Tools exposed

The server keeps a live browser session, so tools operate on the current page rather than taking a URL each call. Navigate first, then read or act.

Navigation and lifecycle:

- `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`, `browser_close`

Read the page:

- `browser_snapshot`: accessibility/DOM snapshot of the current page.
- `browser_markdown`, `browser_links`, `browser_extract`: page as markdown, link list, or structured content.
- `browser_interactive_elements`, `browser_detect_forms`: actionable elements and form fields.
- `browser_get_attribute`, `browser_count`, `browser_search`: read an attribute, count matches, find text.

Interact:

- `browser_click`, `browser_fill`, `browser_fill_form`, `browser_type`, `browser_press_key`, `browser_select_option`, `browser_scroll`

Wait and run JS:

- `browser_wait_for`, `browser_wait_for_text`, `browser_evaluate`

Diagnostics:

- `browser_network_requests`, `browser_console_messages`

Cookies and storage:

- `browser_get_cookies`, `browser_set_cookie`, `browser_clear_cookies`, `browser_storage_state`, `browser_set_storage_state`

Tabs:

- `browser_tab_new`, `browser_tab_list`, `browser_tab_switch`, `browser_tab_close`

## Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or `%APPDATA%\Claude\claude_desktop_config.json` (Windows):

```json
{
  "mcpServers": {
    "obscura": {
      "command": "/path/to/obscura",
      "args": ["mcp"]
    }
  }
}
```

Restart Claude Desktop. The obscura tools appear in the tool list.

## Claude Code

```bash
claude mcp add obscura /path/to/obscura mcp
```

## With stealth in config

```json
{
  "mcpServers": {
    "obscura": {
      "command": "/path/to/obscura",
      "args": ["mcp", "--stealth"]
    }
  }
}
```
