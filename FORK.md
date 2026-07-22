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
| GHCR image publish workflow (upstream publishes to Docker Hub with secrets we do not carry) | **fork-only** | n/a | `.github/workflows/docker-ghcr.yml`, `Dockerfile` |
| This manifest + the sync script | **fork-only** | n/a | `FORK.md`, `scripts/sync-upstream.sh` |

The virtualized **scroll geometry** heuristic (synthetic `scrollHeight` from
`querySelectorAll('*').length * 40`, viewport-sized `clientHeight` on every
element, and a `scrollTop` clamped to that box) was dropped rather than carried.
It made pagination *worse*, and non-deterministically so: the clamp resolves to
`max(0, descendants * 40 - innerHeight)`, and obscura's stealth layer randomises
`innerHeight` per session (measured 640 / 640 / 688 / 970 across four runs). So
whether a feed paginated or froze at its first batch depended on the viewport
draw — same page, same code, different outcome per run. Measured on a
progress-gated feed fixture: 20/120 items on an unlucky draw, 120/120 without
the heuristic. What survives is the scroll-event dispatch above, which is the
part that was actually load-bearing.

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
