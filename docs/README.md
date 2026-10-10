Obscura is an open-source headless browser engine written in Rust. It runs JavaScript via V8, speaks the Chrome DevTools Protocol, and works as a drop-in replacement for headless Chrome with Puppeteer and Playwright.

## Versus headless Chrome

| Metric      | Obscura  | Headless Chrome |
| ----------- | -------- | --------------- |
| Memory      | 30 MB    | 200+ MB         |
| Binary size | ~70 MiB  | 300+ MB         |
| Startup     | Instant  | ~2s             |
| Page load   | 85 ms    | ~500 ms         |
| Anti-detect | Built-in | None            |
| Puppeteer   | Yes      | Yes             |
| Playwright  | Yes      | Yes             |

Rendering and stealth are both first-class capabilities. Release builds
support screenshots, scroll-aware layout, activity-driven CDP screencasting,
and raster PDF export; stealth builds retain all of those surfaces while adding
the wreq/BoringSSL transport and browser-identity protections.

## Quickstart

- [Installation](Installation.md)
- [Your first fetch](Your-first-fetch.md)
- [Extract data](Extract-data.md)
- [Connect Puppeteer or Playwright](Connect-Puppeteer-or-Playwright.md)

## Guides

- [Build from source](Build-from-source.md)
- [Configure stealth and proxies](Configure-stealth-and-proxies.md)
- [Markdown extraction](Markdown-extraction.md)
- [Use with Puppeteer](Use-with-Puppeteer.md)
- [Use with Playwright](Use-with-Playwright.md)
- [Use the MCP server](Use-the-MCP-server.md)
- [Use as a Rust library](Use-as-a-Rust-library.md)
- [Persist cookies and storage](Persist-cookies-and-storage.md)
- [Intercept and modify requests](Intercept-and-modify-requests.md)
- [Run in production at scale](Run-in-production-at-scale.md)

## Reference

- [CLI reference](CLI-reference.md)
- [Environment variables](Environment-variables.md)

## Contributing

- [Architecture overview](Architecture-overview.md)
- [Adding a CDP method or Web API](Adding-a-CDP-method-or-Web-API.md)
- [Testing and debugging](Testing-and-debugging.md)

## Links

- Source: https://github.com/h4ckf0r0day/obscura
- Releases: https://github.com/h4ckf0r0day/obscura/releases
- Issues: https://github.com/h4ckf0r0day/obscura/issues

License: Apache-2.0.
