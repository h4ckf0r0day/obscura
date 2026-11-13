#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${ROOT_DIR}/target/release/obscura"

if [ ! -f "$BIN" ]; then
    echo "Binary not found at $BIN. Building now..."
    source "$HOME/.cargo/env" 2>/dev/null || true
    CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release -p obscura-cli --bins --no-default-features
fi

echo "Running Obscura Obstacle Course Gate against $BIN..."
OBSCURA_BIN="$BIN" python3 "${ROOT_DIR}/benchmarks/obstacle-course/run.py" --runs 1 --warmup 0

