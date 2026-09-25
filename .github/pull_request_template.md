## What changed

Describe the behavior change and the reason for it. Link the issue with `Fixes #...` when applicable.

## Validation

List the commands and manual checks you ran. Include focused regression tests and the full relevant test suite.

## Rendering

If this changes layout, paint, screenshots, screencast, or PDF output, include the viewport, device scale factor, output options, and before/after images. Write `Not applicable` otherwise.

## Performance

Include before/after measurements for hot paths, memory changes, or rendering work. Write `No expected impact` only when the changed code is outside a runtime path.

## Checklist

- [ ] The change is focused and does not remove existing behavior without justification.
- [ ] Tests cover the failure or feature.
- [ ] Existing tests pass, including render and no-render configurations when affected.
- [ ] I checked for CPU, latency, and memory regressions.
- [ ] Public API or user-facing behavior changes are documented.
