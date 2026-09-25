`obscura fetch` loads a URL, runs its JavaScript, and prints the result.

## Load a page

```bash
obscura fetch https://example.com
```

Prints the rendered HTML.

## Run JavaScript with `--eval`

```bash
obscura fetch https://example.com --eval "document.title"
```

```
"Example Domain"
```

Returns JSON:

```bash
obscura fetch https://news.ycombinator.com \
  --eval "Array.from(document.querySelectorAll('.titleline a')).slice(0, 5).map(a => a.textContent)"
```

## Multi-statement eval

`--eval` evaluates one expression. For multiple statements, wrap in an IIFE:

```bash
obscura fetch https://example.com --eval "(function(){
  const links = document.querySelectorAll('a');
  return Array.from(links).map(a => a.href);
})()"
```

A bare block starting with `const` or `let` returns `null` because V8 gives top-level declarations an empty completion value.

## Wait for the right moment

CLI default is `load`. For faster returns on slow sites:

```bash
obscura fetch https://my-spa.example --wait-until domcontentloaded --eval "document.title"
```

| Level              | Returns when                                  |
| ------------------ | --------------------------------------------- |
| `domcontentloaded` | HTML parsed, scripts ran                      |
| `load`             | All subresources finished (default)           |
| `networkidle2`     | ≤2 network connections active for 500ms       |
| `networkidle0`     | 0 network connections active for 500ms        |

(When driving obscura via Puppeteer or Playwright the default is `domcontentloaded` to match client expectations.)

## Common flags

```
--user-agent "..."        Override the User-Agent
--timeout 30                Navigation timeout in seconds (default 30)
--wait 5                    Extra wait after the page settles, in seconds (default 5)
--selector ".main"          CSS selector to narrow output to
--proxy http://host:port    Route through a proxy
--stealth                   Stealth client (TLS fingerprint, tracker blocking)
-o, --output file.html      Write output to a file
-q, --quiet                 Suppress info logging
```

Full list: [CLI reference](CLI-reference.md).
