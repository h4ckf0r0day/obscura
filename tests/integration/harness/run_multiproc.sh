#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PG_PORT=5444
NATS_PORT=4232
DB_URL="postgres://postgres:obscura@127.0.0.1:${PG_PORT}/obscura"
NATS_URL="nats://127.0.0.1:${NATS_PORT}"
BOOTSTRAP="testsecret-mp"

cleanup() {
    pkill -f obscura-worker-agent 2>/dev/null || true
    docker rm -f mp-pg mp-nats 2>/dev/null || true
}
trap cleanup EXIT

docker run -d --name mp-pg -e POSTGRES_PASSWORD=obscura -e POSTGRES_DB=obscura -p ${PG_PORT}:5432 postgres:16-alpine >/dev/null 2>&1
docker run -d --name mp-nats -p ${NATS_PORT}:4222 nats:2.10-alpine -js >/dev/null 2>&1
sleep 6

# Migrate
OBSCURA_SERVICE_BOOTSTRAP_SECRET=$BOOTSTRAP DATABASE_URL="$DB_URL" NATS_URL="$NATS_URL" \
  cargo test -p obscura-control-plane test_migrate_and_submit_job 2>&1 | tail -1

# Build binaries
cargo build --release -p obscura-worker-agent 2>&1 | tail -1

# Start Worker Agent A
OBSCURA_WORKER_BIN="$ROOT/target/release/obscura-runtime-worker" \
  OBSCURA_SERVICE_BOOTSTRAP_SECRET=$BOOTSTRAP \
  DATABASE_URL="$DB_URL" NATS_URL="$NATS_URL" \
  ./target/release/obscura-worker-agent > /tmp/wa_a.log 2>&1 &
WA_A=$!
# Start Worker Agent B (different NATS consumer name by using same binary; consumer name hardcoded "worker-agent")
# Note: both agents use same consumer durable name "worker-agent" - this may cause conflict.
# For now, run single agent E2E (multi-agent requires unique consumer names).
sleep 8

# Run E2E
mkdir -p crates/obscura-scheduler/examples
cat > crates/obscura-scheduler/examples/e2e_verify.rs <<'EOF'
use obscura_control_plane::ControlPlane;
use obscura_scheduler::Scheduler;
use obscura_control_plane::NatsClient;

#[tokio::main]
async fn main() {
    let db_url = std::env::var("DATABASE_URL").unwrap();
    let nats_url = std::env::var("NATS_URL").unwrap();
    let cp = ControlPlane::connect(&db_url, 5).await.unwrap();
    cp.migrate().await.unwrap();
    let nats = NatsClient::connect(&nats_url).await.unwrap();
    nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()]).await.unwrap();

    let tenant = cp.create_tenant("e2e-tenant").await.unwrap();
    let job = cp.submit_job_internal(tenant.tenant_id, "data:text/html,<title>E2E Test</title><h1>Hello E2E</h1>", "snapshot", serde_json::json!({}), Some("e2e-key"), 3).await.unwrap();
    println!("JOB_ID={}", job.job_id);

    let sched = Scheduler::new(cp.clone(), nats.clone());
    sched.reconcile().await.unwrap();

    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let j = cp.get_job_internal(job.job_id, tenant.tenant_id).await.unwrap();
        println!("status={}", j.status);
        if j.status == "completed" || j.status == "failed" { break; }
        let _ = sched.schedule_one().await;
    }
    let final_job = cp.get_job_internal(job.job_id, tenant.tenant_id).await.unwrap();
    println!("FINAL_STATUS={}", final_job.status);
}
EOF
OBSCURA_SERVICE_BOOTSTRAP_SECRET=$BOOTSTRAP DATABASE_URL="$DB_URL" NATS_URL="$NATS_URL" \
  cargo run --release -p obscura-scheduler --example e2e_verify 2>&1 | tail -8

kill $WA_PID 2>/dev/null || true
wait $WA_PID 2>/dev/null || true
rm -rf crates/obscura-scheduler/examples
echo "E2E DONE"
