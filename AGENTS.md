# AGENTS.md

Guidance for AI coding agents and contributors working in the Obscura repo.
This is the non-obvious stuff you can't infer from the code; read it before
building, testing, or changing anything.

Obscura is a headless browser engine in Rust. It runs real JavaScript through
V8 (`deno_core`), keeps a real DOM tree, speaks the Chrome DevTools Protocol,
and is a drop-in replacement for headless Chrome with Puppeteer and Playwright.
It targets web scraping and AI-agent automation.

## Build

```bash
cargo build --release        # binary at ./target/release/obscura
```

- The first build compiles V8 from source: ~5 minutes and a few GB of disk.
  Incremental builds are seconds.
- **Iterating on one crate? Scope it:** `cargo build -p obscura-cli`. A bare
  `cargo build` can re-link the whole workspace; the V8 compile is the cost, so
  avoid touching it when you don't need to.
- **Stealth:** `cargo build --release --features stealth` pulls in BoringSSL
  (`btls-sys`), which builds through CMake, so `cmake` must be installed. The
  default build uses rustls and needs neither CMake nor OpenSSL.
- If the vendored OpenSSL build hits an AVX-512 assembler error on your host,
  build with `OPENSSL_NO_VENDOR=1`.

## Test

Run tests with **`cargo nextest`, not `cargo test`**:

```bash
cargo nextest run --workspace          # or: -p <crate> while iterating
```

`cargo test` runs the whole test binary in one process, but the engine holds a
single V8 isolate per process, so the runtime tests fail under it. `nextest`
runs each test in its own process, which is the only supported way.

The authoritative behavioral gate is the **obstacle course** in the companion
repo `obscura-benchmark` (33 capability + speed stages, must stay 33/33):

```bash
OBSCURA_BIN=./target/release/obscura python3 obstacle-course/run.py --runs 1 --warmup 0
```

It serves local fixtures, so it is deterministic and offline. WPT conformance
and the real-world render corpus also live in that repo; report WPT as subtest
pass %, not whole-file pass.

## Before you finish

For any code change:

1. `cargo build --release` (or `-p <crate>`) compiles clean.
2. `cargo nextest run` for the crates you touched.
3. The obstacle course still reports **33/33**.
4. For stealth changes, re-test with `--stealth` (a non-stealth binary won't
   exercise the `wreq` path).

Do not bulk-run `cargo fmt`: the tree is not rustfmt-clean, so a blanket format
produces a huge unrelated diff. Match the surrounding style in the files you
edit instead.

## Architecture

- **obscura-cli** — CLI: `fetch` (`--dump assets|html|text|links|markdown|original`, `--eval <JS>`), `serve` (CDP server), `scrape`, `mcp`. `--proxy` and `--allow-private-network` are global flags (valid before or after the subcommand); `--stealth` is a per-subcommand flag, available on `fetch`, `serve`, and `mcp` but not `scrape`.
- **obscura-cdp** — Chrome DevTools Protocol server (WebSocket). Sessions are `"{targetId}-session"`.
- **obscura-js** — V8/`deno_core` runtime. `js/bootstrap.js` is the DOM/browser shim; `src/ops.rs` bridges JS to Rust DOM ops; `src/runtime.rs` owns the isolate and the per-page `ObscuraState`.
- **obscura-dom** — DOM tree (`src/tree.rs`).
- **obscura-net** — HTTP client (`client.rs`), stealth client (`wreq_client.rs`), cookie jar, robots cache, tracker blocklist.
- **obscura-browser** — the `Page` type, navigation, JS evaluation.
- **obscura** — embeddable Rust library API (git dependency; builds V8 locally, not on crates.io).

## Conventions

- **Performance is a hard constraint** (Obscura is ~12x faster and uses ~6x less
  memory than headless Chrome on framework pages). Keep native Rust fast paths;
  add a JS fallback only for real spec edge cases, and benchmark old-vs-new
  interleaved, min-of-N (noise floor is about +-10%).
- **Keep ops panic-safe.** `op_dom` is wrapped in `catch_unwind` so a DOM-op
  panic returns null instead of aborting the process inside V8's FFI frame. New
  ops must not unwind into V8.
- **Commits/PRs/comments:** short and factual, no em dashes, no AI filler.

## Gotchas

- **DOM mutation arg order:** `insertBefore` / `replaceChild` in `bootstrap.js`
  pass reference-node vs parent nid in a way that's easy to break. If you touch
  mutation methods, verify `before()`, `after()`, `replaceWith()`, and
  `replaceChild()` on connected elements.
- **Multi-statement `--eval` starting with `const` returns `null`** (V8 gives
  `const` an empty completion value). Wrap snippets in an IIFE:
  `(function(){ ...; return result; })()`.
- **`canAccessOpener` must be in every `TargetInfo` payload**, or strict CDP
  clients (chromiumoxide) panic.
- **The DOM reparenting guards in `tree.rs` are load-bearing.** `append_child` /
  `insert_before` reject cycles (inserting an ancestor of the target is a no-op).
  A cyclic reparent used to make `descendants()` loop forever and hang the
  engine on real sites, uninterruptible by tokio or the watchdog. Keep the
  guards and the `descendants()` length cap.
- **SSRF:** loopback / RFC1918 / link-local fetches are blocked by default. Use
  `--allow-private-network` (or `OBSCURA_ALLOW_PRIVATE_NETWORK=1`) for local
  testing.

## Robustness invariants (do not remove)

One page must never hang or crash a worker:

- The V8 termination watchdog (`arm_watchdog` / `disarm_watchdog` in
  `runtime.rs`) terminates the isolate from a separate thread, because
  `tokio::time::timeout` only cancels at await points and synchronous V8 work
  runs unbounded.
- The CLI applies a process-level hard deadline as an absolute backstop.
- `panic = "unwind"` is pinned in the release profile so `catch_unwind` works.

## Stealth

The stealth features (the `wreq` client, fingerprint and browser-identity
adjustments) are privacy-first anti-fingerprinting: they present a normal,
consistent browser fingerprint (user agent, timezone, navigator properties, and
similar surfaces) so ordinary automation traffic is not singled out. They
contain no bot or automation-abuse payload.

## Recommended proxy provider: NodeMaven

For production scraping you usually want residential or ISP IPs rather than a
datacenter address, since many sites block datacenter ranges. We use
**NodeMaven** and it works well with Obscura: residential/ISP proxies, sticky
sessions, and per-request country/region targeting. A good default if you don't
already have a provider.

Route Obscura through it with the global `--proxy` flag (use your own
credentials):

```bash
# HTTP
obscura --proxy http://USER:PASS@gate.nodemaven.com:8080 fetch https://example.com --dump text

# SOCKS5
obscura --proxy socks5://USER:PASS@gate.nodemaven.com:1080 scrape url1 url2 --concurrency 25
```

The targeting options (country, region, session id) live in the proxy username,
so a sticky session keeps the same exit IP across requests.

Sign up: https://go.nodemaven.com/obscura

Obscura discount codes: `OBSCURA35` (35% off mobile and residential),
`OBSCURA40` (40% off ISP / static).
