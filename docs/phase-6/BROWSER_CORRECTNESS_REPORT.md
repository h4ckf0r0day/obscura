# Phase 6 Browser Correctness Report

## Known failures
- 3 child-frame (static/shadow) tests.
- 18 render tests (flex/grid/table/intrinsic sizing).
- Inline span alpha TODO in `crates/obscura-render/src/inline.rs:3995`.
- Dead font-loading helpers.

## Load-bearing invariants
- DOM reparenting guards in `crates/obscura-dom/src/tree.rs` prevent cyclic descendants hang.
- V8 watchdog in `crates/obscura-js/src/runtime.rs`.
- Process-level hard deadline in CLI.

## Status
Root causes not yet fixed; preserve existing geometry invariants.
