<p align="center">
  <img src="https://raw.githubusercontent.com/h4ckf0r0day/obscura/main/assets/icon.png" alt="Obscura" width="80" />
</p>

<h2 align="center">Obscura</h2>

<p align="center">
  <strong>The open-source headless browser for AI agents and web scraping.</strong><br>
  Lightweight, stealthy, and built in Rust.
</p>

---

Obscura is a headless browser engine written in Rust, built for web scraping and AI agent automation. It runs real JavaScript via V8, supports the Chrome DevTools Protocol, and acts as a drop-in replacement for headless Chrome with Puppeteer and Playwright.

### Why Obscura over headless Chrome?

Designed for automation at scale, not desktop browsing.

| Metric       | Obscura      | Headless Chrome |
|--------------|--------------|------------------|
| Memory       | **30 MB**    | 200+ MB          |
| Binary size  | **70 MB**    | 300+ MB          |
| Anti-detect  | **Built-in** | None          |
| Page load    | **85 ms**    | ~500 ms          |
| Startup      | **Instant**  | ~2s              |
| Puppeteer    | **Yes**      | Yes              |
| Playwright   | **Yes**      | Yes              |

## 🎉 10,000 stars and what's next

I'm working on **Obscura Cloud** the hosted version, with managed infrastructure, residential proxies, and dedicated support. For people who want the engine without operating it themselves.

The open-source engine stays Apache-2.0, fully featured. No feature gating, ever.

**[Get on the waitlist →](https://tally.so/r/gDWzdD)**

## Install

### Download

Grab the latest binary from [Releases](https://github.com/h4ckf0r0day/obscura/releases):

```bash
# Linux x86_64
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-linux.tar.gz
tar xzf obscura-x86_64-linux.tar.gz
./obscura fetch https://example.com --eval "document.title"

# Arch Linux (AUR)
yay -S obscura-browser

# macOS Apple Silicon
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-aarch64-macos.tar.gz
tar xzf obscura-aarch64-macos.tar.gz

# macOS Intel
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-macos.tar.gz
tar xzf obscura-x86_64-macos.tar.gz

# Windows
Download the `.zip` from the releases page and extract it manually.
```

No Chrome, no Node.js, no dependencies. Release archives include both
`obscura` and `obscura-worker`; keep them in the same directory for the
parallel `scrape` command.

Linux release builds target Ubuntu 22.04 so the downloaded binary remains
usable on common LTS servers with glibc 2.35+.

### Build from source

```bash
git clone https://github.com/h4ckf0r0day/obscura.git
cd obscura
cargo build --release

# With stealth mode (anti-detection + tracker blocking)
cargo build --release --features stealth
```

Requires Rust 1.75+ ([rustup.rs](https://rustup.rs)). First build takes ~5 min (V8 compiles from source, cached after).

## Quick Start

### Fetch a page

```bash
# Get the page title
obscura fetch https://example.com --eval "document.title"

# Extract all links
obscura fetch https://example.com --dump links

# Render JavaScript and dump HTML
obscura fetch https://news.ycombinator.com --dump html

# Write dump or eval output to a file
obscura fetch https://example.com --dump text --output page.txt

# Wait for dynamic content
obscura fetch https://example.com --wait-until networkidle0

# Bound navigation time for slow or broken pages
obscura fetch https://example.com --timeout 10
```

### Start the CDP server

```bash
obscura serve --port 9222

# With stealth mode (anti-detection + tracker blocking)
obscura serve --port 9222 --stealth
```

### Scrape in parallel

```bash
obscura scrape url1 url2 url3 ... \
  --concurrency 25 \
  --eval "document.querySelector('h1').textContent" \
  --format json

# Suppress scrape progress on stderr for script-friendly output
obscura scrape https://example.com --quiet --format json
```

## Puppeteer / Playwright

### Puppeteer

```bash
npm install puppeteer-core
```

```javascript
import puppeteer from 'puppeteer-core';

const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222/devtools/browser',
});

const page = await browser.newPage();
await page.goto('https://news.ycombinator.com');

const stories = await page.evaluate(() =>
  Array.from(document.querySelectorAll('.titleline > a'))
    .map(a => ({ title: a.textContent, url: a.href }))
);
console.log(stories);

await browser.disconnect();
```

### Playwright

```bash
npm install playwright-core
```

```javascript
import { chromium } from 'playwright-core';

const browser = await chromium.connectOverCDP({
  endpointURL: 'ws://127.0.0.1:9222',
});

const page = await browser.newContext().then(ctx => ctx.newPage());
await page.goto('https://en.wikipedia.org/wiki/Web_scraping');
console.log(await page.title());

await browser.close();
```

### Form submission & login

```javascript
await page.goto('https://quotes.toscrape.com/login');
await page.evaluate(() => {
  document.querySelector('#username').value = 'admin';
  document.querySelector('#password').value = 'admin';
  document.querySelector('form').submit();
});
// Obscura handles the POST, follows the 302 redirect, maintains cookies
```

## Benchmarks

Page load:

| Page | Obscura | Chrome |
|------|---------|--------|
| Static HTML | **51 ms** | ~500 ms |
| JS + XHR + fetch | **84 ms** | ~800 ms |
| Dynamic scripts | **78 ms** | ~700 ms |

## Stealth Mode

Enable with `--features stealth`.

### Anti-fingerprinting
- Per-session fingerprint randomization (GPU, screen, canvas, audio, battery)
- Realistic `navigator.userAgentData` (Chrome 145, high-entropy values)
- `event.isTrusted = true` for dispatched events
- Hidden internal properties (`Object.keys(window)` safe)
- Native function masking (`Function.prototype.toString()` → `[native code]`)
- `navigator.webdriver = undefined` (matches real Chrome)

### Tracker Blocking
- 3,520 domains blocked
- Blocks analytics, ads, telemetry, and fingerprinting scripts
- Prevents trackers from loading entirely
- Enabled automatically with `--stealth`

## CDP API

Obscura implements the Chrome DevTools Protocol for Puppeteer/Playwright compatibility.

| Domain | Methods |
|--------|---------|
| **Target** | createTarget, closeTarget, attachToTarget, createBrowserContext, disposeBrowserContext |
| **Page** | navigate, getFrameTree, addScriptToEvaluateOnNewDocument, lifecycleEvents |
| **Runtime** | evaluate, callFunctionOn, getProperties, addBinding |
| **DOM** | getDocument, querySelector, querySelectorAll, getOuterHTML, resolveNode |
| **Network** | enable, setCookies, getCookies, setExtraHTTPHeaders, setUserAgentOverride |
| **Fetch** | enable, continueRequest, fulfillRequest, failRequest (live interception) |
| **Storage** | getCookies, setCookies, deleteCookies |
| **Input** | dispatchMouseEvent, dispatchKeyEvent |
| **LP** | getMarkdown (DOM-to-Markdown conversion) |
## CLI Reference

### `obscura serve`

Start a CDP WebSocket server.

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `9222` | WebSocket port |
| `--proxy` | — | HTTP/SOCKS5 proxy URL |
| `--stealth` | off | Enable anti-detection + tracker blocking |
| `--workers` | `1` | Number of parallel worker processes |
| `--obey-robots` | off | Respect robots.txt |

### `obscura fetch <URL>`

Fetch and render a single page.

| Flag | Default | Description |
|------|---------|-------------|
| `--dump` | `html` | Output: `html`, `text`, or `links` |
| `--eval` | — | JavaScript expression to evaluate |
| `--wait-until` | `load` | Wait: `load`, `domcontentloaded`, `networkidle0` |
| `--timeout` | `30` | Maximum navigation time in seconds |
| `--selector` | — | Wait for CSS selector |
| `--stealth` | off | Anti-detection mode |
| `--output` | — | Write dump or eval output to a file |
| `--quiet` | off | Suppress banner |

### `obscura scrape <URL...>`

Scrape multiple URLs in parallel with worker processes.

| Flag | Default | Description |
|------|---------|-------------|
| `--concurrency` | `10` | Parallel workers |
| `--eval` | — | JS expression per page |
| `--format` | `json` | Output: `json` or `text` |
| `--quiet` | off | Suppress scrape progress on stderr |

## License

Apache 2.0

---
