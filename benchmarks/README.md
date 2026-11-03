# obscura-benchmark

Conformance, capability, and performance benchmarks for
[Obscura](https://github.com/h4ckf0r0day/obscura), a headless browser engine
written in Rust for web scraping and AI agent automation. WPT coverage here is
intentionally limited to the classic DOM/JavaScript standards path. Rendering
regressions belong to Obscura's deterministic `render-repros/` suite in the main
repository, where changes can be reviewed against engine-owned expectations.

## Benchmark tracks

| track | question it answers | where |
| ----- | ------------------- | ----- |
| WPT conformance | how much of the web platform does Obscura implement correctly | `crates/wpt-runner`, `crates/triage` |
| Obstacle course | does it handle the modern web (React/SPA/async/web APIs), and how fast | `obstacle-course/` |
| vs headless Chrome | how does its speed and memory compare to the standard headless engine | `compare/` |
| Real-world corpus | does it render real public pages, including SPAs | `realworld/` |
| Stealth bench | does it present a consistent, undetectable Chrome fingerprint | `stealth-bench/` |
| Perf bench | per-page `fetch` / `scrape` latency on a small URL set | `crates/perf-bench` |
| Reliability | does it crash, panic, or hang on a large corpus of real pages | `reliability/` |

## Results

The latest full pass ran on 2026-09-11 against Obscura commit
`01e1caa33360f6c02643457307894ec885e82eef` and pinned WPT commit
`03f14d4780c4d981bc84c65679b18e9327a1affe`. The complete compressed JSON and
triage report are under `results/published/2026-09-11-wpt-no-render/`.

### Web Platform Tests (conformance)

Subtest pass rate by tier (a tier is a capability scope, defined in
`crates/triage/src/tiers.list`, not a cherry-picked subset). A subtest is one
assertion; a single test file holds many, so subtest pass rate is the standard
conformance measure:

| tier | subtests passing | role |
| ---- | ---------------- | ---- |
| Core | 327,094 / 392,547 (**83.3%**) | the DOM/HTML/URL/fetch scraping contract |
| Relevant | 521,393 / 600,709 (**86.8%**) | Core plus broader JS-observable correctness |
| Full | 603,997 / 893,328 (**67.6%**) | the whole testharness suite, for transparency |

Core subtest pass rate over time:

| date | obscura state | Core subtests |
| ---- | ------------- | ------------- |
| 2026-06-03 | baseline | 8.0% |
| 2026-06-04 | round 2 | 15.4% |
| 2026-06-04 | + charset/URL encoding | 72.7% |
| 2026-06-04 | + IDL reflection, attr folding, storage | 81.6% |
| 2026-07-03 | historical main | **83.3%** |
| 2026-09-11 | `01e1caa3` | **83.3%** |

The "Full" tier includes large subtrees outside Obscura's classic profile scope
(layout, rendering, media, and hardware), so it is reported only for
transparency. Core and Relevant remain the headline because they exclude those
areas by capability, not by outcome. See `crates/triage/src/tiers.list` for the
exact rules. For cross-engine context, the same WPT areas for
Chrome/Firefox/Safari are published on [wpt.fyi](https://wpt.fyi/).

### Obstacle course (capability + speed)

33 / 33 stages pass, median ~44 ms per stage (cold `obscura fetch`, including
process startup). Covers client-side React/Preact/Vue, SSR hydration, ES modules
and dynamic import, IntersectionObserver/MutationObserver, `fetch` + pushState
SPAs, the URL/TextDecoder/FileAPI/Range/Selection/custom-element/dialog web-API
surface, the `--dump` extraction modes, charset decoding, cookies, and stealth
fingerprint consistency.

### vs headless Chrome (speed + memory)

Cold process per page, both fully rendering the same client-side app (verified:
obscura serializes the post-JavaScript DOM, e.g. 100 React `<li>` elements that
are not in the shipped HTML):

| page | obscura | headless Chrome | obscura advantage |
| ---- | ------- | --------------- | ----------------- |
| react | 81 ms, 30 MB | 815 ms, 190 MB | 10x faster, 6x less memory |
| preact | 45 ms, 30 MB | 745 ms, 191 MB | 16x faster, 6x less memory |
| vue | 86 ms, 31 MB | 796 ms, 190 MB | 9x faster, 6x less memory |

Across all 33 obstacle-course fixtures the median is ~21x faster and ~7x less
memory (obscura ~27 MB vs Chrome ~190 MB per process). The framework rows above
are the heaviest-render, most conservative cases; lighter pages widen the gap
because Chrome pays the same fixed startup regardless of the page.

Throughput and memory as concurrency rises (24 React-app loads, idle host):

| engine | 1 worker | 4 workers | 8 workers |
| ------ | -------- | --------- | --------- |
| obscura | 12 pg/s, 30 MB | **40 pg/s, 112 MB** | 18 pg/s, 159 MB |
| headless Chrome | 1.2 pg/s, 1.1 GB | 3.0 pg/s, 4.2 GB | 3.0 pg/s, **8.1 GB** |

Obscura sustains far higher throughput at a fraction of the memory. Chrome pays
a large fixed startup (process + browser stack) on every page; under concurrency
its RAM climbs into the gigabytes while Obscura stays in the low hundreds of MB.
This is the no-render scraping profile; production Chrome would reuse one browser
across tabs (the cold-process numbers are Chrome's worst case).

### Real-world corpus (vs headless Chrome)

98 live public pages, fetched with obscura and headless Chrome side by side:

| engine | rendered | median latency | median peak RSS |
| ------ | -------- | -------------- | --------------- |
| obscura | 94 / 98 (95.9%) | 5.2 s | **64.2 MB** |
| headless Chrome | 85 / 98 (86.7%) | 2.1 s | 201.2 MB |

Obscura renders more of the corpus than headless Chrome (94 vs 85), and at ~3x
less memory. Chrome's misses are mostly sites that serve a datacenter IP an
anti-bot or CAPTCHA wall, which is not an engine failure on either side. Latency
is mixed: obscura is much faster on static content pages and slower on heavy
client-rendered SPAs, where its post-load settle wait (`--wait`) dominates the
median. The live web drifts, so these are a snapshot; see `realworld/sites.txt`.

### Reliability (crash / hang sweep)

Conformance and speed do not matter if the engine crashes or hangs on a live
page. A 1500-URL corpus (a one-level crawl from the real-world seed list) is
rendered through obscura, classifying each outcome from the exit code and stderr:

| outcome | count |
| ------- | ----- |
| rendered | 1432 / 1500 (95.5%) |
| thin / blocked | 67 |
| bounded hang (deadline) | 1 |
| **crash (signal)** | **0** |
| **panic** | **0** |

Zero crashes and zero panics across 1500 diverse pages. Any page that does not
finish in its budget is terminated deterministically (a V8 termination watchdog
plus a process-level hard deadline), so no page can wedge a worker. The single
bounded hang (kayak.com) is a heavy page that exceeds the tight per-page budget
under concurrency; it renders normally when run on its own. Run it with
`OBSCURA_BIN=<bin> python3 reliability/sweep.py`.

## 1. WPT conformance

`crates/wpt-runner` runs every supported WPT testharness file through a separate
`obscura fetch` process and reads the result left by
`wpt-overlay/resources/testharnessreport.js`. WPT reftests are not run here:
rendering regression ownership stays with the deterministic fixtures in the
main Obscura repository.

Tests requiring unimplemented runner automation (testdriver, PAC, or HTTP/3
serving) are reported as unsupported, not as engine
failures. `crates/triage` reports that count separately and groups real failures
into deduplicated root causes.

```sh
# one-time setup: check out the revision in wpt-config/WPT_COMMIT,
# install the report overlay, and build the manifest
scripts/setup-wpt.sh
# then add the WPT hostnames once (needs sudo), as printed by setup-wpt.sh:
#   ( cd wpt && ./wpt make-hosts-file ) | sudo tee -a /etc/hosts

# full classic WPT pass
OBSCURA_BIN=/path/to/obscura-no-render \
  scripts/run-wpt.sh --concurrency 32 --wait-secs 15

# a subset, by one path substring
OBSCURA_BIN=/path/to/obscura-no-render \
  scripts/run-wpt.sh FileAPI/url/
```

The suite is roughly 38k test variants at the current pin. Use a concurrency
that does not oversubscribe the host: oversubscription makes per-test timeouts
fire and depresses the pass rate. Outputs are written under `results/`, including
the JSON report and a triage Markdown report.

Build the classic Obscura binary without optional features:

```sh
BENCH_BIN_DIR=/path/to/obscura-benchmark/.bench-bin
mkdir -p "$BENCH_BIN_DIR"
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release \
  -p obscura-cli --bins --no-default-features
cp target/release/obscura "$BENCH_BIN_DIR/obscura-no-render"
```

Run those build commands in the Obscura repository. Record its exact commit with
the result commit; the WPT side is reproducibly pinned by this repository.

## 2. Obstacle course

A curated set of small self-contained modern-web pages that a no-DOM-engine
browser cannot handle. Each fixture runs its JavaScript and asserts a
deterministic result; the runner also times each page. See
`obstacle-course/README.md` for the full stage list.

```sh
OBSCURA_BIN=/path/to/obscura scripts/run-obstacle-course.sh
OBSCURA_BIN=/path/to/obscura scripts/run-obstacle-course.sh --filter react --runs 10
```

It exits non-zero if any stage's result does not match, so it doubles as a
correctness regression check.

## 3. Head-to-head vs Chrome

`compare/` runs Obscura and headless Chrome as cold processes over the same
pages and reports latency, peak memory, and throughput under concurrency. See
`compare/README.md`.

```sh
OBSCURA_BIN=/path/to/obscura CHROME_BIN=google-chrome scripts/run-compare.sh
```

Run it on an idle host; CPU contention skews throughput (memory is unaffected).

## 4. Real-world corpus

`realworld/` fetches a list of live public pages and reports render-success rate,
latency, and memory. See `realworld/README.md`.

```sh
OBSCURA_BIN=/path/to/obscura scripts/run-realworld.sh
```

## 5. Stealth bench

`stealth-bench/` evaluates Obscura's browser fingerprint, anti-detection measures,
and cross-page consistency when running in `--stealth` mode. It runs locally
and does not require external network access.

```sh
OBSCURA_BIN=/path/to/obscura-stealth python3 stealth-bench/run.py --stealth
```

## 6. Perf bench

`crates/perf-bench` times `obscura fetch` / `obscura scrape` on a small default
URL set (override by passing URLs).

```sh
OBSCURA_BIN=/path/to/obscura scripts/run-bench.sh
OBSCURA_BIN=/path/to/obscura scripts/run-bench.sh https://example.com https://news.ycombinator.com
```

## Repo layout

```
crates/
  wpt-runner/      classic WPT testharness runner
  triage/          groups WPT failures into root causes; tiers.list defines the tiers
  perf-bench/      times `obscura fetch` / `obscura scrape`
obstacle-course/   modern-web capability + speed fixtures (+ run.py, manifest.json)
compare/           obscura vs headless Chrome: head-to-head.py, scale.py
realworld/         live-page render-success corpus: sites.txt, run.py
stealth-bench/     stealth fingerprint, anti-detection, and consistency test suite
wpt-overlay/       the custom WPT report script installed into the WPT checkout
scripts/           setup and run wrappers
results/           generated run artifacts (gitignored)
wpt/               the WPT checkout (gitignored; created by setup-wpt.sh)
```

## Setup and requirements

- Rust toolchain (`cargo build --release` builds the runner, triage, and perf-bench).
- An Obscura binary, passed via `OBSCURA_BIN`.
- Python 3 for the obstacle-course, compare, and realworld runners.
  `compare/scale.py` uses `psutil` for memory sampling (`pip install psutil`),
  and the compare/realworld harnesses read peak RSS from GNU `time -v`.
- For the head-to-head, a Chrome or Chromium build (`CHROME_BIN`).
- For WPT, the one-time `scripts/setup-wpt.sh` (checks out the pinned WPT commit,
  builds the manifest, and installs the report overlay) plus the WPT hostnames
  in `/etc/hosts`.

`results/` and the `wpt/` checkout are gitignored; run artifacts are regenerated
by the scripts.
