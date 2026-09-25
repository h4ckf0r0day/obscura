## Setup

```bash
obscura serve --port 9222
npm install puppeteer-core
```

## Connect

```js
const puppeteer = require('puppeteer-core');

const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222',
});
```

Use `puppeteer-core`, not `puppeteer`. The `puppeteer` package bundles a Chrome download.

## Navigate

```js
const page = await browser.newPage();
await page.goto('https://example.com');
await page.goto('https://example.com', { waitUntil: 'load' });
await page.goto('https://example.com', { waitUntil: 'networkidle0', timeout: 60000 });
```

Default `waitUntil` is `domcontentloaded`. Other values: `load`, `networkidle2`, `networkidle0`.

## Evaluate

```js
const title = await page.evaluate(() => document.title);

const items = await page.evaluate(() => {
  return Array.from(document.querySelectorAll('.item')).map(el => ({
    text: el.textContent,
    href: el.querySelector('a')?.href,
  }));
});
```

## Interact

```js
await page.click('#login-button');
await page.type('#username', 'alice');
await page.fill('#password', 'secret');  // alias of .type for compat

await page.waitForSelector('#dashboard');
await page.waitForFunction(() => window.appReady === true);
```

## Cookies

```js
await page.setCookie({
  name: 'session',
  value: 'abc123',
  domain: 'example.com',
  path: '/',
  httpOnly: true,
  secure: true,
});

const cookies = await page.cookies();
```

For session persistence across runs see [Persist cookies and storage](Persist-cookies-and-storage.md).

## Intercept requests

```js
await page.setRequestInterception(true);

page.on('request', req => {
  if (req.resourceType() === 'image') {
    req.abort();
  } else {
    req.continue();
  }
});
```

See [Intercept and modify requests](Intercept-and-modify-requests.md).

## Expose a Node callback

```js
await page.exposeFunction('logFromPage', (msg) => {
  console.log('page:', msg);
});

await page.evaluate(() => {
  window.logFromPage('hello from the browser');
});
```

## Multiple pages

```js
const page1 = await browser.newPage();
const page2 = await browser.newPage();

await Promise.all([
  page1.goto('https://a.example.com'),
  page2.goto('https://b.example.com'),
]);
```

Pages share one V8 isolate. Concurrent JS execution serializes through a lock. CPU-bound JS on one page blocks the others.

## Screenshots, scrolling, and PDF

```js
await page.setViewport({ width: 1440, height: 1000, deviceScaleFactor: 1 });
await page.screenshot({ path: 'viewport.png' });

await page.evaluate(() => window.scrollTo(0, 1200));
await page.screenshot({ path: 'scrolled.png' });

await page.screenshot({ path: 'full-page.png', fullPage: true });
await page.pdf({ path: 'page.pdf', format: 'A4', printBackground: true });
```

A normal screenshot captures the live viewport and scroll position;
`fullPage: true` captures document space. PDF output is raster-backed.

## Screencasting

Attach a raw CDP session to the page, acknowledge every frame, and detach it
when finished:

```js
const client = await page.createCDPSession();

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
await browser.disconnect();  // leaves obscura serve running
```

## Current limits

- Some device emulation, service-worker, native media, long-tail CSS, and
  compositor behavior remains incomplete relative to Chromium.
- Pages share one V8 isolate; CPU-bound JavaScript serializes across pages.
- PDF text is not selectable/searchable and tagged PDF is not yet available.
