# Native downloads and transport diagnostics over CDP

These Obscura extensions let remote automation clients export file bytes
without using a separate Node HTTP client or a server filesystem path.
Build with `--features render,stealth` and run `obscura serve --stealth`.
They are extensions to CDP, not Chrome methods or browser download events.

`Obscura.getTransportInfo` takes no parameters. It reports `stealthCompiled`,
`stealthActive`, `transport`, `userAgent`, `platform`, `proxyConfigured`,
`privateNetworkAllowed`, and `nativeDownload`. It returns no proxy credentials.
The flags describe the engine's configuration, not a guarantee of invisibility.

`Obscura.download` takes an HTTP(S) `url`, optional `maxBytes` (1–33554432),
and optional `timeoutMs` (1–60000). It requires compiled and active stealth.
It uses wreq/BoringSSL, the selected context's cookie jar, configured proxy,
redirect handling, SSRF checks, and decoded response-size bounds. It does
not parse the response as a page or execute downloaded scripts. On a page
session it uses that page's context; a browser session uses its connection's
default context.

The result contains `url`, `status`, `headers`, `bytes`, and an IO `stream`
handle. Check the HTTP status and expected file type, read the body with
`IO.read`, then call `IO.close` even on validation failure. Native downloads
are buffered under the byte limit before the stream is returned. CDP reads
are chunked; the existing per-connection stream store caps retained bytes
and entries. Closing the CDP connection releases its remaining streams.
Clients are responsible for their own exported-file expiry and access policy.

Example with Playwright connected to Obscura:

```js
const session = await browser.newBrowserCDPSession();
let stream;
try {
  console.log(await session.send('Obscura.getTransportInfo'));
  const result = await session.send('Obscura.download', {
    url: 'https://example.org/document.pdf', maxBytes: 8 * 1024 * 1024,
  });
  stream = result.stream;
  if (result.status !== 200) throw new Error(`HTTP ${result.status}`);
  for (;;) {
    const chunk = await session.send('IO.read', { handle: stream, size: 1024 * 1024 });
    // Write decoded bytes to a destination owned by this client.
    consume(Buffer.from(chunk.data, chunk.base64Encoded ? 'base64' : 'utf8'));
    if (chunk.eof) break;
  }
} finally {
  if (stream) await session.send('IO.close', { handle: stream });
  await session.detach();
}
```

This operation supports GET with cookies. It does not replay POST exports,
extract localStorage bearer tokens, download blob URLs, or serve existing
server files. Playwright `APIRequestContext` requests run in Node and do not
inherit this native transport. Page `fetch()`/XHR uses the engine instead.
