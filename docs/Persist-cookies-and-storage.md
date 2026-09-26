`--storage-dir` persists cookies, localStorage and IndexedDB to disk so they survive across runs.

## CLI

```bash
obscura fetch https://example.com --storage-dir ./obscura-data
obscura fetch https://example.com --storage-dir ./obscura-data
```

The second invocation starts with the cookies, localStorage and IndexedDB data left by the first.

## Server

```bash
obscura serve --storage-dir ./obscura-data
```

All CDP sessions read and write to the same directory. Run separate `obscura serve` processes with different `--storage-dir` paths for isolated profiles.

## Layout

Inside `./obscura-data`:

- `cookies.json`: cookie jar in a stable format with `same_site`, `expires`, `http_only`, `secure`.
- `web_storage.json`: localStorage and IndexedDB, keyed by origin:
  `{"localStorage": {"https://example.com": {"key": "value"}}, "indexedDB": {"https://example.com": {"<database>": "<snapshot>"}}}`.
  Opaque (`null`) origins are never written.

localStorage is shared by every page and frame of an origin in the same
process and kept across navigations; each origin has a 10 MiB quota.
sessionStorage belongs to one tab and is not written to disk.

The format is stable. Inspect with `jq`:

```bash
jq '.[] | select(.domain == "example.com")' ./obscura-data/cookies.json
jq '.localStorage["https://example.com"]' ./obscura-data/web_storage.json
```

## When state is written

The same moments apply to cookies and `web_storage.json`:

- On clean process exit (Ctrl-C, SIGTERM).
- After every navigation completes (CDP `Page.navigate`).
- Manually via CDP `Network.setCookie` and `Network.deleteCookies`.

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
