## Runtime

### `OBSCURA_ALLOW_PRIVATE_NETWORK`

Allow fetches to addresses in the SSRF deny-set. Off by default to block SSRF. The deny-set is defined once, in `is_forbidden_ip` (`crates/obscura-ssrf/src/lib.rs`), and covers:

| Kind | Ranges |
| --- | --- |
| Loopback and unspecified | `127.0.0.0/8`, `0.0.0.0/8`, `::1`, `::` |
| Private (RFC 1918) | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` |
| Shared address space (RFC 6598, CGNAT) | `100.64.0.0/10`, which includes Tailscale addresses and the `100.100.100.200` cloud-metadata endpoint |
| Link-local | `169.254.0.0/16` (including the `169.254.169.254` cloud-metadata endpoint), `fe80::/10` |
| IPv6 unique-local | `fc00::/7` |
| Multicast and broadcast | `224.0.0.0/4`, `255.255.255.255`, `ff00::/8` |
| Documentation and benchmarking | `192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`, `198.18.0.0/15`, `2001:db8::/32`, `3fff::/20` |
| Other special-purpose IPv4 | `192.0.0.0/24` except `192.0.0.9` and `192.0.0.10`, `192.88.99.0/24`, `240.0.0.0/4` |
| Other special-purpose IPv6 | `100::/64` (discard-only), `64:ff9b:1::/48` (local-use NAT64) |

IPv6 addresses that embed an IPv4 address are unwrapped and the IPv4 address is checked against the rows above: IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`), NAT64 (`64:ff9b::/96`), 6to4 (`2002::/16`), and Teredo (`2001::/32`, server or client address).

A target on a Tailscale or other CGNAT address therefore needs this setting, just like one on a private LAN address.

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

### `OBSCURA_NAV_CHAIN_LIMIT`

How many documents a navigation chain may load, the first navigation included. Default 10, which allows the requested document and nine navigations the page itself triggers via `location` assignments or form submissions. Raise the value for an endpoint that chains longer for good reasons, such as an SSO handover across several providers. The low default is what stops a page that resets `location` on every load.

A zero is raised to 1. This loads the requested document. If the page wants to chain further afterwards, the call reports an error, as at any other limit. A value the engine does not read as a number is replaced by the default. This also applies to a negative value and to a value with a trailing space.

The time budget is not tied to this limit. A longer chain usually also needs a higher `OBSCURA_NAV_TIMEOUT_MS`, because its default of 30 seconds applies to the whole chain and not to the individual document.

```bash
OBSCURA_NAV_CHAIN_LIMIT=20 obscura serve
```

### `OBSCURA_SCRIPT_DEADLINE_MS`

Soft deadline for the complete page script-execution phase, including classic scripts and ES modules. Default 30000 (30 seconds). Raise it for a heavy SPA whose initial module is responsible for mounting an otherwise empty document. The engine also uses this value as a hard V8 watchdog budget, with a one-second grace period, so a synchronous script cannot run forever.

```bash
OBSCURA_SCRIPT_DEADLINE_MS=60000 obscura serve
```

### `OBSCURA_MODULE_BUDGET_MS`

Per-module graph-loading and evaluation budget for modules that enhance an already-rendered page. Default 3000 (3 seconds). Raise it when a module such as the Vite HMR client legitimately needs longer to evaluate:

```bash
OBSCURA_MODULE_BUDGET_MS=10000 obscura serve
```

This shorter budget applies when the document body already contains more than 50 descendant nodes, where modules are normally progressive enhancement and should not delay navigation indefinitely. For an unmounted SPA shell, Obscura instead gives each module the full `OBSCURA_SCRIPT_DEADLINE_MS` budget so the app has time to mount. Module network requests remain independently bounded by `OBSCURA_FETCH_TIMEOUT_MS`.

### `OBSCURA_CDP_COMMAND_TIMEOUT_MS`

Per-command V8 deadline for the CDP server. Default 60000 (60 seconds); `0`
disables it. The watchdog terminates JavaScript that exceeds the budget so it
can release the connection's V8 lock. It cannot preempt native Rust work. Keep
this budget above `OBSCURA_NAV_TIMEOUT_MS`.

```bash
OBSCURA_CDP_COMMAND_TIMEOUT_MS=30000 obscura serve
```

### `OBSCURA_CDP_TOKEN`

Bearer token for the CDP discovery and WebSocket endpoints. It is optional on
loopback. A non-loopback bind is refused unless this is set to at least 32
bytes. Pass it as `Authorization: Bearer <token>` in the CDP client's headers.

```bash
OBSCURA_CDP_TOKEN="$(openssl rand -hex 32)" obscura serve --host 0.0.0.0
```

### `OBSCURA_CDP_ALLOWED_ORIGINS`

Comma-separated `Origin` allowlist for CDP discovery and WebSocket requests.
Without it, clients that omit `Origin` or send the endpoint's own HTTP origin
are accepted. Origins with a different scheme, hostname, or port are refused
with an actionable 403, including other loopback applications. Forwarded
workers compare against the client-facing request authority.

List another application's exact origin to grant it access. Matching is
case-insensitive. `*` allows every origin, including `null`, and removes the
browser-origin boundary; avoid it for untrusted pages. Host validation and
`OBSCURA_CDP_TOKEN` authentication still apply to allowlisted origins.

```bash
OBSCURA_CDP_ALLOWED_ORIGINS="https://tunnel.example" obscura serve
```

### `OBSCURA_FETCH_TIMEOUT_MS`

Request timeout for scripted `fetch()`, `XMLHttpRequest`, and ES-module loads. Without it a request to a server that accepts the connection but never responds (including a CORS preflight) hangs forever and the XHR is stuck with no completion event. Default 30000 (30 seconds).

```bash
OBSCURA_FETCH_TIMEOUT_MS=15000 obscura serve
```

### `OBSCURA_PROXY`

Default proxy URL for `serve` and direct `obscura-worker` invocations.
The `scrape` command passes its `--proxy` value to workers through this variable;
an unset `--proxy` clears the inherited worker value. Use `--proxy` for `fetch`,
`scrape`, and `mcp`.

```bash
OBSCURA_PROXY=http://proxy.example.com:8080 obscura serve
```

## Resource limits

Values below are non-negative integers. Byte limits use bytes, not MiB.

| Variable | Default | Effect |
| --- | --- | --- |
| `OBSCURA_FETCH_MAX_BODY_BYTES` | 104857600 for JS fetch/XHR; 33554432 for modules | Maximum buffered response size. Setting it overrides both defaults. |
| `OBSCURA_NETWORK_BODY_BUFFER_ENTRIES` | 128 | Maximum retained response bodies in each page/runtime cache. Zero disables retention. |
| `OBSCURA_NETWORK_BODY_BUFFER_BYTES` | 2097152 | Largest individual response body retained for CDP retrieval. Zero disables retention; this is not a total-cache byte limit. |
| `OBSCURA_IO_STREAM_MAX_ENTRIES` | 32 | Maximum open CDP response-body streams per connection. |
| `OBSCURA_IO_STREAM_MAX_BYTES` | 268435456 | Total bytes retained by those streams. Oldest streams are evicted when a limit is exceeded. |
| `OBSCURA_MAX_LIVE_FRAMES` | 64 | Maximum live child frames per page, including initial realms awaiting adoption. |
| `OBSCURA_FRAME_MESSAGE_QUEUE_ENTRIES` | 4096 | Maximum queued cross-frame messages. |
| `OBSCURA_FRAME_MESSAGE_QUEUE_BYTES` | 8388608 | Maximum bytes in the cross-frame message queue. |

Disabling response-body retention does not disable network events. Reading an
evicted CDP body or stream returns an error.

## Rendering

These settings require a render-enabled build.

| Variable | Default | Effect |
| --- | --- | --- |
| `OBSCURA_SHOT_W` / `OBSCURA_SHOT_H` | 1280 / 720 | CLI screenshot viewport in CSS pixels, set before navigation. |
| `OBSCURA_SHOT_ANIMATION_TIME_MS` | Live timeline | Sample CSS animations at a finite, non-negative time for a CLI screenshot. |
| `OBSCURA_RENDER_RESOURCE_DEADLINE_MS` | 3000 | Resource preparation budget before CLI/CDP screenshots, in milliseconds. |
| `OBSCURA_RENDER_RESOURCE_WARMUP_MS` | 1000 | Resource preparation budget before navigation scripts, in milliseconds. |
| `OBSCURA_RENDER_RESOURCE_POST_SCRIPT_WARMUP_MS` | 1000 | Resource preparation budget after navigation scripts, in milliseconds. |
| `OBSCURA_RENDER_RESOURCE_SETTLE_WARMUP_MS` | 1000 | Resource preparation budget after settling, bounded by its remaining time. |

For example:

```bash
OBSCURA_SHOT_W=1440 OBSCURA_SHOT_H=1000 \
  obscura fetch https://example.com --screenshot page.png
```

## Stealth and identity

These tune the browser identity the engine presents so it stays internally consistent. See [Configure stealth and proxies](Configure-stealth-and-proxies.md) for the full picture.

### `OBSCURA_BLOCK_TRACKERS`

Controls the tracker blocklist used by the stealth HTTP transport. It is on by
default so `--stealth` retains its current privacy-first behavior. Set it to
`0`, `false`, `no`, or `off` to keep the stealth TLS/browser fingerprint while
allowing tracker requests.

The setting is read when a stealth client is created. Values are case-insensitive
and surrounding whitespace is ignored; unset, empty, and unrecognized values keep
blocking enabled. Non-stealth transport settings and SSRF protection are unchanged.

```bash
OBSCURA_BLOCK_TRACKERS=0 obscura --stealth fetch https://example.com
```

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

Pin a specific browser profile from the built-in pool by index (`0`-based). Each profile keeps `navigator.platform`, `userAgentData`, and the UA string internally consistent. Without it a single stable profile is used.

```bash
OBSCURA_PROFILE=2 obscura serve
```

### `OBSCURA_ROTATE_PROFILE`

Opt into picking a random profile per browser context instead of the stable default. Leave it off when you pin a TLS fingerprint, proxy region, or timezone, since a rotated profile would no longer match those.

```bash
OBSCURA_ROTATE_PROFILE=1 obscura serve
```

## MCP

### `OBSCURA_STORAGE_DIR`

Persistent profile directory for MCP. Equivalent to passing `--storage-dir`
before the `mcp` subcommand. See [Persist cookies and storage](Persist-cookies-and-storage.md).

```bash
OBSCURA_STORAGE_DIR=./obscura-data obscura mcp
```

### `OBSCURA_MCP_ALLOWED_ORIGINS`

Comma-separated `Origin` allowlist for the HTTP MCP transport (`obscura mcp --http`). Browser requests are refused by default; when set, only listed origins are accepted. Native clients without an `Origin` header pass the origin check.
If `OBSCURA_MCP_TOKEN` is set, they still need that token.

```bash
OBSCURA_MCP_TOKEN="$(openssl rand -hex 32)" \
OBSCURA_MCP_ALLOWED_ORIGINS="https://app.example.com" \
  obscura mcp --http --host 0.0.0.0
```

### `OBSCURA_MCP_TOKEN`

Bearer token for MCP HTTP requests. It is optional on loopback. A non-loopback
bind is refused unless this is set to at least 32 bytes.

```bash
OBSCURA_MCP_TOKEN="$(openssl rand -hex 32)" obscura mcp --http --host 0.0.0.0
```

## Logging

### `RUST_LOG`

Standard `tracing` filter. Common settings:

```bash
RUST_LOG=obscura=info obscura serve
RUST_LOG=obscura=debug obscura serve
RUST_LOG=obscura_cdp=trace,obscura_browser=debug obscura serve
```

`--verbose` selects debug-level logging unless `RUST_LOG` supplies a filter.
The default filter is `warn`; `--quiet` selects `off`.

## Build

### `OPENSSL_NO_VENDOR`

Forces `cargo build` to use the system OpenSSL instead of compiling the vendored copy. Set to `1` on hosts where the vendored OpenSSL fails (older VPS with AVX-512 issues).

```bash
OPENSSL_NO_VENDOR=1 cargo build --release -p obscura-cli --bins --features render
```

## V8

V8 flags are passed via `--v8-flags`, not environment variables:

```bash
obscura --v8-flags "--max-old-space-size=2048 --expose-gc" serve
```

Defaults are `--max-old-space-size=4096 --max-semi-space-size=4 --optimize-for-size` on 64-bit systems. In the current V8, `--optimize-for-size` overrides the requested semi-space cap to **1 MiB**, not 4 MiB. The old-generation ceiling remains 4 GB.

User flags are appended after the defaults, but V8 applies flag implications afterwards. To change the young-generation cap, also disable size optimization, for example `obscura --v8-flags "--no-optimize-for-size --max-semi-space-size=8" serve`. This can reduce GC CPU at the cost of more memory; measure the complete workload before adopting it. Use `--v8-flags "--print-flag-values"` to inspect the effective settings after V8 initialization.

## HTTP proxy environment

Obscura does not honor `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY`. Use `--proxy`; `OBSCURA_PROXY` is available for `serve` and direct worker
invocations as described above.
