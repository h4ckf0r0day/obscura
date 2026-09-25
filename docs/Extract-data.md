`--dump` formats the page output without writing JavaScript.

```bash
obscura fetch https://example.com --dump html
obscura fetch https://example.com --dump text
obscura fetch https://example.com --dump markdown
obscura fetch https://example.com --dump links
obscura fetch https://example.com --dump assets
obscura fetch https://example.com --dump original
obscura fetch https://example.com --dump cookies
```

## `html`

Rendered HTML after JavaScript runs. Default.

```bash
obscura fetch https://news.ycombinator.com --dump html > hn.html
```

## `text`

Plain text. No markup.

```bash
obscura fetch https://en.wikipedia.org/wiki/Rust_(programming_language) --dump text
```

## `markdown`

Markdown conversion: headings, lists, links, code blocks, images.

```bash
obscura fetch https://docs.example.com/page --dump markdown > page.md
```

## `links`

Every `<a href>` on the page, one per line.

```bash
obscura fetch https://example.com --dump links
```

## `assets`

Every external resource (stylesheets, scripts, images, fonts, iframes), plus the URLs the page requested through `fetch()`/XHR, one JSON object per line.

```bash
obscura fetch https://example.com --dump assets
```

## `original`

The raw HTML the server sent, before JavaScript ran.

```bash
obscura fetch https://my-spa.example --dump original > before.html
obscura fetch https://my-spa.example --dump html     > after.html
diff before.html after.html
```

## `cookies`

Every cookie in the jar as a JSON array, including HttpOnly cookies that `document.cookie` cannot see. Useful for capturing session tokens set by anti-bot challenges.

```bash
obscura fetch https://example.com --dump cookies
```

## With `--wait-until`

`--dump` runs after the wait condition:

```bash
obscura fetch https://my-spa.example --wait-until load --dump markdown
```

## Pipe and redirect

```bash
obscura fetch https://example.com --dump markdown > example.md
obscura fetch https://example.com --dump text --quiet | wc -w
```
