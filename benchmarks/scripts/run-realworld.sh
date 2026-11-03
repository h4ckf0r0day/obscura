#!/usr/bin/env bash
set -euo pipefail

# Real-world render-success benchmark: fetch a corpus of live public pages with
# obscura and report how many render to a usable document, plus latency + memory.
#
# Env:
#   OBSCURA_BIN   path to the obscura binary (default: obscura on PATH)
# Needs outbound network. The live web drifts, so results are a snapshot.
# Extra args are forwarded to run.py (e.g. --wait 6, --sites my-list.txt).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export OBSCURA_BIN="${OBSCURA_BIN:-obscura}"
RESULTS_DIR="$SCRIPT_DIR/results"
mkdir -p "$RESULTS_DIR"

echo "[realworld] obscura: $OBSCURA_BIN"
python3 "$SCRIPT_DIR/realworld/run.py" "$@"

if [[ " $* " != *" --json "* ]]; then
  python3 "$SCRIPT_DIR/realworld/run.py" --json >"$RESULTS_DIR/realworld.json" 2>/dev/null || true
  echo "[realworld] json: $RESULTS_DIR/realworld.json"
fi
