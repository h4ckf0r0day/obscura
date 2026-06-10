Obscura speaks the Chrome DevTools Protocol over WebSocket. Puppeteer and Playwright connect to it like remote Chrome.

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

DOM-agent frameworks such as browser-use also connect: obscura implements `DOMSnapshot.captureSnapshot` and `Target.targetInfoChanged` for perception, and `DOM.focus` so a focused field receives `Input.dispatchKeyEvent` keystrokes.

## Not supported

- `page.screenshot`: obscura doesn't render pixels.
- Per-page V8 isolation: pages share one V8 isolate. A heavy script on one page can stall others.
- Cloudflare / Datadome / Akamai bypass: see [Configure stealth and proxies](Configure-stealth-and-proxies.md).
