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

The HTTP transport exposes a privileged browser session. Its guards are:

- **Bearer authentication.** Set `OBSCURA_MCP_TOKEN` and send it in the `Authorization: Bearer ...` header. A non-loopback bind is refused without a token of at least 32 bytes.
- **Origin allowlist.** Browser requests are refused by default. Set `OBSCURA_MCP_ALLOWED_ORIGINS` to permit specific browser origins. Native clients send no `Origin` and are unaffected.
- **Resource bounds.** Request bodies, headers, JSON-RPC batches, pending work, and live connections are bounded.

```bash
OBSCURA_MCP_TOKEN="$(openssl rand -hex 32)" \
OBSCURA_MCP_ALLOWED_ORIGINS="https://app.example.com" \
  obscura mcp --http --host 0.0.0.0
```

## Tools exposed

The server keeps a live browser session, so tools operate on the current page rather than taking a URL each call. Navigate first, then read or act.

Navigation and lifecycle:

- `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`, `browser_close`

Read the page:

- `browser_snapshot`: current URL, title, readable body text, and interactive
  element references. Optional `max_chars` limits the returned text.
- `browser_markdown`, `browser_links`, `browser_extract`: page as markdown, link list, or structured content.
- `browser_interactive_elements`, `browser_detect_forms`: actionable elements and form fields.
- `browser_get_attribute`, `browser_count`, `browser_search`: read an attribute, count matches, find text.

Interact:

- `browser_click`, `browser_fill`, `browser_fill_form`, `browser_type`, `browser_press_key`, `browser_select_option`, `browser_scroll`

Wait and run JS:

- `browser_wait_for`, `browser_wait_for_text`, `browser_evaluate`

Diagnostics:

- `browser_network_requests`, `browser_console_messages`

Visual output (render-enabled builds):

- `browser_screenshot`: current viewport as an MCP `image/png` content block.
- `browser_pdf`: current page as an embedded `application/pdf` resource.

`browser_screenshot` accepts optional positive `width` and `height` values in
CSS pixels and enforces a bounded capture size. `browser_pdf` accepts
`landscape`, `print_background`, `scale`, paper width/height, and top, bottom,
left, and right margins. Paper dimensions and margins are measured in inches.

Cookies and storage:

- `browser_get_cookies`, `browser_set_cookie`, `browser_clear_cookies`, `browser_storage_state`, `browser_set_storage_state`

Tabs:

- `browser_tab_new`, `browser_tab_list`, `browser_tab_switch`, `browser_tab_close`

Element references describe the current rendered page state and can become
stale after navigation, interaction, scrolling, or a framework rerender. Take
a fresh snapshot or interactive-element listing before acting again.

MCP exposes still-image and PDF output. It does not stream video frames; use
CDP `Page.startScreencast` for activity-driven screencasting.

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
