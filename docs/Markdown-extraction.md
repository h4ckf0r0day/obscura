`--dump markdown` converts the rendered page to markdown.

Extraction switches to an iterative tree walk for deep subtrees, so deeply
nested DOMs do not exhaust the JavaScript call stack. Nested table cells are
converted once and reused when assembling their rows.

```bash
obscura fetch https://example.com --dump markdown
```

## What gets converted

- Headings (`<h1>` through `<h6>`)
- Paragraphs, line breaks
- Bold, italic, code spans
- Links (with `href`)
- Images (with `src` and `alt`)
- Ordered and unordered lists
- Block quotes
- Code blocks (`<pre>`, `<code>`)
- Tables

## What gets stripped

- `<script>`, `<style>`, `<noscript>`
- Inline styles
- ARIA attributes

Images are converted without checking their dimensions or purpose. Tracking
pixel URLs can remain in the output.

## Save to file

```bash
obscura fetch https://docs.example.com/page --dump markdown -o page.md
```

## For RAG / LLM context

```bash
obscura fetch https://docs.example.com/page --dump markdown --quiet
```

`--quiet` suppresses logs. Progress messages and logs use stderr; Markdown uses
stdout. `--verbose` or `RUST_LOG` can override the log filter.

## Wait for SPA content

For pages that render content client-side:

```bash
obscura fetch https://my-spa.example --wait-until load --dump markdown
```

## Wait for an element

`--selector` waits for an element before reading the page. It does not restrict
Markdown conversion to that element:

```bash
obscura fetch https://example.com --selector "main" --dump markdown
obscura fetch https://example.com --selector "article.post" --dump markdown
```

To extract a single region, use `--eval` to read its text or HTML, or use the
[structured extraction tools](Use-the-MCP-server.md#tools-exposed) in MCP.
