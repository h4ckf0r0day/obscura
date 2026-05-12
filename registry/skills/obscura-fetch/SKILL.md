---
name: obscura-fetch
description: Fetch a single web page using Obscura headless browser. Best for public, read-only pages. Returns HTML, plain text, links, or JS-evaluated results.
---

Fetch a URL with Obscura and return the content. Use this for single-page read operations — documentation, articles, public data pages.

## Usage
```
/obscura-fetch <url> [--dump html|text|links] [--eval <js>] [--selector <css>] [--stealth]
```

## Decision: which flag to use

| Goal | Flag |
|------|------|
| Read article / docs | `--dump text` |
| Extract links | `--dump links` |
| Get raw HTML | `--dump html` (default) |
| Extract structured data | `--eval "JSON.stringify(...)"` |
| Wait for dynamic content | `--selector <css>` or `--wait-until networkidle0` |
| Bot-detection suspected | `--stealth` |

## Instructions

1. Parse URL and options from user input.
2. Run the command:
   ```bash
   obscura fetch <url> --quiet [options]
   ```
3. If output > 5000 chars, summarize key content — don't dump raw.
4. If exit code ≠ 0, report the error and suggest `--stealth` or `--wait-until networkidle0`.

## Known limitations

- No session/cookie persistence across calls
- No click, form input, or navigation
- Login-required pages → not supported (suggest Playwright instead)

## Examples

```bash
# Read article as clean text
obscura fetch https://example.com --quiet --dump text

# Extract page title
obscura fetch https://example.com --quiet --eval "document.title"

# Extract all links
obscura fetch https://example.com --quiet --dump links

# Structured data extraction
obscura fetch https://news.ycombinator.com --quiet \
  --eval "JSON.stringify(Array.from(document.querySelectorAll('.titleline > a')).map(a => ({title: a.textContent, url: a.href})))"

# Wait for SPA content
obscura fetch https://example.com --quiet --selector "#main-content" --dump text

# Stealth mode for bot-protected pages
obscura fetch https://example.com --quiet --stealth --dump text
```
