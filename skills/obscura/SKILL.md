---
name: obscura
description: Use Obscura — a Rust headless browser with a Chrome DevTools Protocol server — for fast page fetches, JS execution, scraping, and CDP automation. Drop-in CDP replacement for Chrome with Puppeteer or Playwright. Trigger on requests to "open a page", "fetch a URL with JS", "scrape a site", "render this page", "automate browser via CDP", or any task where Chrome would be too heavy. Also use when the user mentions stealth fingerprinting, tracker blocking, `navigator.webdriver` masking, or evading basic bot detection.
---

# Obscura

Single-developer, open-source Rust headless browser. Boots instantly, ~70 MB binary, ~30 MB RAM at runtime, and serves a Chrome DevTools Protocol port that Puppeteer and Playwright connect to unchanged. **You swap the binary, not the code.**

Repo: https://github.com/h4ckf0r0day/obscura

## Why pick Obscura over Chrome

| | Obscura | Chrome |
|---|---|---|
| Binary | ~70 MB | ~300 MB |
| RAM | ~30 MB | ~200 MB |
| Cold start | instant | ~2 s |
| Page load (upstream claim) | ~85 ms | varies |

Field measurement on Cloudflare-protected `nairaland.com` (warm fetch): **Obscura ~4.1–4.9 s, returns real HTML body**. Real Chrome over CDP: ~5.1 s warm / 9.3 s cold. `curl`: 0.5–0.9 s but only the CF challenge interstitial.

Obscura is roughly as fast as warm Chrome, ~2× faster cold, parallelizes far better because it doesn't carry Chrome's per-process overhead, and clears Cloudflare's basic JS challenge **without** the stealth feature.

## Build

```bash
git clone https://github.com/h4ckf0r0day/obscura.git
cd obscura
CARGO_TARGET_DIR=/tmp/obscura-target cargo build -p obscura-cli --bin obscura --no-default-features
```

Resulting binary: `/tmp/obscura-target/debug/obscura`

`--no-default-features` skips the stealth build. Stealth needs `cmake` locally because it pulls `wreq` / BoringSSL.

### Stealth build

```bash
CARGO_TARGET_DIR=/tmp/obscura-target cargo build -p obscura-cli --bin obscura --features stealth
```

What stealth gives you:

- **Per-session randomized fingerprints** — GPU, canvas, audio, battery
- **3,520 tracker domains blocked** (built-in blocklist)
- **`navigator.webdriver` masked**
- **Native functions patched** so common automation detectors can't unmask them via `Function.prototype.toString` inspection
- **TLS / HTTP-2 fingerprint** matching real Chromium (defeats most JA3/JA4 + ALPN-ordering bot management)

Use stealth against: Cloudflare Turnstile non-interactive, Akamai BMP, PerimeterX, DataDome.
Stealth still won't clear: hard interactive CAPTCHAs (Turnstile interactive, hCaptcha challenge), and fingerprinters using WebGPU/WebAssembly quirks not yet patched.

## CLI fetch

```bash
/tmp/obscura-target/debug/obscura fetch https://example.com/ --dump text --quiet
```

Useful flags:
- `--dump text` — visible text only
- `--dump html` — full rendered DOM
- `--quiet` — suppress progress logs
- `--timeout <ms>` — per-page timeout

## CDP server (Puppeteer / Playwright)

```bash
/tmp/obscura-target/debug/obscura serve --port 9222
```

**Playwright:**

```ts
import { chromium } from "playwright-core";

const browser = await chromium.connectOverCDP("ws://127.0.0.1:9222");
const page = await browser.newContext().then((ctx) => ctx.newPage());
await page.goto("https://example.com/");
console.log(await page.title());
await browser.close();
```

**Puppeteer:**

```ts
import puppeteer from "puppeteer-core";

const browser = await puppeteer.connect({
  browserWSEndpoint: "ws://127.0.0.1:9222/devtools/browser",
});
const page = await browser.newPage();
await page.goto("https://example.com/");
console.log(await page.title());
await browser.disconnect();
```

## Scaling profile

- ✅ **High concurrency, low resource:** static + lightly-dynamic pages — hundreds of parallel fetches per box.
- ⚠️ **Medium:** JS-rendered SPAs, light bot protection — works but slower than raw HTTP, watch timeouts.
- ❌ **Low / unreliable:** aggressive bot defense (Turnstile interactive, Akamai BMP), real auth-walled apps, anything needing pixel-perfect rendering parity with Chrome.

## Known limits

- Not full Chrome — some browser APIs and CDP methods are incomplete relative to upstream Chromium.
- Screenshot capture is not implemented (no layout/rendering engine).
- Authenticated pages need cookie or session injection via CDP; Obscura won't run interactive logins.
- Hard CAPTCHAs (Turnstile interactive, hCaptcha) require a human or a third-party solver.

## Safety

Treat Obscura like any external Rust crate: `cargo build` runs dependency build scripts (V8, TLS). Build into a disposable target dir (`CARGO_TARGET_DIR=/tmp/obscura-target`) when evaluating new branches.
