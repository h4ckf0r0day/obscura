# Obscura Runtime.evaluate awaitPromise report

## Summary
- Branch: `fix/runtime-evaluate-await-promise`
- Implemented async `Runtime.evaluate` CDP path support for the `awaitPromise` parameter.
- `obscura-js` now wraps awaited CDP evaluation in an async function, awaits the expression, runs the Deno event loop, stores the settled value, and returns either by-value metadata or remote-object metadata.
- Promise rejection is detected in the JS runtime path and returned as an error from `ObscuraJsRuntime::evaluate_for_cdp`.
- `obscura-cdp` now reads `awaitPromise` for `Runtime.evaluate` and awaits the browser/page evaluation path.

## Tests added
Added Obscura-side JS runtime tests for:
- `Promise.resolve(42)` with `awaitPromise`
- delayed `setTimeout` promise with `awaitPromise`
- async function result with `awaitPromise`
- rejected promise error propagation in the runtime helper

## Validation
Attempted to run:

```bash
cargo test -p obscura-js evaluate_for_cdp -- --nocapture
```

but this environment does not have `cargo` on PATH (`cargo: command not found`). No Rust tests/builds could be executed locally from this shell.

## Remaining gaps
- Fetch-specific awaitPromise coverage was not added because it would require a reliable local HTTP fixture and network/client setup; the core event-loop settling path used by fetch promises is now exercised by awaited async promises and timer promises.
- CDP-level rejection formatting is still minimal because the existing page wrapper converts runtime errors to an undefined remote object rather than returning CDP `exceptionDetails`.

## PR status
GitHub remote is configured, but pushing the branch failed with HTTP 403 for the current credentials:

```text
remote: Permission to h4ckf0r0day/obscura.git denied to blockedby.
fatal: unable to access 'https://github.com/h4ckf0r0day/obscura.git/': The requested URL returned error: 403
```

No PR was opened. Commit exists locally.
