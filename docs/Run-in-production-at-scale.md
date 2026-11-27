## Docker

```bash
# The container runs as uid 65532, so a mounted storage dir must be writable
# by it. Without this the cookie jar silently fails to persist.
sudo install -d -o 65532 -g 65532 /srv/obscura/data

export OBSCURA_CDP_TOKEN="$(openssl rand -hex 32)"
docker run -d \
  --name obscura \
  --restart unless-stopped \
  -p 127.0.0.1:9222:9222 \
  -e OBSCURA_CDP_TOKEN \
  -v /srv/obscura/data:/data \
  h4ckf0r0day/obscura \
  serve --host 0.0.0.0 --storage-dir /data
```

The image runs `obscura serve` by default. Override with arguments after the image name.

### The container does not run as root

The image is built on `gcr.io/distroless/cc-debian12:nonroot` and runs as
uid/gid **65532**. Obscura executes untrusted page JavaScript in-process through
V8, so a V8 exploit lands with the process's privileges — there is no reason for
those to be root's.

Storage and privileges:

- **A mounted `--storage-dir` must be writable by uid 65532**, as above. This is
  the one thing that breaks quietly rather than loudly. Verified: with an
  unwritable storage dir Obscura completes the run, exits `0`, and prints no
  warning — the cookie jar simply never persists. Check that
  `{storage-dir}/cookies.json` exists after your first run rather than assuming
  it does.
- **Nothing in the image needs a privileged operation.** It binds an
  unprivileged port, reads the CA bundle, and writes only to the storage dir and
  a temp dir. Verified in the non-root image: an HTTPS fetch succeeds, so the CA
  bundle is readable, and a writable storage dir is populated.

### Why the in-container bind is `0.0.0.0`

A container-loopback bind is unreachable through `-p`, so `--host 0.0.0.0` is
required for the published port to work at all. Publish to **host loopback**
(`-p 127.0.0.1:9222:9222`, as above) rather than `-p 9222:9222`: the latter
exposes the port on every host interface, and Docker's iptables rules bypass
most host firewalls.

The container bind requires `OBSCURA_CDP_TOKEN`. Send it as a bearer token from
the CDP client and still publish the port to host loopback where possible.

## Systemd

Create a service account if it does not exist:

```bash
sudo useradd --system --user-group --home-dir /var/lib/obscura --shell /usr/sbin/nologin obscura
```

Save this unit as `/etc/systemd/system/obscura.service`. `StateDirectory` creates
`/var/lib/obscura` with the service account as its owner:

```ini
[Unit]
Description=Obscura headless browser
After=network.target

[Service]
ExecStart=/usr/local/bin/obscura serve --port 9222 --storage-dir /var/lib/obscura
StateDirectory=obscura
Restart=always
RestartSec=5
User=obscura
Group=obscura
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now obscura
sudo journalctl -fu obscura
```

## Workers

`obscura serve --workers N` runs N CDP server workers behind the listener.

```bash
obscura serve --workers 4
```

Each worker accepts multiple CDP connections. Start with a small worker count
and measure CPU and memory under your workload. Each connection stays on the
worker that accepted it. `--max-connections` defaults to 128 per worker; excess
connections receive HTTP 503.

Worker memory can grow over days. Recycle workers with `--worker-max-connections`, `--worker-max-rss` (MB, Linux) or `--worker-max-age` (seconds). A replacement starts first and takes all new connections; the old worker finishes its live connections, then exits. `--worker-drain-timeout` (default 120 s) bounds that wait, after which remaining connections are closed, so set it above your longest session.

```bash
obscura serve --workers 50 --worker-max-connections 500 --worker-max-rss 1500
```

## V8 heap

The V8 old-generation ceiling is 4 GB on 64-bit systems. The defaults request `--max-semi-space-size=4`, but the current V8's `--optimize-for-size` implication overrides the effective semi-space cap to 1 MiB. Override the old-generation ceiling with:

```bash
obscura --v8-flags "--max-old-space-size=2048" serve
```

Flags you pass are appended after the defaults, so this old-generation override retains memory tuning. To change the young-generation cap, also pass `--no-optimize-for-size`; V8's flag implications otherwise override your cap. Disabling size optimization can save GC CPU but increase peak memory. Benchmark both before changing production defaults.

## Parallel scrape

`obscura scrape` fans out a list of URLs across worker processes:

```bash
obscura scrape \
  --concurrency 20 \
  --format json \
  --timeout 60 \
  url1 url2 url3 ...
```

Reads URLs from stdin:

```bash
cat urls.txt | obscura scrape --concurrency 20 -
```

Keep `obscura-worker` in the same directory as the `obscura` executable.

## Resource limits

Per-process memory cap with systemd:

```ini
[Service]
MemoryMax=4G
MemoryHigh=3G
```

Per-container with Docker:

```bash
docker run --memory=4g --cpus=2 ...
```

## Reverse proxy

Expose obscura on TLS through nginx or caddy:

```nginx
location /obscura/ {
  proxy_pass http://127.0.0.1:9222/;
  proxy_http_version 1.1;
  proxy_set_header Upgrade $http_upgrade;
  proxy_set_header Connection "upgrade";
  proxy_read_timeout 86400;
}
```

CDP needs WebSocket upgrade and long read timeouts.

## Authentication

Obscura requires `OBSCURA_CDP_TOKEN` (at least 32 bytes) for every non-loopback
CDP bind. Pass it in the client's `Authorization` header. Also:

- Bind to `127.0.0.1` and require SSH for access (default).
- Put it behind a reverse proxy that enforces an additional auth boundary.
- Use Docker network isolation.

Never bind `0.0.0.0` on a public IP without one of the above.

## MCP HTTP transport

`obscura mcp --http` binds `127.0.0.1` by default. A non-loopback bind requires a bearer token. Browser origins are denied by default; set an allowlist only when a browser-based MCP client needs access:

```bash
OBSCURA_MCP_TOKEN="$(openssl rand -hex 32)" \
OBSCURA_MCP_ALLOWED_ORIGINS="https://app.example.com" \
  obscura mcp --http --host 0.0.0.0 --port 3000
```

Request bodies and headers, batch size, pending requests, and connections are bounded. Keep the service on an internal network even with authentication. See [Use the MCP server](Use-the-MCP-server.md).

## Observability

```bash
obscura serve --verbose
RUST_LOG=obscura=debug obscura serve
```

`--verbose` selects debug-level logging unless `RUST_LOG` supplies a filter.
The default filter is `warn`. Logs go to stderr.

## Reliability and timeouts

A V8 watchdog terminates scripts that exceed their execution budget. DOM ops
catch panics, and the DOM tree rejects cyclic mutations. Navigation and
scripted network requests have separate timeouts. These controls do not prevent
every native crash or resource-exhaustion failure; use worker recycling and
process memory limits for long-running deployments.

Tune the bounds with environment variables (see [Environment variables](Environment-variables.md)):

```bash
OBSCURA_NAV_TIMEOUT_MS=60000 \
OBSCURA_SCRIPT_DEADLINE_MS=45000 \
OBSCURA_MODULE_BUDGET_MS=10000 \
OBSCURA_CDP_COMMAND_TIMEOUT_MS=70000 \
OBSCURA_FETCH_TIMEOUT_MS=20000 \
  obscura serve
```

`OBSCURA_NAV_TIMEOUT_MS` is the per-navigation ceiling (default 30000). `OBSCURA_SCRIPT_DEADLINE_MS` bounds the complete script phase (default 30000), while `OBSCURA_MODULE_BUDGET_MS` bounds each enhancement module's graph loading and evaluation (default 3000; unmounted SPA shells use the full script deadline). `OBSCURA_CDP_COMMAND_TIMEOUT_MS` is the outer per-CDP-command V8 deadline (default 60000, `0` disables), so keep it above the navigation ceiling. `OBSCURA_FETCH_TIMEOUT_MS` bounds scripted fetch/XHR and module network requests (default 30000).
