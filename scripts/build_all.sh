#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

echo "Building all workspace crates and release binaries..."
source "$HOME/.cargo/env" 2>/dev/null || true

CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo build --release --workspace

echo "All workspace binaries built successfully!"
ls -lh target/release/obscura target/release/obscura-worker target/release/obscura-runtime-worker 2>/dev/null || true

