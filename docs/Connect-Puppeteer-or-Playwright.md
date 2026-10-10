Obscura speaks the Chrome DevTools Protocol over WebSocket. Puppeteer and
Playwright can connect to its CDP endpoint for the supported workflows below.

## Start the server

```bash
obscura serve --port 9222
```

```
obscura listening on ws://127.0.0.1:9222
```

## Puppeteer

```bash
npm install puppeteer-core
```

```js
const puppeteer = require('puppeteer-core');

const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222',
});

const page = await browser.newPage();
await page.goto('https://example.com');
console.log(await page.title()); // "Example Domain"

await browser.disconnect();
```

Use `puppeteer-core`, not `puppeteer`. The `puppeteer` package bundles a Chrome download.

## Playwright

```bash
npm install playwright
```

```js
const { chromium } = require('playwright');

const browser = await chromium.connectOverCDP('ws://127.0.0.1:9222');
const context = browser.contexts()[0] || await browser.newContext();
const page = await context.newPage();

await page.goto('https://example.com');
console.log(await page.title());

await browser.close();
```

Use `connectOverCDP`, not `connect`. Playwright's `connect` speaks Playwright's own protocol, which obscura does not implement.

## `waitUntil`

Default is `domcontentloaded`. For full subresource load:

```js
await page.goto('https://example.com', { waitUntil: 'load' });
```

| Value              | Returns when                            |
| ------------------ | --------------------------------------- |
| `domcontentloaded` | HTML parsed, scripts ran (default)      |
| `load`             | All subresources finished               |
| `networkidle2`     | ≤2 network connections active for 500ms |
| `networkidle0`     | 0 network connections active for 500ms  |

## Supported

- `page.goto`, `page.reload`, `page.goBack`, `page.goForward`
- `page.evaluate`, `page.evaluateHandle`
- `page.click`, `page.type`, `page.fill`, `page.focus`
- `page.waitForSelector`, `page.waitForFunction`, `page.waitForNavigation`
- `page.cookies`, `page.setCookie`, `context.cookies`
- `page.setRequestInterception`, block / modify
- `page.exposeFunction`
- `page.content`, `page.title`, `page.url`
- `page.screenshot` for viewport, clipped, and full-page capture
- `page.pdf` for raster-backed print output
- raw CDP `Page.startScreencast` with frame acknowledgements (`page.createCDPSession()`
  in Puppeteer; `context.newCDPSession(page)` in Playwright)

DOM-agent frameworks such as browser-use also connect: obscura implements `DOMSnapshot.captureSnapshot` and `Target.targetInfoChanged` for perception, and `DOM.focus` so a focused field receives `Input.dispatchKeyEvent` keystrokes.

## Capture example

```js
await page.setViewport({ width: 1440, height: 1000 });
await page.screenshot({ path: 'viewport.png' });
await page.screenshot({ path: 'full-page.png', fullPage: true });
await page.pdf({ path: 'page.pdf', format: 'A4', printBackground: true });
```

Rendering is included in official binaries and requires `--features render`
for source builds. The client-specific guides cover scrolling, raw CDP
screencasting, and current output limits.

## Current limits

- Pages share one V8 isolate. CPU-bound JavaScript on one page can delay others.
- PDF output is raster-backed; text is not selectable and tagged PDF,
  headers/footers, outlines, and full CSS paged media are not implemented.
- Service workers, native media playback, some Web APIs, and long-tail CSS or
  compositor effects are still incomplete relative to Chromium.
