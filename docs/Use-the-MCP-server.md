`obscura mcp` exposes obscura as a Model Context Protocol server so MCP-capable clients (Claude Desktop, Claude Code, etc.) can drive it.

Render-enabled builds can load additional fonts for screenshots and PDF output:

```bash
obscura mcp --font-dir /path/to/fonts
```

Repeat `--font-dir` to load multiple directories. It recursively loads TTF, TTC,
OTF, and OTC files once before rendering, using the same opt-in font database as
`obscura serve`. System fonts are not scanned automatically. Invalid directories
fail at startup, and no-render builds reject a nonempty font directory option.
The option works with both stdio and HTTP transports.

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

The active page continues running timers and network callbacks between tool
calls over both stdio and HTTP. Task driving is wake-driven and parks when
the page is idle; a future timer does not block incoming MCP requests.

Navigation and lifecycle:

- `browser_navigate`, `browser_back`, `browser_forward`, `browser_reload`, `browser_close`

Read the page:

- `browser_snapshot`: current URL, title, actionable element references, control
  states, and visible body text. References can be passed directly to interaction
  tools without a separate listing call. Names use the same native accessibility
  logic as CDP, including associated labels and ARIA name precedence.
  `max_chars` defaults to 4000 and is shared by whole control descriptions first,
  then body text. Omission markers are added outside that budget. URL and title
  are separately capped at 256 characters each; `max_chars: 0` returns only that
  metadata. Descriptions preview at most 80 characters per name or value and
  do not include password control values.
- `browser_markdown`, `browser_links`, `browser_extract`: page as markdown, link list, or structured content.
- `browser_interactive_elements`, `browser_detect_forms`: actionable elements and form fields.

Snapshots and interactive-element listings exclude HTML/ARIA-hidden content;
render builds also apply computed CSS visibility. Hidden text explicitly
referenced by a visible control can still supply its accessible name. These are
compact browsing context, not a claim of complete accessibility conformance.
- `browser_get_attribute`, `browser_count`, `browser_search`: read an attribute, count matches, find text.

Interact:

- `browser_click`, `browser_fill`, `browser_fill_form`, `browser_type`, `browser_press_key`, `browser_select_option`, `browser_scroll`

Wait and run JS:

- `browser_wait_for`, `browser_wait_for_text`, `browser_evaluate`

`browser_evaluate` accepts JavaScript scripts, executes them once, and waits for
returned promises. `timeoutMs` defaults to 5000 ms and bounds synchronous
execution as well as promise settlement. Exceptions, rejected promises, and
timeouts return MCP tool errors (`isError: true`); a successful JavaScript
`null` or `undefined` result is still returned as `null` text.

Interaction tools also report execution failures as tool errors.
Internal interaction and extraction scripts have a five-second execution and
serialization deadline.
`browser_fill_form` does not submit if any field fails, and completes a queued
submit navigation before replying. A failed interaction can have partial side
effects; take a fresh snapshot before deciding whether to retry.

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

Storage-state export includes localStorage for all stored origins and
sessionStorage for all stored origins in the active tab. Restoration merges
the supplied keys into their declared origins and can run before navigation.
Restored sessionStorage belongs only to the active tab, creating a default
tab if needed; localStorage and cookies belong to the browser context.
Origins must be canonical HTTP(S) origins, and storage entries are `[key, value]`
string pairs. Invalid input is rejected before mutation. A quota failure returns
a tool error and reports that preceding writes may already have been applied.
Cookie entries pass through the browser's existing cookie validation.

Tabs:

- `browser_tab_new`, `browser_tab_list`, `browser_tab_switch`, `browser_tab_close`

Switching tabs retains JavaScript globals, event listeners, frame state and
pending promises. Each open tab keeps its runtime in memory until closed.
Autonomous task progress currently belongs to the active tab; retaining a tab
does not promise that background work runs while another tab is active.

Element references describe the current rendered page state and can become
stale after navigation, interaction, scrolling, or a framework rerender. Take
a fresh snapshot or interactive-element listing before acting again.

Treat references as opaque tokens. Each new snapshot or interactive-element
listing invalidates the previous tokens. A reference targets the original
native element, not a replacement or clone that copies its attributes; stale
references return a tool error instead of acting on a different element.

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

Local and private network addresses are blocked by default. CLI and stdio MCP
navigation errors explain the existing `--allow-private-network` /
`OBSCURA_ALLOW_PRIVATE_NETWORK=1` opt-in for your own local services. The hint
does not grant access, is omitted for known metadata endpoints and special
address ranges, and is not shown by the HTTP MCP transport.
