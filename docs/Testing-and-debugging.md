## Test suites

### Rust unit and integration

```bash
cargo nextest run --release --features render --no-fail-fast
```

Crate-scoped:

```bash
cargo nextest run --release --features render -p obscura-cdp
cargo nextest run --release --features render -p obscura-browser
```

By name, also selecting the crate and test target:

```bash
rtk proxy env CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo nextest run --release --features render -p obscura-js --lib completed_scripts_do_not_wait_for_watchdog_deadline
```

Use `cargo nextest`, not `cargo test`. Runtime tests require process isolation
because the engine owns one V8 isolate per process. Render tests must run in
release mode; debug builds are not a fidelity or performance gate.

### Fast edit/test loop

For runtime unit tests, the `test-js` Cargo alias enables incremental compilation
only for `obscura-js`, retaining release optimization and reusing dependencies:

```bash
rtk proxy env -u CARGO_INCREMENTAL CARGO_BUILD_JOBS=2 cargo test-js -E 'test(classic_script_url_is_dynamic_import_referrer)'
```

The first invocation warms an extra compiler cache. Later source edits reuse it.
Unset `CARGO_INCREMENTAL`: an exported `0` overrides the alias's package setting.
This is a local feedback command, not the production benchmark build. Cargo
defaults to more code-generation units with incremental compilation, so do not
use its timings as performance evidence. No global release setting changes.

For other crates, select the affected binary before applying a test-name filter.
CDP integration tests share one binary; select a module within it:

```bash
rtk proxy env CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo nextest run --release --features render -p obscura-cdp --test integration -E 'test(page_frame_contract::)'
```

`-p` selects packages; `--lib` or `--test NAME` selects binaries to compile.
A bare test name or `-E` filters test execution, not the Cargo build target set.
Keep the same package/features while iterating. Workspace tests and the CLI can
resolve different dependency features, requiring distinct engine artifacts.
Do not enable extra production features merely to unify those artifacts.

Run the focused repro after each edit. At a stable candidate, run the full
release nextest gate, exact CLI build and obstacle course from `AGENTS.md`, plus
render/stealth verification when applicable. Do not repeat the whole gate at
each red/green step, and do not omit it when declaring a candidate validated.

For build attribution, append `--timings` to Cargo or nextest; inspect
`target/cargo-timings/`. Preserve caches: avoid `cargo clean`, changing Rust flags,
or switching profiles unless those changes are intentional. Incremental caches
consume extra disk. See Cargo's [profile settings](https://doc.rust-lang.org/cargo/reference/profiles.html)
and nextest's [target selection](https://nexte.st/docs/running/).

### CDP parity tests

`crates/obscura-cdp/tests/*.rs` exercise CDP methods end-to-end with a real
`dispatch` call and an in-process HTTP server. `integration.rs` registers these
files as modules in one binary to avoid repeatedly linking the browser engine.
Nextest still runs each test in its own process. Register new files in its
`integration_tests!` list; a guard fails if a sibling Rust test file is omitted.
Old `--test FILE` commands become `--test integration -E 'test(FILE::)'`.
The tradeoff is that editing one CDP test file rebuilds the shared test binary;
the consolidation targets engine-change rebuild fan-out and artifact duplication.

Pattern:

```rust
#[tokio::test(flavor = "current_thread")]
async fn my_test() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_once().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id.clone());

    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url}), session_id).await;
    // assertions
}
```

`serve_once` and `cdp` helpers are copied across the parity tests; reuse them.

## Logging

```bash
RUST_LOG=obscura=info  obscura serve
RUST_LOG=obscura=debug obscura serve
RUST_LOG=obscura_cdp=trace,obscura_browser=debug obscura serve
```

Logs go to stderr.

`--verbose` on any subcommand is equivalent to `RUST_LOG=obscura=info`.

## Driving the CDP server manually

```bash
obscura serve --port 9222 --verbose
```

In another shell:

```bash
wscat -c ws://127.0.0.1:9222
> {"id":1,"method":"Target.createTarget","params":{"url":"about:blank"}}
> {"id":2,"method":"Target.attachToTarget","params":{"targetId":"...","flatten":true}}
> {"id":3,"sessionId":"...-session","method":"Page.navigate","params":{"url":"https://example.com"}}
> {"id":4,"sessionId":"...-session","method":"Runtime.evaluate","params":{"expression":"document.title"}}
```

Useful for reproducing what Puppeteer or Playwright is doing without their abstraction.

## Common failure modes

### JavaScript behavior disagrees with the current bootstrap source

The runtime embeds a generated V8 snapshot, not `bootstrap.js` directly.
Restored build artifacts can contain an older snapshot even when the Rust ops
are current. Confirm which `OBSCURA_SNAPSHOT.bin` the binary's dependency file
under `target/release/deps/` references before changing engine behavior.
To regenerate the snapshot while retaining dependency caches:

```bash
rtk proxy touch crates/obscura-js/js/bootstrap.js
rtk proxy env CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release -p obscura-cli --bins --features render
```

Then rerun the failing nextest command, which rebuilds its own feature-specific
snapshot as needed. Do not copy generated snapshots between revisions or
feature configurations.

### `Target.createTarget timed out`

Lock contention in the dispatcher. Should not happen on current main. If it does, run with `RUST_LOG=obscura_cdp=trace`, look for handlers that hold `v8_lock` across long awaits.

### `page.goto()` returns `null` from Puppeteer

Means `Network.requestWillBeSent` for the main document did not arrive with `requestId == loaderId`. Check `do_navigate` in `crates/obscura-cdp/src/domains/page.rs`.

### `Cannot find context with specified id`

Playwright's local context counter diverged from the server's `valid_context_ids`. Each navigation must allocate a fresh `executionContextId`. Check `ctx.next_isolated_context()` is called on every nav.

### `V8_Fatal: heap->isolate() == Isolate::TryGetCurrent()`

Two pages tried to use V8 concurrently. The `v8_lock` was bypassed, or a handler suspended a JS runtime while another isolate was entered. Search for direct `JsRuntime` access outside the lock.

### Test hangs

A handler is awaiting something that never resolves. Run with `RUST_LOG=obscura=trace` and check the last log line before the hang.

## Reproducing user bug reports

The integration suite in `tests/test_all.py` is the fastest path from a one-line repro to a regression test. Add the failing case as a new test function, get it failing, then fix.

For Puppeteer / Playwright bug reports, the user's repro script usually drops straight in. Save it as `tests/repro_<issue>.js`, run with `node`, fix until it passes.

### Rendering regressions

Start with the committed deterministic fixtures, then use the representative
real-site suite at both the top and bottom of pages:

```bash
RUN_ROOT="$(mktemp -d)"
OBSCURA_BIN=./target/release/obscura render-repros/run.sh "$RUN_ROOT/fixtures"
OBSCURA_BIN=./target/release/obscura render-repros/representative-suite/run.sh "$RUN_ROOT/top"
OBSCURA_BIN=./target/release/obscura render-repros/representative-suite/run.sh "$RUN_ROOT/bottom" bottom
```

Set `BASELINE_BIN` or `CHROMIUM_BIN` when producing paired captures. Keep the
viewport, user agent, settle policy, scroll position, animation sample, and
capture boundary identical. A pixel-distance score is a regression tripwire,
not a verdict: verify both engines succeeded and produced nonblank output,
then inspect missing resources, geometry, structural edges, and a reduced
fixture. Do not add hostname-specific render branches.

## Profiling

CPU with `perf` and a flamegraph:

```bash
cargo build --release --features render
perf record -F 99 -g -- ./target/release/obscura fetch https://heavy-spa.example
perf script | flamegraph.pl > flame.svg
```

Memory with heaptrack:

```bash
heaptrack ./target/release/obscura serve
```

Tokio task inspection:

```bash
RUSTFLAGS="--cfg tokio_unstable" cargo build --release --features render
./target/release/obscura serve
# in another shell
tokio-console
```

Requires the workspace `tokio` dependency to be built with the `tracing` feature; not enabled by default, add it in the relevant `Cargo.toml` before profiling.
