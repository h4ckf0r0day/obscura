---
name: web-browser
description: "Drive web pages through Obscura, a Rust headless browser with CDP: navigate, click, fill forms, read content, screenshot, emulate devices, and inspect console/network. Use when a task needs a real browser. One-shot reads need no setup; stateful interaction needs start.js."
license: Apache-2.0
---

# Web Browser Skill (Obscura)

A real browser for agents, backed by [Obscura](https://github.com/h4ckf0r0day/obscura) — a
CDP-compatible engine in Rust. No Chrome or Chromium is used, installed or required.

## Pick the cheap path first

Most browser work is "read this page". That needs no server:

```bash
./scripts/read.js https://example.com                 # page text
./scripts/read.js https://example.com --markdown      # readable markdown
./scripts/read.js https://example.com --eval='document.title'
```

Only reach for the stateful commands when you need cookies, a session, multiple steps, or
input. Measured on this machine: a one-shot `read.js` costs 600–930 ms, while a broker command
costs 50–110 ms — a 6-command task costs roughly **300–670 ms via the broker vs 3800–4900 ms as
six one-shot fetches**, about 6–7×. The gap grows with every extra step, because the broker
reuses the page.

## Stateful work: start, then use commands

```bash
./scripts/start.js            # starts the CDP server + the state broker
./scripts/nav.js https://example.com
./scripts/eval.js 'document.title'
./scripts/stop.js
```

Page state survives between commands because a broker process holds the CDP connection.
This is required, not an optimisation: Obscura scopes page targets to the WebSocket
connection that created them, so a fresh connection sees zero tabs and every tab dies with
its connection (measured on 0.2.7).

### Commands

| Command | Purpose |
|---|---|
| `start.js` | Start the CDP server and broker |
| `stop.js` | Stop both; keeps the persisted profile |
| `nav.js <url>` | Navigate the active tab (`--new` for a new tab) |
| `eval.js '<js>'` | Evaluate; promises and top-level `await` are awaited |
| `click.js <sel>` | Real mouse click (`--text=` to match a label) |
| `type.js <sel> <text>` | Type into a field (`--keys`, `--clear`, `--submit`) |
| `read.js <url>` | One-shot fetch + extract, no server needed |
| `tabs.js` | List tabs (live URLs); `tabs.js close <id>`, `tabs.js use <id>` |
| `screenshot.js` | Viewport or `--full-page` PNG |
| `emulate.js <device>` | Mobile viewport (`--list`, `--reset`) |
| `pick.js '<intent>'` | Rank elements against a description, print selectors |
| `logs-tail.js` | Console output and uncaught exceptions (see note) |
| `net-summary.js` | Document timing and DOM resource inventory |
| `dismiss-cookies.js` | Accept or `--reject` EU consent dialogs |

## Flags that matter

```bash
./scripts/start.js --allow-private-network   # localhost/LAN (blocked by default)
./scripts/read.js http://127.0.0.1:3000 --allow-private-network   # same for one-shot reads
./scripts/start.js --reset-storage           # fresh cookies/profile
./scripts/start.js --restart                 # replace a running server
./scripts/start.js --no-stealth              # disable anti-detect (on by default)
./scripts/nav.js https://x.com --settle=500  # extra paint time after load
./scripts/emulate.js --list                  # available device presets
```

Environment: `BROWSER_DEBUG_PORT`/`OBSCURA_PORT`, `OBSCURA_BIN`, `OBSCURA_STEALTH=0`,
`OBSCURA_ALLOW_PRIVATE_NETWORK=1`, `OBSCURA_PROXY`, `OBSCURA_USER_AGENT`, `DEBUG=1`.

## Stealth is on by default, and it matters

`start.js` runs Obscura with `--stealth`. Measured against Cloudflare on this IP:
without stealth the page came back as an "Attention Required" block in 3/3 runs; with
stealth it succeeded 5/5. Stealth also changes the TLS fingerprint (JA3
`ddd4e8a…`/h2 vs `4cfcc9…`/h1). Disable it only for trusted internal hosts.

## Obscura behaviour you must not fight

These are measured facts, not guesses. Each one caused a bug in an earlier version.

- **Never wait for `Page.loadEventFired` after navigating.** Obscura's `Page.navigate`
  already resolves after `load` (with a ~2 s subresource cap), so the event has fired
  before your await returns. Waiting for it hangs forever.
- **`Page.captureScreenshot` fails above ~16.9 Mpx** with `bitmap is too large`, and a single
  capture may not exceed 32768 px in either dimension. `screenshot.js --full-page` tiles and
  stitches, so the *file* it writes can be far larger (a 300 000 px page works); do not hand-roll
  a full-page capture.
- **Every `Network.*` event is emitted twice.** Deduplicate by `requestId` before counting.
  `Network.loadingFailed` carries no `url` — join it back to `requestWillBeSent`.
- **`Log.entryAdded` never fires.** `logs-tail.js` combines two sources: a page hook for
  `console.*`, and the broker's buffer of `Runtime.exceptionThrown` for uncaught exceptions.
  Synchronous throws appear as `[exception] … (at url:line)`. Unhandled promise rejections are
  invisible to both — the engine emits nothing for them.
- **`Runtime.awaitPromise` does not exist.** `Runtime.evaluate` with `awaitPromise:true`
  awaits correctly and is what this skill uses.
- **`Emulation.setUserAgentOverride` never reaches the network** and reverts on navigation.
  Device emulation changes layout only. Set `OBSCURA_USER_AGENT` for a real UA change.
- **Clicks are viewport-relative and do not auto-scroll.** `click.js` scrolls the element to
  centre first and refuses out-of-viewport coordinates, which would silently hit nothing.
- **A page stuck in a busy loop blocks the whole connection.** Obscura runs one OS thread per
  CDP connection, so `while(true)` in *any* tab stalls every command, including ones aimed at a
  healthy tab. Measured: `eval.js` on a good tab took 8 s (its outer timeout) while another tab
  spun. `tabs.js` stays fast (175 ms) because it reports cached URLs, but real work does not.
  Recovery: `stop.js` then `start.js` (tabs are lost), or avoid unbounded loops in injected JS.
  Prefer `read.js` for one-shot reads, which never touches the shared connection.
- **`/json/list` is not a tab list.** It reports a default page that a WebSocket connection
  cannot reach (`No page`). Treat it as useless for finding tabs; the broker creates and tracks
  real ones.

## Typical flows

Read a page, then click into it:

```bash
./scripts/nav.js https://news.ycombinator.com
./scripts/eval.js '[...document.querySelectorAll(".titleline>a")].slice(0,5).map(a=>a.textContent)'
./scripts/click.js '.titleline>a' --index=0
./scripts/eval.js 'document.title'
```

Fill and submit a form:

```bash
./scripts/nav.js https://example.com/login
./scripts/type.js '#username' alice
./scripts/type.js '#password' 's3cret' --keys    # --keys for framework-bound inputs
./scripts/click.js 'button[type=submit]'
./scripts/eval.js 'document.cookie'
```

Mobile layout check:

```bash
./scripts/emulate.js iphone-14
./scripts/nav.js https://example.com
./scripts/screenshot.js --full-page
./scripts/emulate.js --reset
```

Find something when you do not know the selector:

```bash
./scripts/pick.js 'the login link'      # prints ranked selectors ready for click.js
```

## Files

`broker.js` (holds the CDP connection), `broker-client.js` (broker RPC + `evaluate`),
`obscura.js` (paths, config, port/token, process discovery), `cdp.js` (direct CDP client),
`cli.js` (flag parsing), `devices.js` + `emulation-state.js` (device presets).

Debugging: `DEBUG=1` prints per-step tracing; server output goes to
`~/.cache/agent-web/logs/serve.log`.
