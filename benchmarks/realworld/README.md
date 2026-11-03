# Real-world render-success benchmark

Fetches a corpus of live public pages (`sites.txt`) with obscura, runs each
page's JavaScript to settle, and reports whether obscura produced a usable
rendered document, plus per-page latency and peak memory.

A page counts as rendered when obscura exits cleanly with a non-trivial
serialized DOM and a readable `<title>`. The client-rendered SPAs in the list
(react.dev, vuejs.org, svelte.dev, angular.dev, ...) only pass if their
JavaScript actually ran, since their shipped HTML is near-empty.

This measures "does obscura handle the real web", as opposed to the synthetic
`obstacle-course/` fixtures. The live web drifts and some sites rate-limit or
block automation, so the success rate and timings are a snapshot, not a fixed
score.

```sh
OBSCURA_BIN=/path/to/obscura python3 run.py                 # default corpus
OBSCURA_BIN=/path/to/obscura python3 run.py --wait 6        # give SPAs longer
OBSCURA_BIN=/path/to/obscura python3 run.py --sites my.txt  # your own list
OBSCURA_BIN=/path/to/obscura python3 run.py --json
```

`sites.txt` is a plain list of URLs (blank lines and `#` comments ignored).
Edit it to match the kind of pages your scraper actually targets.
