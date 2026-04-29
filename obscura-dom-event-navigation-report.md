# Obscura DOM Event + Navigation Side Effects Report

Status: Completed locally on branch `fix/dom-event-navigation-side-effects`.

Base/dependency note: branch was created from current `fix/runtime-evaluate-await-promise` state as requested, so this work depends on that branch until it is merged/rebased.

## Changes

- `crates/obscura-js/js/bootstrap.js`
  - Preserves the original `event.target` during bubbling.
  - Allows bubbling even when `preventDefault()` was called, matching browser behavior.
  - Implements propagation stop flags for `stopPropagation()` and `stopImmediatePropagation()`.
  - Makes `preventDefault()` honor `cancelable`.
- `crates/obscura-js/src/ops.rs`
  - `op_navigate` now updates runtime URL state immediately as well as recording pending navigation.
- `crates/obscura-js/src/runtime.rs`
  - Added acceptance tests for:
    1. `button.click()` runs click listener and updates `dataset`.
    2. `dispatchEvent(new MouseEvent('click', { bubbles: true }))` runs listener.
    3. `location.href = '/next'` updates URL/navigation state.
    4. Submit button click runs submit handler; handler `preventDefault(); location.href='/submitted'` updates URL/navigation state.

## Verification

- `cargo test -p obscura-js` — passed, 58 tests.
- `cargo test -p obscura-cdp` — passed, 0 tests.

Existing warnings remain in `obscura-net`, `obscura-js`, and `obscura-cdp`; no new failure observed.

## Remaining gaps

- I did not open a PR or push because remote permissions were not attempted/confirmed in this environment.
- `cargo fmt` was tested earlier but it reformats many unrelated existing files in this repo, so I reverted those broad formatting-only changes and kept the final diff scoped to this task.
