# Obscura Node and WebAssembly migration memory

## Decision

V8 137.3.0 cannot be compiled to `wasm32-unknown-unknown`. Its build requests
`librusty_v8_release_wasm32-unknown-unknown.a.gz`, which is not published, and
its source build has no wasm32 platform branch. V8 can execute WebAssembly, but
the V8 engine is not itself a supported WebAssembly payload.

The viable design is:

```text
Node Worker and its V8 isolate
  page JavaScript and browser bootstrap
  host transport, timers, scheduling, and process limits
             |
             | versioned, batched ABI
             v
obscura-wasm
  DOM, selectors, style, layout, portable state, and CPU paint
```

Node is the first host. Deno can use the same ABI after Node is behaviorally
complete. The separate `obscura-node` experiment embeds a second native V8 in
Node and is a fidelity fallback, not the host-V8 WebAssembly design.

## Implemented feasibility slice

- `crates/obscura-wasm` reuses Obscura's parser, DOM tree, selectors, text
  access, and serialization through wasm-bindgen.
- `node/wasm-v8-harness` loads the module in a persistent Worker, evaluates
  JavaScript with Node V8, and exposes a small read-only `document` facade
  backed by the Rust/WASM DOM.
- The harness tests deterministic disposal, startup failure cleanup, repeated
  evaluation, raw WASM inspection, and forced Worker termination.
- `tooling/wasm/audit.sh` checks the portable boundary and classifies the
  native networking, paint, deno_core, and rusty_v8 blockers.
- `crates/obscura-node` is an experimental Node-API wrapper around the current
  native `ObscuraJsRuntime` and its embedded V8.

This is not a full browser, CDP endpoint, Puppeteer/Playwright replacement, or
Deno integration.

## Build and verification commands

Portable release WASM:

```bash
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 \
  cargo build --release -p obscura-wasm --target wasm32-unknown-unknown

wasm-bindgen --target nodejs \
  --out-dir /workspaces/.obscura-wasm-pkg \
  target/wasm32-unknown-unknown/release/obscura_wasm.wasm

node node/wasm-v8-harness/bin/run.mjs \
  --module /workspaces/.obscura-wasm-pkg/obscura_wasm.js \
  --mode all
```

Portable boundary audit:

```bash
tooling/wasm/audit.sh
tooling/wasm/audit.sh --require-full
```

The strict audit remains expected to fail until networking, paint resources,
deno_core, and V8 have portable replacements.

Native embedded-V8 experiment on Linux:

```bash
CARGO_TARGET_DIR=/workspaces/.obscura-node-target \
  CARGO_INCREMENTAL=0 \
  CARGO_BUILD_JOBS=2 \
  V8_FROM_SOURCE=1 \
  GN_ARGS='v8_monolithic=true v8_use_external_startup_data=false v8_monolithic_for_shared_library=true' \
  cargo build --release -p obscura-node
```

The prebuilt rusty_v8 archive cannot link into a shared object because it uses
`R_X86_64_TPOFF32` local-exec TLS relocations. The source configuration above
produces `-fPIC` objects and defines `V8_TLS_USED_IN_LIBRARY` so V8 uses a
shared-library-safe TLS model. Keep `napi_build::setup()` because it emits
`-Wl,-z,nodelete` on Linux.

## Verified measurements

All measurements are local, network-free, and use a warm filesystem.

| Path | Median |
| --- | ---: |
| Native process through `--version` | 3.72 ms |
| Native process through a real CDP WebSocket handshake | 16.75 ms |
| Native local HTML fetch plus `document.title` | 22.36 ms |
| Node release-WASM first Worker ready | 88.32 ms |
| WASM module load within that Worker | 8.30 ms |

Final regenerated artifacts after adding the document-element serializer:

- raw release WASM: 885,228 bytes;
- Node-bound WASM: 849,307 bytes;
- Node wrapper: 8,974 bytes;
- release-mode nextest for `obscura-wasm`: 1/1 passed;
- Node harness: 8/8 passed;
- real host-V8 to WASM-DOM bridge: passed;
- forced infinite-evaluation Worker termination: passed.

The spike is not a cold-start speedup. First Worker readiness is about four to
five times the measured native startup paths. A persistent pool amortizes the
cost. WASM should not be assumed to improve DOM, layout, or paint throughput;
chatty JavaScript/WASM calls can be much slower. Batch operations and buffers.
The initial warm-performance target is within 10 to 25 percent of native on
representative end-to-end workloads, subject to interleaved measurement.

## Migration phases and acceptance gates

1. Stable host ABI and ownership
   - Add version/capability negotiation, integer handles, stable errors, bulk
     buffers, page reset, request/response queues, and deterministic disposal.
   - Reject stale and cross-page handles. No panic may cross wasm-bindgen or
     Node-API boundaries.

2. Host transport and profile state
   - Keep redirect, cookie, interception, proxy, and SSRF policy in portable
     Rust where practical. Execute sockets and fetch in Node.
   - Validate initial, redirected, and rewritten URLs. Preserve private-network
     blocking unless explicitly enabled.

3. DOM, bootstrap, and task bridge
   - Run the existing browser bootstrap in Worker V8 and replace deno_core ops
     with a generated batched adapter.
   - Preserve mutation argument order, cycle guards, traversal limits,
     microtask/timer/network/render ordering, and forced termination.

4. Render and resource split
   - Keep style/layout in portable Rust. Move image, SVG, font, CSS, time, and
     randomness acquisition behind host contracts.
   - Return paint/PDF buffers with explicit ownership and release.

5. Browser and CDP compatibility
   - Add Page, targets, sessions, lifecycle, and CDP over the completed runtime.
   - Preserve strict fields such as `canAccessOpener` and validate Puppeteer and
     Playwright flows.

6. Production hardening
   - Use a persistent Worker pool, explicit deadlines and memory/body limits,
     cancellation, page isolation, and clean shutdown.
   - Run focused and full release nextest, the exact CLI build, obstacle course
     33/33, WPT subtest comparisons, rendering fixtures, and interleaved latency
     and resource benchmarks.

Migration completion requires all phases. The current successful build and DOM
bridge prove only the feasibility slice.
