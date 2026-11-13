# AUDIT VALIDATION 05 — BROWSER CORRECTNESS

## F-01 Child frames — CONFIRMED (3 failures)

### Failing tests
- `a_static_child_frame_runs_its_own_script`
- `a_shadow_dom_child_frame_stays_alive_and_runs_its_script`
- `navigating_blank_releases_child_realms`

### Evidence
Ran `cargo test -p obscura --test child_frame_scripts`: 3 failed, 8 passed.

### Root cause
Static parser-created `<iframe src>` loading depends on a post-parse JS pass (`_loadIframeSrc`) executed at line ~3735 of page.rs. Frame realms are only created when pending frames are queued (`queue_pending_frames` → `take_pending_frames`). Shadow-root iframes and static iframes are not being queued as pending frames during `build_document_frames`/`advance_frames`, so no realm is created and no script runs. Navigation to about:blank does not release realms because frames list stays empty (nothing was registered), but test expects 0 and got 1 mismatch due to partial state.

### Classification
Missing/incorrect execution order in frame lifecycle: static iframe discovery happens but frame registration not completed before assertions. Not an environmental failure.

### Required correction
Ensure static iframe elements are assigned frame IDs during parse and queued in the initial pending-frames batch; shadow DOM traversal in frame discovery. Add regression tests already present.

## F-02 Rendering — CONFIRMED (18 failures)

### Failing tests (obscura-render dom::tests)
All 18 are layout edge cases: flex anonymous text wrapping, auto-width column flex, float BFC, fixed table layout, inline fragments, pseudo positioning, textarea intrinsic size, white-space inheritance, etc.

### Evidence
`cargo test -p obscura-render`: 297 passed, 18 failed. Example failure: `direct_flex_text_is_one_wrapping_anonymous_item` expected height 60.0, got 20.0 (text did not wrap).

### Root cause
Flex layout anonymous text item handling: direct text in a flex container is not being wrapped into an anonymous flex item with proper inline formatting context; instead treated as a single non-wrapping item. Similar under-measurement in table fixed layout and inline continuation. Not font-environmental (Liberation Sans present; failures deterministic).

### Impact
Layout correctness for real pages using flexbox with text, tables, floats. Screenshots/geometry affected.

### Required correction
Fix flex anonymous item creation and inline wrapping; table track resolution; inline edge participation. Do not weaken tests.

## Other browser failures
- Network: 1 (not separately reproduced here; likely URL/redirect).
- JS: 1 (import_map clippy-related?).
- Browser: 1.

## Status: CONFIRMED for F-01 and F-02.
