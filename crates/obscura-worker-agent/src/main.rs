use async_nats::jetstream::Context;
use obscura_control_plane::{ControlPlane, NatsClient, NatsConsumer, Principal};
use obscura_scheduler::DispatchMessage;
use obscura_supervisor::{SupervisorConfig, WorkerPoolSupervisor};
use obscura_worker::{WorkerCommand, WorkerRequest};
use std::path::PathBuf;
use std::str::FromStr;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let worker_bin = std::env::var("OBSCURA_WORKER_BIN")
        .unwrap_or_else(|_| "target/debug/obscura-runtime-worker".to_string());

    let cp = ControlPlane::connect(&db_url, 5).await.expect("cp connect");
    let nats = NatsClient::connect(&nats_url).await.expect("nats connect");
    nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
        .await
        .expect("stream");

    let tenant_id = std::env::var("TENANT_ID").ok().and_then(|s| Uuid::from_str(&s).ok());
    let worker_id = cp.register_worker(tenant_id, serde_json::json!({}), 4).await.expect("register worker");
    cp.worker_heartbeat(worker_id).await.ok();

    // supervisor pool
    let config = SupervisorConfig {
        worker_bin: PathBuf::from(worker_bin),
        pool_size: 1,
        ..Default::default()
    };
    let supervisor = WorkerPoolSupervisor::new(config);
    supervisor.start().await.expect("supervisor start");

    // consumer for dispatch
    let consumer = NatsConsumer::create(nats.jetstream(), "OBScura_JOBS", "worker-agent")
        .await
        .expect("consumer");

    tracing::info!(worker_id = %worker_id, "worker agent started");

    loop {
        match consumer.next_message().await {
            Ok(Some(msg)) => {
                // parse envelope
                if let Ok(env) = serde_json::from_slice::<obscura_control_plane::EventEnvelope>(&msg.payload) {
                    if env.event_type == "job.dispatch" {
                        if let Ok(dispatch) = serde_json::from_value::<DispatchMessage>(env.payload) {
                            if dispatch.worker_id == worker_id {
                                // execute
                                let req = WorkerRequest {
                                    id: Some(dispatch.dispatch_id.to_string()),
                                    version: "1.0".to_string(),
                                    command: obscura_worker::WorkerCommand::Navigate {
                                        session_id: format!("session-{}", dispatch.job_id),
                                        url: dispatch.url.clone(),
                                        wait_until: obscura_worker::NavigationWaitUntil::Load,
                                        timeout_ms: Some(30000),
                                    },
                                };
                                // ensure session exists
                                let create_req = WorkerRequest {
                                    id: Some(format!("create-{}", dispatch.dispatch_id)),
                                    version: "1.0".to_string(),
                                    command: obscura_worker::WorkerCommand::SessionCreate {
                                        session_id: format!("session-{}", dispatch.job_id),
                                        config: obscura_worker::SessionConfig::default(),
                                    },
                                };
                                let _ = supervisor.dispatch(create_req).await;
                                let resp = supervisor.dispatch(req).await;
                                match resp {
                                    Ok(r) if r.ok => {
                                        cp.complete_job(&cp.service_principal("worker-agent"), dispatch.job_id, dispatch.tenant_id, dispatch.attempt_id, dispatch.fencing_token, r.result.unwrap_or(serde_json::json!({}))).await.ok();
                                    }
                                    _ => {
                                        cp.fail_job(&cp.service_principal("worker-agent"), dispatch.job_id, dispatch.tenant_id, dispatch.attempt_id, dispatch.fencing_token, "EXEC_ERROR", "execution failed", true).await.ok();
                                    }
                                }
                            }
                        }
                    }
                }
                let _ = msg.ack().await;
            }
            Ok(None) => {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "consumer error");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        cp.worker_heartbeat(worker_id).await.ok();
    }
}
