---
name: obscura
description: Fetch web pages, extract content, and run multi-step scraping pipelines using the Obscura headless browser CLI. Trigger with /obscura or when asked to scrape, fetch, or collect web content.
---

Use Obscura CLI tools to fetch web content and extract data for the user.

## When to trigger

- User says: "fetch this URL", "scrape this page", "extract content from..."
- User shares a URL and asks for content, data, or summary
- User asks to collect data from multiple pages

## Tools

| Command | Purpose |
|---------|---------|
| `obscura fetch` | Single URL — read content, extract data |
| `obscura scrape` | Multiple URLs — parallel batch collection |

## Quick reference

```bash
# Read page as clean text
obscura fetch <url> --quiet --dump text

# Extract structured data (JS eval)
obscura fetch <url> --quiet --eval "JSON.stringify({title: document.title, ...})"

# Extract all links
obscura fetch <url> --quiet --dump links

# Wait for dynamic/SPA content
obscura fetch <url> --quiet --selector "#content" --dump text

# Bot-protected page
obscura fetch <url> --quiet --stealth --dump text

# Parallel batch
obscura scrape <url1> <url2> <url3> --eval "document.title" --format json

# Polite scraping (rate-limited)
obscura scrape <url1> <url2> --concurrency 2 --format json
```

## Multi-step pipeline

When URLs aren't known upfront — discover then collect:

```bash
# Step 1: discover links
obscura fetch <index-url> --quiet --dump links

# Step 2: scrape discovered URLs
obscura scrape <url1> <url2> ... --eval "<extraction>" --format json
```

## Limitations — stop and tell the user when:

- Login required → "Use Playwright or Browser-use instead"
- CAPTCHA → "Cannot proceed — Obscura cannot solve CAPTCHAs"
- Click / form input needed → "Obscura is read-only"

## Error recovery

| Error | Try |
|-------|-----|
| 403 / empty body | Add `--stealth` |
| Content missing (SPA) | Add `--wait-until networkidle0` or `--selector` |
| Rate limited (429) | Lower `--concurrency` to 2, batch URLs |
| Binary not found | `OBSCURA_BIN=/path/to/obscura obscura-mcp serve` |
