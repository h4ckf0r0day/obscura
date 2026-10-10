`--storage-dir` persists cookies and `localStorage` so they survive across runs.

## CLI

```bash
obscura fetch https://example.com --storage-dir ./obscura-data
obscura fetch https://example.com --storage-dir ./obscura-data
```

The second invocation starts with the cookies and `localStorage` left by the first.

## Server

```bash
obscura serve --storage-dir ./obscura-data
```

All CDP sessions read and write to the same directory. Run separate `obscura serve` processes with different `--storage-dir` paths for isolated profiles.

`--storage-dir` is a global flag, so it applies to `obscura mcp` as well. The MCP
server picks it up from `OBSCURA_STORAGE_DIR`, which `obscura mcp` sets from the
flag, and it flushes the profile on exit.

## Layout

Inside `./obscura-data`:

- `cookies.json`: cookie jar in a stable format with `same_site`, `expires`, `http_only`, `secure`.
- `localStorage/<origin>-<digest>.json`: one file per origin, holding
  `{"origin": "...", "entries": [["key", "value"], ...]}`. The digest is an FNV-1a
  hash of the origin, so two origins that sanitize to the same filename cannot
  collide. Entries stay in insertion order because `Storage.key(i)` is defined
  in insertion order.

Inspect one with `jq`:

```bash
jq '.entries' ./obscura-data/localStorage/*.json
```

The format is stable. Inspect with `jq`:

```bash
jq '.[] | select(.domain == "example.com")' ./obscura-data/cookies.json
```

## When state is written

- After every navigation completes (CDP `Page.navigate`, MCP `browser_navigate`),
  and only when something actually changed.
- On clean process exit (Ctrl-C, SIGTERM).
- Manually via CDP `Network.setCookie` and `Network.deleteCookies`.

`sessionStorage` is deliberately not written to disk. It belongs to one tab and
is dropped when the tab goes away, the same as a real browser, but it does
survive navigation and CDP target switching within the process
([#678](https://github.com/h4ckf0r0day/obscura/issues/678)).

## Login once, scrape many

```bash
obscura serve --storage-dir ./session-1
```

Drive a login flow once via Puppeteer or Playwright. Stop the server. Subsequent runs against the same `--storage-dir` start logged in.

## Multiple identities

```bash
obscura serve --port 9222 --storage-dir ./identity-a
obscura serve --port 9223 --storage-dir ./identity-b
```

## Clear state

```bash
rm -rf ./obscura-data
```
