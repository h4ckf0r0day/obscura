# Watch agent sessions live

## Native application window

Build desktop support explicitly, then start a stdio MCP session:

```sh
cargo build --release -p obscura-cli --bins --features headed
obscura mcp --headed
```

This opens an Obscura application window. It displays raw pixels from the same
active page used by MCP, and forwards mouse buttons, pointer movement, keyboard
input, committed IME text, scrolling, and resize to that page. It launches no
external browser and starts no HTTP listener. `browser_viewer` returns native
window metadata (`mode` and `pid`) instead of a URL.

The desktop window runs on the main thread of a private child process of the
same executable. V8 stays on its owning MCP thread. Frame requests and input use
bounded private pipes; the native window reuses CDP's page-input implementation.
There is at most one requested frame in flight, at up to 15 captures per second.
Occluded/minimized windows stop requesting frames. The software surface handles
OS display scaling without changing the page's CSS pixel ratio.

Closing the window leaves MCP automation running without a window. Stdio EOF or
the parent process exiting closes the child. A missing desktop display is a
startup error, rather than silently switching to a web viewer. On Linux, a
Wayland or X11 session is required; WSLg provides these displays on Windows.
X11 also needs the runtime `libxkbcommon-x11` and `libxcb-xkb` libraries. The
`headed` feature is optional, so normal render/no-render builds have no desktop
dependencies and can still run on display-less servers.

This is an initial desktop surface, not a full browser shell. It has no address
bar, tab strip, downloads UI, clipboard integration, or IME preedit UI. Agents
navigate and switch tabs through MCP. A long MCP operation delays frames and
input until that operation finishes. Native output currently uses one raster
pixel per CSS pixel, scaled to the window's physical size.

See the [minimal reproduction](../render-repros/headed/README.md) for automated
Linux/X11 checks of real OS input, native window pixels, resize, and cleanup.
macOS and Windows desktop backends are provided by the windowing libraries;
only Linux/WSLg is validated here.

## Read-only web viewer

Render builds retain a separate browser-based viewer:

```sh
obscura mcp --web-viewer
```

Open the `Obscura viewer:` URL printed on stderr, or call `browser_viewer`
through MCP to retrieve it. It follows the active tab in that same MCP process.
Each process chooses its own available loopback port. `--viewer-port 8080`
selects a fixed port; `--viewer-ready-file /tmp/session.url` creates a file
containing the URL. The file must not already exist and is removed on graceful
stdio EOF. After forced termination, remove a stale file before reusing it.

The viewer polls at up to twice a second per visible tab. Pause and hidden tabs
stop requesting frames. Its listener binds to `127.0.0.1`, requires a random
per-process URL path, checks Host and Origin, disables caching, and bounds
connections and queued work. It exposes no interaction/evaluation endpoint.

Both display modes are stdio-only and mutually exclusive. `--web-viewer`
requires rendering; `--headed` additionally requires the desktop build feature.

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

MCP offers `browser_screenshot` for individual captures, `--headed` for a native
window, and `--web-viewer` for a read-only local viewer. It does not expose a CDP endpoint for attaching to that session.

## Verify the viewer

1. Run `watch.mjs` and open the local viewer.
2. Navigate or scroll using the script's `page` object. The viewer should update
   after the next successful capture.
3. Close and reopen the viewer tab. It should receive the most recent frame.
