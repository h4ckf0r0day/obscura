#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PG_PORT=5442
NATS_PORT=4230
DB_URL="postgres://postgres:obscura@127.0.0.1:${PG_PORT}/obscura"
NATS_URL="nats://127.0.0.1:${NATS_PORT}"
BOOTSTRAP="testsecret-$(date +%s)"

cleanup() {
    pkill -f obscura-worker-agent 2>/dev/null || true
    docker rm -f f5h-pg f5h-nats 2>/dev/null || true
}
trap cleanup EXIT

# Start PG
docker run -d --name f5h-pg -e POSTGRES_PASSWORD=obscura -e POSTGRES_DB=obscura -p ${PG_PORT}:5432 postgres:16-alpine >/dev/null 2>&1
# Start NATS
docker run -d --name f5h-nats -p ${NATS_PORT}:4222 nats:2.10-alpine -js >/dev/null 2>&1
sleep 6

# Migrate
OBSCURA_SERVICE_BOOTSTRAP_SECRET=$BOOTSTRAP DATABASE_URL="$DB_URL" NATS_URL="$NATS_URL" \
  cargo test -p obscura-control-plane test_migrate_and_submit_job 2>&1 | tail -2

# Start worker agent A
OBSCURA_WORKER_BIN="$ROOT/target/release/obscura-runtime-worker" \
  OBSCURA_SERVICE_BOOTSTRAP_SECRET=$BOOTSTRAP \
  DATABASE_URL="$DB_URL" NATS_URL="$NATS_URL" \
  TENANT_ID="" \
  ./target/release/obscura-worker-agent > /tmp/wa.log 2>&1 &
WA_PID=$!
sleep 8

# Run scheduler E2E example
OBSCURA_SERVICE_BOOTSTRAP_SECRET=$BOOTSTRAP DATABASE_URL="$DB_URL" NATS_URL="$NATS_URL" \
  cargo run --release -p obscura-scheduler --example e2e_verify 2>&1 | tail -5

# Cleanup
kill $WA_PID 2>/dev/null || true
wait $WA_PID 2>/dev/null || true
echo "E2E DONE"
