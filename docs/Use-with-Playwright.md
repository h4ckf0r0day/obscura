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

## SVG element interfaces

SVG nodes created with `document.createElementNS` or parsed from SVG markup
expose their corresponding interfaces, such as `SVGImageElement` and
`SVGUseElement`, with prototype ancestry from the [SVG IDL](https://svgwg.org/svg2-draft/idl.html)
and [Filter Effects IDL](https://www.w3.org/TR/filter-effects-1/#svg-interfaces). This supports interface
checks in page scripts; it does not imply support for every SVG property,
method or rendering feature.

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

## JavaScript heap measurements

```js
const client = await context.newCDPSession(page);
await client.send('HeapProfiler.collectGarbage'); // Optional explicit collection.
const heap = await client.send('Runtime.getHeapUsage');
console.log(heap.usedSize, heap.totalSize, heap.backingStorageSize);
await client.detach();
```

These are native V8 isolate byte counts, not process memory. Reading them does
not run page tasks or force collection. `embedderHeapUsedSize` is zero because
Obscura's Rust DOM is not a V8-managed cppgc heap; Rust allocations and renderer
memory are not included.

## Application timing

Page scripts can record `performance.mark()` and `performance.measure()`
entries, retrieve them through the timeline getters, and observe them through
`PerformanceObserver`, including buffered delivery. Measurement options accept
named marks or numeric timestamps; `detail` is structured-cloned.

`PerformanceObserver.supportedEntryTypes` reports `mark` and `measure` only.
Navigation, resource, paint, and long-task entries are not emitted. User Timing
entries remain available until cleared or the realm is destroyed; applications
that continuously record timings should call `clearMarks()` and `clearMeasures()`.
Legacy `performance.timing` records main-document navigation start, fetch start,
response completion and DOM/load milestones. Event-end values remain zero until
their handlers finish. These read-only, integer epoch timestamps use a native
monotonic clock and share the real origin with `performance.timeOrigin`; they
survive runtime suspension and are not replaced by synthetic events or
`document.open()`/`close()`. Named User Timing measures can use these milestones.
Transport response completion is observed when the buffered fetch returns.
DNS, connection, TLS, first-byte, redirect and previous-document unload timings
remain unavailable rather than fabricated. Child realms record their own DOM
and load milestones, but do not yet receive the fetch's start/completion metadata.
This is partial legacy timing support, not Navigation Timing conformance.

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

## Fetch cancellation

Page `fetch()` honors an `AbortSignal` passed directly or inherited from a
`Request`. Pre-aborted calls reject with the signal's reason without sending a
request. Aborting an active fetch cancels native transport work, including a
pending response body, and releases the page's network-readiness counters.
Cancelled requests report `Network.loadingFailed` rather than a successful
`Network.loadingFinished`; the Rust network event exposes `error_text` for
failed requests. Explicit `signal: null` overrides an inherited signal.

Aborting also errors a fully buffered but unread response body. A drained or
cancelled body stays closed. `Request` and its clones expose distinct following
signals; dependent signals are marked aborted before the parent's abort event,
then receive their own events afterward. Cancellation uses internal abort
algorithms, not overridable public event methods. Dependency links and internal
body observers use weak references; signals with live abort listeners remain
reachable.

Cloned intercepted responses follow cancellation even when their bodies are
already buffered and have no native transport resource. Browser-generated abort
events are trusted; dispatching an author-created event does not abort a fetch.

Response bodies remain capped, buffered one-chunk bodies. Cancellation does
not introduce incremental network stream delivery or backpressure support.
`Response.clone()` currently shares buffered bytes rather than a byte-stream
tee. Close/error timing can differ when the last chunk is consumed inside an
early parent abort listener, before dependent abort steps run.

## Current limits

- `document.queryCommandSupported()` reports editing commands as unsupported.
  `execCommand()` does not perform clipboard or rich-text editing operations.
  Capability detection allows editor libraries to initialize; it is not full
  editor interaction support.
- Playwright `page.video()` and tracing artifacts that require desktop capture
  are not implemented. Use the raw CDP flow above for page frames.
- `BrowserContext` storage-state save/restore remains limited; use
  `--storage-dir` on `obscura serve`, as described in
  [Persist cookies and storage](Persist-cookies-and-storage.md).
- Service workers, native media, some Web APIs, long-tail CSS, and compositor
  behavior remain incomplete relative to Chromium.
- PDF text is not selectable/searchable and tagged PDF is not yet available.
