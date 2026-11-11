# Watch agent sessions live

## Watch the active MCP page

With a render-enabled build, start the stdio server with:

```sh
obscura mcp --headed
```

Open the `Obscura viewer:` URL printed on stderr, or call `browser_viewer`
through MCP to retrieve it. It displays PNG frames from the active tab in that
same MCP process, including navigation, scrolling, and agent interactions.
This is a read-only browser-based viewer, not a native desktop window. It does
not launch Chrome or another browser automatically.

Each process chooses its own available loopback port. `--viewer-port 8080`
selects a fixed port; `--viewer-ready-file /tmp/session.url` creates a file
containing the URL. The file must not already exist and is removed on graceful
stdio EOF. After a forced termination, remove a stale ready file before reusing
that path.

The viewer polls at most twice a second per visible tab. Pause stops polling,
and hidden tabs stop requesting frames. Captures happen only on request, on
the MCP page's owning thread; ordinary MCP mode starts no listener or capture
timer. Long MCP operations may delay a frame until the operation finishes.
Viewer requests cannot navigate, click, evaluate scripts, or change viewport.

The listener binds to `127.0.0.1`, requires its random per-process URL path,
checks Host and Origin, disables caching, and bounds connections and queued
frame requests. The viewer closes when the MCP process exits. `--headed` is
currently stdio-only, conflicts with `--http`, and requires rendering.

Run the self-contained [counter reproduction](../render-repros/headed/README.md)
to verify clicks, tab changes, discovery, and shutdown without an external site.

## Watch a CDP session

Capture frames through the same CDP connection that drives the page. Separate
browser connections have their own targets; opening another connection does
not let a viewer attach to an existing agent session. CLI `fetch` and stdio MCP
also run separate browser sessions.

## Run a viewer with Puppeteer

Use a render-enabled Obscura build and Node.js 22 or later:

```bash
obscura serve --port 9222
npm install puppeteer-core
```

Save this as `watch.mjs`. Put your automation where the example navigates, using
the same `page` object. The viewer polls for a JPEG twice a second and sends it
to local browser tabs over Server-Sent Events.

```js
import http from 'node:http';
import puppeteer from 'puppeteer-core';

const token = process.env.OBSCURA_CDP_TOKEN;
const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222',
  headers: token ? { Authorization: `Bearer ${token}` } : {},
});
const page = await browser.newPage();
const clients = new Set();
let latest = null;
let stopping = false;

const html = `<!doctype html>
<meta charset="utf-8"><title>Obscura live view</title>
<style>body{margin:0;background:#111}img{max-width:100%}</style>
<img id="frame" alt="Current page">
<script>
const image = document.getElementById('frame');
const events = new EventSource('/events');
events.onmessage = event => {
  image.src = 'data:image/jpeg;base64,' + event.data;
};
</script>`;

const server = http.createServer((req, res) => {
  if (req.url !== '/events') {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(html);
    return;
  }
  res.writeHead(200, {
    'Content-Type': 'text/event-stream',
    'Cache-Control': 'no-store',
    Connection: 'keep-alive',
  });
  if (latest) res.write(`data:${latest}\n\n`);
  clients.add(res);
  res.on('close', () => clients.delete(res));
});
server.listen(8080, '127.0.0.1');

async function capture() {
  try {
    latest = await page.screenshot({ type: 'jpeg', quality: 70, encoding: 'base64' });
    for (const client of clients) client.write(`data:${latest}\n\n`);
  } catch (error) {
    if (!stopping) console.error('Capture failed:', error.message);
  }
  if (!stopping) setTimeout(capture, 500);
}

process.once('SIGINT', async () => {
  stopping = true;
  for (const client of clients) client.end();
  server.close();
  await browser.disconnect();
});

await page.goto('https://example.com');
capture();
// Continue automation here with the same `page` object.
```

Run it, then open http://127.0.0.1:8080:

```bash
node watch.mjs
```

The HTTP listener binds only to loopback. If the CDP server requires a token,
export the same `OBSCURA_CDP_TOKEN` before running the script.

## Other capture paths

Playwright can use the same pattern with its existing `page.screenshot()`;
convert the returned buffer to base64 before sending it to the viewer.

For CDP screencasting, create an additional session from the existing client
connection. Handle `Page.screencastFrame`, acknowledge each frame with
`Page.screencastFrameAck`, and stop with `Page.stopScreencast`. See the
[Puppeteer](Use-with-Puppeteer.md#screencasting) and
[Playwright](Use-with-Playwright.md#screencasting) examples. Frames are emitted
when the page changes, rather than at a fixed rate.

MCP offers `browser_screenshot` for individual captures and `--headed` for its
own local viewer. It does not expose a CDP endpoint for attaching to that session.

## Verify the viewer

1. Run `watch.mjs` and open the local viewer.
2. Navigate or scroll using the script's `page` object. The viewer should update
   after the next successful capture.
3. Close and reopen the viewer tab. It should receive the most recent frame.
