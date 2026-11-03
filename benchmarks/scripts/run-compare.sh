#!/usr/bin/env bash
set -euo pipefail

# Head-to-head: obscura vs headless Chrome (speed + memory), and throughput /
# concurrency scaling. Runs cold processes for both engines and reports latency,
# peak RSS, pages/sec.
#
# Env:
#   OBSCURA_BIN   path to the obscura binary (default: obscura on PATH)
#   CHROME_BIN    path to chrome/chromium (default: google-chrome)
# For accurate numbers, run on an otherwise idle host (CPU contention skews
# throughput). Extra args are forwarded to the underlying scripts.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export OBSCURA_BIN="${OBSCURA_BIN:-obscura}"
export CHROME_BIN="${CHROME_BIN:-google-chrome}"
RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

echo "[compare] obscura: $OBSCURA_BIN   chrome: $CHROME_BIN"
echo "[compare] === head-to-head (latency + memory) ==="
python3 "$SCRIPT_DIR/compare/head-to-head.py" "$@"
python3 "$SCRIPT_DIR/compare/head-to-head.py" --json >"$RESULTS_DIR/head-to-head.json" 2>/dev/null || true

echo ""
echo "[compare] === throughput + concurrency scaling ==="
python3 "$SCRIPT_DIR/compare/scale.py"
python3 "$SCRIPT_DIR/compare/scale.py" --json >"$RESULTS_DIR/scale.json" 2>/dev/null || true

echo ""
echo "[compare] json: $RESULTS_DIR/head-to-head.json , $RESULTS_DIR/scale.json"
