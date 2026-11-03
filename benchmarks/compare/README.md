# Head-to-head: obscura vs headless Chrome

Two harnesses that compare obscura against headless Chrome on the same pages.
Both launch a fresh process per page and ask it to load the page, run its
JavaScript to settle, and serialize the resulting DOM:

- obscura: `obscura fetch <url> --dump html --wait <W>`
- chrome: `google-chrome --headless --dump-dom --virtual-time-budget=<W>ms <url>`

This is the scraping / automation path: fetch, run scripts, read the DOM.
obscura has no rendering, layout, or paint pipeline, so this is not a
full-browser comparison. It is the comparison that matters for headless
scraping and agent automation, where the rendered pixels are never used.

## head-to-head.py

Per-page latency (wall clock) and peak memory (GNU `time -v` maximum resident
set size), median over N validated runs. A run is only counted if it produced a
fully rendered DOM, so a dropped sub-resource never shows up as a fast result.

```sh
OBSCURA_BIN=/path/to/obscura python3 head-to-head.py            # all fixtures
OBSCURA_BIN=/path/to/obscura python3 head-to-head.py --filter frameworks --runs 5
OBSCURA_BIN=/path/to/obscura python3 head-to-head.py --json
```

## scale.py

Throughput (pages/sec) and peak total RSS as concurrency rises (a pool of C
cold processes at a time). The "scrape at scale" view: pages/sec a worker box
sustains and the RAM it costs. Run on an otherwise idle host; CPU contention
skews throughput (memory is unaffected).

```sh
OBSCURA_BIN=/path/to/obscura python3 scale.py --pages 24 --levels 1,4,8
```

## Notes on fairness

- Cold process per page is obscura's normal model and the worst case for Chrome
  (production Chrome usually reuses one browser across tabs). The point is that
  obscura's per-page process is cheap enough to fan out cold, where Chrome's is
  not: Chrome pays a large fixed startup (process + V8 + browser stack) on every
  invocation regardless of the page.
- Set `CHROME_BIN` to point at a specific Chrome/Chromium build.
