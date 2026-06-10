## `obscura`

Top-level flags apply to every subcommand.

```
-v, --verbose                Enable info logging
-p, --port <PORT>            CDP port (default 9222)
    --proxy <URL>            HTTP or SOCKS5 proxy
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
    --dump <FORMAT>          html | text | links | markdown | original | assets
                             (default html)
    --selector <CSS>         Narrow output to a CSS selector
    --wait <SECONDS>         Extra wait after settle (default 5)
    --timeout <SECONDS>      Navigation timeout (default 30)
    --wait-until <LEVEL>     domcontentloaded | load | networkidle2 | networkidle0
                             (default load)
    --user-agent <UA>        Override the User-Agent
    --proxy <URL>            HTTP or SOCKS5 proxy
    --stealth                TLS fingerprint randomization + tracker blocking
-e, --eval <JS>              Evaluate JS, print the result as JSON
-o, --output <FILE>          Write to a file instead of stdout
-q, --quiet                  Suppress info logging
-v, --verbose                Enable verbose logging
```

`--dump` values:

| Value      | Output                                                    |
| ---------- | --------------------------------------------------------- |
| `html`     | Rendered HTML (default)                                   |
| `text`     | Plain text                                                |
| `markdown` | Markdown conversion                                       |
| `links`    | Every `<a href>`, one URL per line                        |
| `assets`   | Every external resource, one JSON object per line         |
| `original` | Raw HTTP response body (binary-safe, bypasses the engine) |

## `obscura serve`

Run the CDP server. Puppeteer and Playwright connect over WebSocket.

```
-p, --port <PORT>            CDP port (default 9222)
    --host <HOST>            Bind host (default 127.0.0.1)
    --proxy <URL>            HTTP or SOCKS5 proxy
    --user-agent <UA>        Override the User-Agent
    --stealth                TLS fingerprint randomization + tracker blocking
    --workers <N>            Worker processes (default 1)
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
    --allow-private-network  Permit loopback / RFC1918 / link-local
-q, --quiet                  Suppress info logging
-v, --verbose                Enable verbose logging
```

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
    --stealth                TLS fingerprint randomization + tracker blocking
    --allow-private-network  Permit loopback / RFC1918 / link-local
-v, --verbose                Enable info logging
```

`--host` only applies with `--http`. The default `127.0.0.1` keeps the server loopback-only; set `0.0.0.0` to bind all interfaces (for example a Docker Compose sidecar) and pair it with `OBSCURA_MCP_ALLOWED_ORIGINS`.

Default transport is stdio. See [Use the MCP server](Use-the-MCP-server.md).
