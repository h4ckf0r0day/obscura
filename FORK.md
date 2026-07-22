# Fork manifest

`asap-cool/obscura` is a long-lived fork of [`h4ckf0r0day/obscura`](https://github.com/h4ckf0r0day/obscura),
maintained so the ASAP platform can pin a browser engine it controls (leads
sourcing: concurrent Google Maps scraping) instead of depending on an upstream
release. The fork **tracks upstream continuously** — see [Sync policy](#sync-policy).

The fork keeps the upstream name (crate, binary, CLI all stay `obscura`) so
upstream merges keep applying cleanly.

## License

Apache-2.0 (unchanged from upstream). Per Apache-2.0 §4(b), the files listed
below carry changes relative to upstream. `LICENSE` and `NOTICE` are retained.
This fork does not imply endorsement by the upstream authors.

## Carry set

Everything this fork adds on top of `upstream/main`. Two buckets:

- **in-flight** — proposed upstream; when the PR merges, the code returns via the
  next sync and the local carry drops out. No permanent divergence.
- **fork-only** — will not be upstreamed; carried indefinitely. Keep these small
  and localized so they survive upstream churn.

| Change | Bucket | Upstream | Files |
|---|---|---|---|
| Fire a `scroll` event when `scrollTop` / `scrollLeft` are assigned directly (upstream only fires from `scrollTo`/`scrollBy`), so scroll-driven lazy loaders advance | in-flight | issue [#459](https://github.com/h4ckf0r0day/obscura/issues/459), PR [#458](https://github.com/h4ckf0r0day/obscura/pull/458). Verified against real Chrome: assigning `scrollTop` fires exactly one `scroll` event there, zero on upstream obscura, one with this patch. Measured impact: a scroll-listener loader runs 0 times instead of 2, collecting 2 rows instead of 12 — silently, with no error. | `crates/obscura-js/js/bootstrap.js`, `crates/obscura-cdp/tests/scroll_event_on_assignment.rs` |
| Report a content-sized scroll box on non-viewport elements (`clientHeight` = a constant instead of `20`, `scrollHeight` = `max(that, descendants * 40)`), so `scrollTop = scrollHeight` keeps *moving* across passes and a virtualized feed pages past its first batch | in-flight | issue [#441](https://github.com/h4ckf0r0day/obscura/issues/441). Reporting only: `scrollTop` stays unclamped, and neither number tracks `innerHeight` — see below for why both matter. Measured on live Google Maps search: **31 → 110** unique results for `plombier marseille`, **36 → 108** for `restaurant lyon`. | `crates/obscura-js/js/bootstrap.js`, `crates/obscura-cdp/tests/scroll_box_geometry.rs` |
| GHCR image publish workflow (upstream publishes to Docker Hub with secrets we do not carry) | **fork-only** | n/a | `.github/workflows/docker-ghcr.yml`, `Dockerfile` |
| This manifest + the sync script | **fork-only** | n/a | `FORK.md`, `scripts/sync-upstream.sh` |

The **scroll geometry** carry above is the second attempt. The first one bundled
three things and was dropped for the two that were wrong:

- a `scrollTop` **clamped** to the synthetic box — the harmful half. Clamping to
  a made-up maximum pins the offset on a loader that has not been handed content
  to scroll over yet, and it deadlocks: no scroll, no content, no scroll.
  Re-measured on live Maps while reintroducing only this: the feed freezes at 20
  results. `scrollTop` therefore stays unclamped.
- a `clientHeight` derived from **`innerHeight`** — the non-deterministic half.
  The stealth layer randomises the viewport per session (measured 640 / 640 /
  688 / 970 across four runs), so the same page paginated or stalled depending on
  the draw: 20/120 items on an unlucky one. Both numbers are now constants.
- the **reporting** itself, which was the load-bearing part and is what the row
  above carries. With a 20px box, `scrollTop = scrollHeight` lands on the same
  offset every pass; an unchanged offset is not a scroll, so the loader is never
  reached again and the feed plateaus after its first batch.

`crates/obscura-cdp/tests/scroll_box_geometry.rs` locks all three in: the box
grows with the subtree, it does not move when `innerHeight` does, and a
virtualized feed pages to its last row. The subtree walk behind `scrollHeight` is
memoized against a DOM epoch bumped by the structural `op_dom` commands, so
repeat reads inside one scroll operation are free while the first read after an
insertion measures the new rows.

The thread-per-connection V8 isolate confinement and the `--max-connections` /
glibc-arena caps that used to be carried here landed upstream via
PR [#435](https://github.com/h4ckf0r0day/obscura/pull/435) (`Closes #430`) and
came back through the sync, so they are no longer a local carry. Same for the
Element scroll methods (PR [#431](https://github.com/h4ckf0r0day/obscura/pull/431))
and the CDP context isolation (issue
[#449](https://github.com/h4ckf0r0day/obscura/issues/449) →
PR [#456](https://github.com/h4ckf0r0day/obscura/pull/456)).

When you add a change that upstream will not take, append a **fork-only** row here
and keep the patch surgical.

## Sync policy

Run `scripts/sync-upstream.sh`. It fetches `upstream/main`, merges it into `main`,
builds, and runs the hard gate (concurrency test + obstacle-course). It never
pushes: review, then push and re-pin yourself.

```sh
git remote add upstream https://github.com/h4ckf0r0day/obscura.git  # once
scripts/sync-upstream.sh
```

Gate (must be green before pushing a sync):

- `cargo nextest run -p obscura-cdp` — 100% (includes the concurrency regression).
- obstacle-course **33/33** (from the `obscura-benchmark` repo, path via `OBSCURA_BENCH`).
- Manual: the live Google Maps feed still paginates to its full result set.

The pre-existing `obscura-js runtime::tests::*` failures are an upstream unit-harness
limitation (no DOM built), not a regression — the gate ignores them.

## Downstream pinning (ASAP)

Pin the **digest**, never a moving tag, so nothing changes under the runtime
without a deliberate re-pin:

```
ghcr.io/asap-cool/obscura@sha256:<digest>
```

Sync on your cadence, run the gate, rebuild the image, then re-pin.
