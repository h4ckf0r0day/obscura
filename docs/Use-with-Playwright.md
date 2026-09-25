## Setup

```bash
obscura serve --port 9222
npm install playwright
```

## Connect

```js
const { chromium } = require('playwright');

const browser = await chromium.connectOverCDP('ws://127.0.0.1:9222');
const context = browser.contexts()[0] || await browser.newContext();
const page = await context.newPage();
```

Use `connectOverCDP`, not `connect`. Playwright's `connect` speaks Playwright's own protocol.

## Navigate

```js
await page.goto('https://example.com');
await page.goto('https://example.com', { waitUntil: 'load' });
await page.goto('https://example.com', { waitUntil: 'networkidle' });
```

Default is `domcontentloaded`. Other values: `load`, `networkidle`.

## Evaluate

```js
const title = await page.evaluate(() => document.title);

const items = await page.$$eval('.item', els => els.map(el => ({
  text: el.textContent,
  href: el.querySelector('a')?.href,
})));
```

## Interact

```js
await page.click('#login-button');
await page.fill('#username', 'alice');
await page.fill('#password', 'secret');

await page.waitForSelector('#dashboard');
await page.waitForFunction(() => window.appReady === true);
```

## Locators

```js
await page.locator('button.submit').click();
await page.getByRole('button', { name: 'Submit' }).click();
await page.getByLabel('Email').fill('alice@example.com');
```

## Cookies

```js
await context.addCookies([{
  name: 'session',
  value: 'abc123',
  domain: 'example.com',
  path: '/',
}]);

const cookies = await context.cookies();
```

## Intercept requests

```js
await page.route('**/*', route => {
  if (route.request().resourceType() === 'image') {
    route.abort();
  } else {
    route.continue();
  }
});
```

## Multiple pages

```js
const page1 = await context.newPage();
const page2 = await context.newPage();

await Promise.all([
  page1.goto('https://a.example.com'),
  page2.goto('https://b.example.com'),
]);
```

Pages share one V8 isolate. CPU-bound JS on one page blocks the others.

## Screenshots, scrolling, and PDF

```js
await page.setViewportSize({ width: 1440, height: 1000 });
await page.screenshot({ path: 'viewport.png' });

await page.evaluate(() => window.scrollTo(0, 1200));
await page.screenshot({ path: 'scrolled.png' });

await page.screenshot({ path: 'full-page.png', fullPage: true });
await page.pdf({ path: 'page.pdf', format: 'A4', printBackground: true });
```

A normal screenshot captures the live viewport and scroll position;
`fullPage: true` captures document space. PDF output is raster-backed.

## Screencasting

Playwright does not expose CDP screencasting as a page method. Attach a raw CDP
session to the page, acknowledge every frame, and detach it when finished:

```js
const client = await context.newCDPSession(page);

client.on('Page.screencastFrame', async ({ data, sessionId }) => {
  const jpeg = Buffer.from(data, 'base64');
  // Consume or forward `jpeg` here.
  await client.send('Page.screencastFrameAck', { sessionId });
});

await client.send('Page.startScreencast', {
  format: 'jpeg',
  quality: 80,
  maxWidth: 1280,
  maxHeight: 720,
});

// ...navigate, scroll, and interact...

await client.send('Page.stopScreencast');
await client.detach();
```

Frames are activity-driven page captures, not fixed-rate desktop video.

## Disconnect

```js
await browser.close();  // closes the CDP connection, leaves obscura serve running
```

## Current limits

- Playwright `page.video()` and tracing artifacts that require desktop capture
  are not implemented. Use the raw CDP flow above for page frames.
- `BrowserContext` storage-state save/restore remains limited; use
  `--storage-dir` on `obscura serve`, as described in
  [Persist cookies and storage](Persist-cookies-and-storage.md).
- Service workers, native media, some Web APIs, long-tail CSS, and compositor
  behavior remain incomplete relative to Chromium.
- PDF text is not selectable/searchable and tagged PDF is not yet available.
