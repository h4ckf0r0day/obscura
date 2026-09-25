`--dump markdown` converts the rendered page to markdown.

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
- Tracking pixels and beacons

## Save to file

```bash
obscura fetch https://docs.example.com/page --dump markdown -o page.md
```

## For RAG / LLM context

```bash
obscura fetch https://docs.example.com/page --dump markdown --quiet
```

`--quiet` strips info logging so the output is just markdown.

## Wait for SPA content

For pages that render content client-side:

```bash
obscura fetch https://my-spa.example --wait-until load --dump markdown
```

## Narrow to a region

`--selector` restricts the conversion to a CSS selector:

```bash
obscura fetch https://example.com --selector "main" --dump markdown
obscura fetch https://example.com --selector "article.post" --dump markdown
```

Useful for skipping nav, sidebars, and footers.
