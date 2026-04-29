# Obscura inline script click-submit follow-up

## Findings

- The existing CDP click-submit parity test was not strong enough to prove inline page scripts loaded by `Page.navigate` executed in the document.
- I updated `crates/obscura-cdp/tests/cdp_click_submit_parity.rs` so the served page now includes an inline script with:
  - a global `function submitCompat() { location.href = '/submitted'; }`
  - a real `document.querySelector('button').addEventListener('click', ...)` handler that calls `submitCompat()`
- The test now navigates with CDP `Page.navigate`, verifies `Runtime.evaluate` reports `typeof submitCompat` as `function`, then obtains the real button element and invokes `this.click()` through `Runtime.callFunctionOn`.
- After the click, the test asserts the page navigated to `/submitted` and the body contains `submitted`.

## Changes made

- Strengthened `crates/obscura-cdp/tests/cdp_click_submit_parity.rs` only.
- No production code changes were necessary: the strengthened regression test passes against the current implementation.
- SSRF/private-network behavior remains unchanged. The integration test continues to opt in with `OBSCURA_ALLOW_PRIVATE_NETWORK=1`.
- Did not touch positions.

## Verification

- `cargo test -p obscura-cdp --test cdp_click_submit_parity -- --nocapture` passed.
- `cargo test` passed.

## Notes

- This validates inline script execution and real DOM click listener navigation at the direct CDP dispatch layer. If `/home/kcnc/.local/bin/obscura-cdp-click` still fails against a live CDP server, the remaining gap is likely in the CLI/server/client path rather than inline script execution during `Page.navigate` itself.
