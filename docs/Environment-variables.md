## Runtime

### `OBSCURA_ALLOW_PRIVATE_NETWORK`

Allow fetches to loopback (`127.0.0.0/8`), RFC1918 (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`), and link-local (`169.254.0.0/16`, including the `169.254.169.254` cloud-metadata endpoint) addresses. The deny-set also covers the unspecified address (`0.0.0.0` / `::`), IPv6 unique-local (`fc00::/7`), and any IPv4-mapped form of the above. Off by default to block SSRF.

The guard validates at DNS-resolution time as well as on literal hosts, so a public hostname that resolves to a forbidden address is rejected at connect time (DNS-rebinding safe), not just hosts written as raw IPs.

Truthy values: `1`, `true`, `yes`, `on`.

```bash
OBSCURA_ALLOW_PRIVATE_NETWORK=1 obscura fetch http://localhost:8080
```

Per-process equivalent: `--allow-private-network` on any subcommand.

### `OBSCURA_NAV_TIMEOUT_MS`

Hard ceiling on a single navigation. Default 30000 (30 seconds). Applies to `Page.navigate` and the CLI `fetch` command.

```bash
OBSCURA_NAV_TIMEOUT_MS=60000 obscura serve
```

### `OBSCURA_CDP_COMMAND_TIMEOUT_MS`

Per-command deadline for the CDP server. A hung page (a runaway `Runtime.evaluate`, a synchronous DOM op) is terminated after this budget so one bad session cannot hold the shared V8 lock and stall the others. Default 60000 (60 seconds); `0` disables it. Navigation self-bounds via `OBSCURA_NAV_TIMEOUT_MS` well under this.

```bash
OBSCURA_CDP_COMMAND_TIMEOUT_MS=30000 obscura serve
```

### `OBSCURA_FETCH_TIMEOUT_MS`

Request timeout for scripted `fetch()`, `XMLHttpRequest`, and ES-module loads. Without it a request to a server that accepts the connection but never responds (including a CORS preflight) hangs forever and the XHR is stuck with no completion event. Default 30000 (30 seconds).

```bash
OBSCURA_FETCH_TIMEOUT_MS=15000 obscura serve
```

### `OBSCURA_PROXY`

Default proxy URL used by `obscura-worker` for the parallel `scrape` command when no `--proxy` flag is set.

```bash
OBSCURA_PROXY=http://proxy.example.com:8080 obscura scrape - < urls.txt
```

## Stealth and identity

These tune the browser identity the engine presents so it stays internally consistent. See [Configure stealth and proxies](Configure-stealth-and-proxies.md) for the full picture.

### `OBSCURA_TIMEZONE`

Pins the process timezone before V8/ICU reads it, so `Date` (`getTimezoneOffset`, `toString`) and `Intl.DateTimeFormat` report one consistent zone. Default `Europe/Berlin`. Set it to match the exit IP's region.

```bash
OBSCURA_TIMEZONE=America/New_York obscura serve
```

### `OBSCURA_GEOLOCATION`

Override the coordinates the `navigator.geolocation` shim reports, as `lat,lon`. Without it the shim reports a fixed default. Keep it consistent with `OBSCURA_TIMEZONE` and the proxy region.

```bash
OBSCURA_GEOLOCATION="40.7128,-74.0060" obscura serve
```

### `OBSCURA_PROFILE`

Pin a specific browser profile from the built-in pool by index (`0`-based). Each profile keeps `navigator.platform`, `userAgentData`, the UA string, and the GPU renderer internally consistent. Without it a single stable profile is used.

```bash
OBSCURA_PROFILE=2 obscura serve
```

### `OBSCURA_ROTATE_PROFILE`

Opt into picking a random profile per browser context instead of the stable default. Leave it off when you pin a TLS fingerprint, proxy region, or timezone, since a rotated profile would no longer match those.

```bash
OBSCURA_ROTATE_PROFILE=1 obscura serve
```

## MCP

### `OBSCURA_MCP_ALLOWED_ORIGINS`

Comma-separated `Origin` allowlist for the HTTP MCP transport (`obscura mcp --http`). Off by default, which keeps the permissive behavior. When set, a browser request whose `Origin` is not listed is refused with `403` before it can drive the server; native, non-browser MCP clients (which send no `Origin`) are always allowed. Use it to stop cross-origin pages from reaching a loopback MCP port.

```bash
OBSCURA_MCP_ALLOWED_ORIGINS="https://app.example.com" obscura mcp --http --host 0.0.0.0
```

## Logging

### `RUST_LOG`

Standard `tracing` filter. Common settings:

```bash
RUST_LOG=obscura=info obscura serve
RUST_LOG=obscura=debug obscura serve
RUST_LOG=obscura_cdp=trace,obscura_browser=debug obscura serve
```

`--verbose` on the CLI is equivalent to `RUST_LOG=obscura=info`.

## Build

### `OPENSSL_NO_VENDOR`

Forces `cargo build` to use the system OpenSSL instead of compiling the vendored copy. Set to `1` on hosts where the vendored OpenSSL fails (older VPS with AVX-512 issues).

```bash
OPENSSL_NO_VENDOR=1 cargo build --release
```

## V8

V8 flags are passed via `--v8-flags`, not environment variables:

```bash
obscura serve --v8-flags "--max-old-space-size=2048 --expose-gc"
```

Defaults are `--max-old-space-size=4096 --max-semi-space-size=4 --optimize-for-size` on 64-bit systems (a 4 GB old-space ceiling, a capped young generation, and codegen tuned for a smaller footprint to cut RSS). Anything you pass with `--v8-flags` is appended after these, and V8 uses the last value for a repeated flag, so your value wins for that flag while the other defaults stay in effect.

## HTTP proxy environment

Obscura does not honor `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY`. Use `--proxy` or `OBSCURA_PROXY`.
