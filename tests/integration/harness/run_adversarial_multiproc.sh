#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PG_PORT="${PG_PORT:-5432}"
NATS_PORT="${NATS_PORT:-4222}"
DB_URL="${DATABASE_URL:-postgres://postgres:obscura@127.0.0.1:${PG_PORT}/obscura}"
NATS_URL="${NATS_URL:-nats://127.0.0.1:${NATS_PORT}}"
BOOTSTRAP="${OBSCURA_SERVICE_BOOTSTRAP_SECRET:-phase5-11-integration-secret}"

cleanup() {
    echo "Cleaning up lingering worker agents and runtime workers..."
    pkill -f obscura-worker-agent 2>/dev/null || true
    pkill -f obscura-runtime-worker 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "=== BUILDING RELEASE BINARIES ==="
cargo build --release -p obscura-worker --bin obscura-runtime-worker
cargo build --release -p obscura-worker-agent
cargo build --release -p obscura-scheduler --example adversarial_e2e

echo "=== RUNNING MULTI-PROCESS ADVERSARIAL E2E SUITE ==="
OBSCURA_ALLOW_PRIVATE_NETWORK="true" \
OBSCURA_SERVICE_BOOTSTRAP_SECRET="$BOOTSTRAP" \
DATABASE_URL="$DB_URL" \
NATS_URL="$NATS_URL" \
cargo run --release -p obscura-scheduler --example adversarial_e2e

echo "=== MULTI-PROCESS ADVERSARIAL SUITE COMPLETED SUCCESSFULLY ==="
