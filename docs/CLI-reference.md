## `obscura`

Top-level flags apply to every subcommand.

```
-v, --verbose                Enable info logging
-p, --port <PORT>            CDP port (default 9222)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --stealth                Consistent browser fingerprint + tracker blocking
    --obey-robots            Respect robots.txt
    --user-agent <UA>        Override the User-Agent
    --storage-dir <DIR>      Persistent cookies and localStorage
    --allow-private-network  Permit loopback / RFC1918 / link-local
    --v8-flags <FLAGS>       Raw V8 flags, applied at startup
-h, --help                   Help
-V, --version                Version
```

## `obscura fetch <URL>`

Load a URL and print its content or an evaluated expression.

```
    --dump <FORMAT>          html | text | links | markdown | original | assets | cookies
                             (default html)
    --selector <CSS>         Narrow output to a CSS selector
    --wait <SECONDS>         Fixed post-load delay; omitted uses adaptive settle (5s cap)
    --timeout <SECONDS>      Navigation timeout (default 30)
    --wait-until <LEVEL>     domcontentloaded | load | networkidle2 | networkidle0
                             (default load)
    --user-agent <UA>        Override the User-Agent
    --proxy <URL>            HTTP or SOCKS5 proxy
    --stealth                Consistent browser fingerprint + tracker blocking (global)
-e, --eval <JS>              Evaluate JS, print the result as JSON
-o, --output <FILE>          Write to a file instead of stdout
-s, --screenshot <FILE>      Capture the settled page as PNG (single URL)
-q, --quiet                  Suppress info logging
-v, --verbose                Enable verbose logging
```

`--screenshot` requires a render-enabled build. It uses a 1280×720 viewport by
default and may be combined with `--eval`; the expression runs before capture,
which is useful for scrolling or preparing page state. It is not available in
`--file` batch mode.

When `--wait` is omitted, Obscura drives timers and async work until the page
becomes quiescent, with a five-second ceiling. Supplying `--wait N` instead
requests a fixed `N`-second delay. `--timeout` separately bounds navigation.

`--dump` values:

| Value      | Output                                                    |
| ---------- | --------------------------------------------------------- |
| `html`     | Rendered HTML (default)                                   |
| `text`     | Plain text                                                |
| `markdown` | Markdown conversion                                       |
| `links`    | Every `<a href>`, one URL per line                        |
| `assets`   | Every external resource, one JSON object per line (DOM assets plus `fetch()`/XHR requests) |
| `original` | Raw HTTP response body (binary-safe, bypasses the engine) |
| `cookies`  | All cookies in the jar as a JSON array, including HttpOnly cookies invisible to `document.cookie` |

## `obscura serve`

Run the CDP server. Puppeteer and Playwright connect over WebSocket.

```
-p, --port <PORT>            CDP port (default 9222)
    --host <HOST>            Bind host (default 127.0.0.1)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --user-agent <UA>        Override the User-Agent
    --stealth                Consistent browser fingerprint + tracker blocking (global)
    --workers <N>            Worker processes (default 1)
    --font-dir <DIR>         Recursively load fonts once per worker (repeatable; render build)
    --allow-file-access      Permit CDP clients to navigate to file:// URLs
    --storage-dir <DIR>      Persistent cookies and localStorage
    --allow-private-network  Permit loopback / RFC1918 / link-local
-q, --quiet                  Suppress info logging
-v, --verbose                Enable info logging
```

Default endpoint is `ws://127.0.0.1:9222`.

## `obscura scrape [URLS]...`

Run a JS expression across many URLs in parallel.

```
-e, --eval <JS>              JS to run on each page
    --concurrency <N>        Parallel pages (default 10)
    --format <FORMAT>        Output format (default json)
    --timeout <SECONDS>      Per-URL timeout (default 60)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --stealth                Consistent browser fingerprint + tracker blocking (global)
    --allow-private-network  Permit loopback / RFC1918 / link-local
-q, --quiet                  Suppress info logging
-v, --verbose                Enable verbose logging
```

`--stealth`, `--proxy`, and `--allow-private-network` are global flags: they work before or after any subcommand, so each worker in a `scrape` run inherits stealth too.

Read URLs from stdin with `-`:

```bash
cat urls.txt | obscura scrape - --eval "document.title" --concurrency 20
```

Requires `obscura-worker` next to `obscura` in `PATH`.

## `obscura mcp`

Run obscura as an MCP server.

```
    --http                   HTTP transport instead of stdio
    --host <HOST>            HTTP bind host (default 127.0.0.1)
    --port <PORT>            HTTP port (default 3000)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --user-agent <UA>        Override the User-Agent
    --stealth                Consistent browser fingerprint + tracker blocking (global)
    --allow-private-network  Permit loopback / RFC1918 / link-local
-v, --verbose                Enable info logging
```

`--host` only applies with `--http`. The default `127.0.0.1` keeps the server loopback-only. A non-loopback bind requires `OBSCURA_MCP_TOKEN` (at least 32 bytes); use `OBSCURA_MCP_ALLOWED_ORIGINS` as well when a browser-based client needs access.

Default transport is stdio. See [Use the MCP server](Use-the-MCP-server.md).

Render-enabled builds add `browser_screenshot` and `browser_pdf` to the MCP
tool list. Streaming screencasts are available through CDP rather than MCP.
