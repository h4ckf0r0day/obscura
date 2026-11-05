use obscura_control_plane::{ControlPlane, NatsClient, NatsConsumer, Principal};
use obscura_scheduler::DispatchMessage;
use obscura_supervisor::{SupervisorConfig, WorkerPoolSupervisor};
use obscura_worker::{WorkerCommand, WorkerRequest, NavigationWaitUntil, SessionConfig};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::oneshot;
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

    let config = SupervisorConfig {
        worker_bin: PathBuf::from(worker_bin),
        pool_size: 1,
        ..Default::default()
    };
    let supervisor = WorkerPoolSupervisor::new(config);
    supervisor.start().await.expect("supervisor start");

    let consumer = NatsConsumer::create(nats.jetstream(), "OBScura_JOBS", "worker-agent")
        .await
        .expect("consumer");

    tracing::info!(worker_id = %worker_id, "worker agent started");

    loop {
        match consumer.next_message().await {
            Ok(Some(msg)) => {
                if let Ok(env) = serde_json::from_slice::<obscura_control_plane::EventEnvelope>(&msg.payload) {
                    if env.event_type == "job.dispatch" {
                        if let Ok(dispatch) = serde_json::from_value::<DispatchMessage>(env.payload) {
                            if dispatch.worker_id == worker_id {
                                // Acquire lease before execution
                                let lease = cp.acquire_lease("job", &dispatch.job_id.to_string(), worker_id, 30).await;
                                let (lease_id, fencing_token) = match lease {
                                    Ok(l) => (l.lease_id, l.fencing_token),
                                    Err(_) => {
                                        let _ = msg.ack().await;
                                        continue;
                                    }
                                };
                                let _ = cp.update_job_fencing_token(dispatch.job_id, dispatch.tenant_id, fencing_token).await;

                                // ensure session exists
                                let create_req = WorkerRequest {
                                    id: Some(format!("create-{}", dispatch.dispatch_id)),
                                    version: "1.0".to_string(),
                                    command: WorkerCommand::SessionCreate {
                                        session_id: format!("session-{}", dispatch.job_id),
                                        config: SessionConfig::default(),
                                    },
                                };
                                let _ = supervisor.dispatch(create_req).await;

                                let req = WorkerRequest {
                                    id: Some(dispatch.dispatch_id.to_string()),
                                    version: "1.0".to_string(),
                                    command: WorkerCommand::Navigate {
                                        session_id: format!("session-{}", dispatch.job_id),
                                        url: dispatch.url.clone(),
                                        wait_until: NavigationWaitUntil::Load,
                                        timeout_ms: Some(30000),
                                    },
                                };

                                // Renewal channel
                                let (loss_tx, mut loss_rx) = oneshot::channel::<()>();
                                let cp_renew = cp.clone();
                                let renew_handle = tokio::spawn(async move {
                                    loop {
                                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                        if cp_renew.renew_lease(lease_id, 30).await.is_err() {
                                            let _ = loss_tx.send(());
                                            break;
                                        }
                                    }
                                });

                                let principal = cp.service_principal("worker-agent", &std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET").unwrap_or_default()).unwrap();
                                // Execute browser with select against lease loss
                                let exec_future = supervisor.dispatch(req);
                                tokio::select! {
                                    resp = exec_future => {
                                        // cancel renewal
                                        renew_handle.abort();
                                        match resp {
                                            Ok(r) if r.ok => {
                                                let _ = cp.complete_job(&principal, dispatch.job_id, dispatch.tenant_id, dispatch.attempt_id, fencing_token, r.result.unwrap_or(serde_json::json!({}))).await;
                                            }
                                            _ => {
                                                let _ = cp.fail_job(&principal, dispatch.job_id, dispatch.tenant_id, dispatch.attempt_id, fencing_token, "EXEC_ERROR", "execution failed", true).await;
                                            }
                                        }
                                    }
                                    _ = &mut loss_rx => {
                                        renew_handle.abort();
                                        // ownership lost: fail job, don't persist success
                                        let _ = cp.fail_job(&principal, dispatch.job_id, dispatch.tenant_id, dispatch.attempt_id, fencing_token, "LEASE_LOST", "ownership lost", true).await;
                                        tracing::warn!(job_id = %dispatch.job_id, "lease lost during execution");
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
