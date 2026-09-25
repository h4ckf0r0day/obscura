#!/usr/bin/env bash
set -euo pipefail

SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SUITE_DIR/../.." && pwd)"
PAIRED_CORPUS="$REPO_ROOT/render-repros/paired-corpus.py"
SITES="$SUITE_DIR/sites.txt"
OBSCURA="${OBSCURA_BIN:-$REPO_ROOT/target/release/obscura}"
PYTHON="${PYTHON_BIN:-python3}"

usage() {
  echo "usage: $0 OUT_DIR [SCROLL_Y]" >&2
  echo "SCROLL_Y is an integer CSS-pixel offset or 'bottom'." >&2
  echo "Set CAPTURE_MODE=live to leave animations and runtime state unfrozen." >&2
  echo "CAPTURE_MODE defaults to deterministic (Chromium animations sampled at T=0)." >&2
  echo "Representative fidelity runs default to SETTLE_MS=3000." >&2
  echo "For a zero-settle diagnostic, explicitly set SUITE_MODE=latency SETTLE_MS=0." >&2
  echo "Latency mode retains raw images/metrics but excludes them from fidelity evidence." >&2
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi
if (( $# < 1 || $# > 2 )); then
  usage
  exit 2
fi

OUT="$1"
CAPTURE_MODE="${CAPTURE_MODE:-deterministic}"
SUITE_MODE="${SUITE_MODE:-fidelity}"
SETTLE_MS="${SETTLE_MS:-3000}"
if [[ "$CAPTURE_MODE" != "deterministic" && "$CAPTURE_MODE" != "live" ]]; then
  echo "CAPTURE_MODE must be 'deterministic' or 'live': $CAPTURE_MODE" >&2
  exit 2
fi
if [[ "$SUITE_MODE" != "fidelity" && "$SUITE_MODE" != "latency" ]]; then
  echo "SUITE_MODE must be 'fidelity' or 'latency': $SUITE_MODE" >&2
  exit 2
fi
if [[ ! "$SETTLE_MS" =~ ^[0-9]+$ ]] || (( SETTLE_MS % 1000 != 0 )); then
  echo "SETTLE_MS must be a non-negative whole number of seconds in milliseconds: $SETTLE_MS" >&2
  exit 2
fi
if [[ "$SETTLE_MS" == "0" && "$SUITE_MODE" != "latency" ]]; then
  echo "SETTLE_MS=0 requires explicit SUITE_MODE=latency" >&2
  exit 2
fi
if [[ "$SUITE_MODE" == "latency" && "$SETTLE_MS" != "0" ]]; then
  echo "SUITE_MODE=latency requires SETTLE_MS=0" >&2
  exit 2
fi
if [[ "$SUITE_MODE" == "latency" ]]; then
  CAPTURE_PURPOSE="cold-load-latency"
else
  CAPTURE_PURPOSE="representative-fidelity"
fi
if [[ -e "$OUT" ]]; then
  echo "output path already exists: $OUT" >&2
  exit 2
fi
if [[ ! -x "$OBSCURA" ]]; then
  echo "Obscura binary is not executable: $OBSCURA" >&2
  exit 2
fi

args=(
  "$PAIRED_CORPUS"
  "$SITES"
  --obscura-bin "$OBSCURA"
  --out "$OUT"
  --width 1440
  --height 1000
  --settle-ms "$SETTLE_MS"
  --capture-purpose "$CAPTURE_PURPOSE"
  --geometry-selector "header, nav, footer"
  --geometry-selector "main, article"
  --geometry-selector "section"
  --geometry-selector "form, fieldset, input, button, select, textarea"
  --geometry-selector "img, svg, video, canvas"
  --geometry-selector "pre"
  --geometry-selector "table"
)

if [[ "$CAPTURE_MODE" == "deterministic" ]]; then
  args+=(--animation-time-ms 0)
fi

if [[ -n "${CHROMIUM_BIN:-}" ]]; then
  args+=(--chromium-bin "$CHROMIUM_BIN")
fi
if [[ -n "${BASELINE_BIN:-}" ]]; then
  args+=(--baseline-bin "$BASELINE_BIN")
fi
if (( $# == 2 )); then
  args+=(--scroll-y "$2")
fi

exec env PYTHONDONTWRITEBYTECODE=1 "$PYTHON" "${args[@]}"
