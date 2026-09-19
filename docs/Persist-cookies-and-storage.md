`--storage-dir` persists cookies and localStorage to disk so they survive across runs.

## CLI

```bash
obscura fetch https://example.com --storage-dir ./obscura-data
obscura fetch https://example.com --storage-dir ./obscura-data
```

The second invocation starts with the cookies and localStorage left by the first.

## Server

```bash
obscura serve --storage-dir ./obscura-data
```

All CDP sessions read and write to the same directory. Run separate `obscura serve` processes with different `--storage-dir` paths for isolated profiles.

## Layout

Inside `./obscura-data`:

- `cookies.json`: cookie jar in a stable format with `same_site`, `expires`, `http_only`, `secure`.
- `localStorage/<origin>.json`: one file per origin.

The format is stable. Inspect with `jq`:

```bash
jq '.[] | select(.domain == "example.com")' ./obscura-data/cookies.json
```

## When state is written

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

## What the store is, and is not

`{storage-dir}/cookies.json` holds live session tokens **in plaintext**. The
file is written 0600, and a directory Obscura creates for it is 0700, but the
contents are not encrypted — Chrome uses Keychain on macOS and DPAPI on Windows;
Obscura has no equivalent. Do not point `--storage-dir` at shared or backed-up
storage for a credentialed session.

In the container image the process runs as uid 65532, so a mounted storage dir
must be writable by that uid. If it is not, the run still completes and exits 0
— check that `cookies.json` exists after the first run rather than assuming it.
