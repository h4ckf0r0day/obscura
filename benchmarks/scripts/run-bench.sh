#!/usr/bin/env bash
set -euo pipefail

# Run the Obscura perf benchmark against a small default URL list and write the
# JSON to results/perf.json. The human-readable table is printed too. Pass extra
# URLs or flags as args and they are forwarded to perf-bench.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OBSCURA_BIN="${OBSCURA_BIN:-obscura}"
RESULTS_DIR="$SCRIPT_DIR/results"
PERF_JSON="$RESULTS_DIR/perf.json"

DEFAULT_URLS=(
  "https://example.com"
  "https://www.iana.org/help/example-domains"
)

mkdir -p "$RESULTS_DIR"

echo "[run-bench] building perf-bench"
( cd "$SCRIPT_DIR" && cargo build --release -p perf-bench )

BENCH="$SCRIPT_DIR/target/release/perf-bench"

# If the caller passed args, use those. Otherwise fall back to the default list.
if [ "$#" -gt 0 ]; then
  ARGS=("$@")
else
  ARGS=("${DEFAULT_URLS[@]}")
fi

export OBSCURA_BIN

echo "[run-bench] running perf-bench"
# Human table to the terminal.
"$BENCH" "${ARGS[@]}"

echo "[run-bench] writing JSON results"
# JSON to disk for tracking over time.
"$BENCH" --json "${ARGS[@]}" >"$PERF_JSON"

echo ""
echo "[run-bench] done."
echo "[run-bench] perf json: $PERF_JSON"
