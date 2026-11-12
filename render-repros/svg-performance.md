# SVG PR performance reproduction

Build the base and SVG revision with the same Rust toolchain, lockfile, release
profile, and `render` feature. Use separate source and target directories so
artifacts from another worktree cannot be reused accidentally.

The reduced reproduction runs the existing CI `table-rows-hot` scenario. It
checks the result and alternates base/candidate execution order over 15 rounds:

```sh
python3 render-repros/svg-performance.py \
  --base /absolute/path/to/base/obscura \
  --candidate /absolute/path/to/candidate/obscura \
  --output /tmp/svg-table-performance.json
```

It imports `scripts/ci/perf_smoke.py`, preserving the scenario, process limits,
latency/CPU/RSS measurements, and failure thresholds. `--rounds 5` matches CI's
sample count. The workload reads a live HTML table collection without painting
SVG or capturing screenshots.

Run the complete, unchanged CI evaluator separately:

```sh
python3 scripts/ci/perf_smoke.py \
  --base /absolute/path/to/base/obscura \
  --candidate /absolute/path/to/candidate/obscura \
  --output /tmp/svg-full-performance.json
```

Run performance comparisons without concurrent builds or tests. Record both
commit hashes, compiler/features, runtime environment, and all raw samples.
A local pass does not erase an earlier hosted failure or establish its cause;
the hosted comparison must also clear the unchanged budget before merging.
