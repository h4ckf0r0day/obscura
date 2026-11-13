#!/usr/bin/env bash
set -euo pipefail

echo "=========================================================="
echo "      Obscura Next: Autonomous Platform Bootstrap        "
echo "=========================================================="

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"

# 1. Environment & Toolchain Check
echo "--> Checking Rust toolchain..."
if ! command -v rustc &>/dev/null; then
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    else
        echo "Error: Rust toolchain not found." >&2
        exit 1
    fi
fi

rustc --version
cargo --version

# 2. Build release binary
echo "--> Building Obscura Next binaries..."
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release -p obscura-cli --bins --no-default-features

# 3. Verify Obstacle Course (Authoritative Gate)
echo "--> Running 33-Stage Obstacle Course Gate..."
OBSCURA_BIN="${ROOT_DIR}/target/release/obscura" python3 benchmarks/obstacle-course/run.py --runs 1 --warmup 0

echo "=========================================================="
echo "    Obscura Next Bootstrap & Verification COMPLETE!       "
echo "=========================================================="

