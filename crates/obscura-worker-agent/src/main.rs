use obscura_control_plane::{ControlPlane, NatsClient, NatsConsumer, Principal};
use obscura_scheduler::DispatchMessage;
use obscura_supervisor::{SupervisorConfig, WorkerPoolSupervisor};
use obscura_worker::{NavigationWaitUntil, SessionConfig, WorkerCommand, WorkerRequest};
use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::oneshot;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    // Reaper mode: detached process that survives agent death and kills the
    // worker process group when the agent disappears.
    if std::env::var("OBSCURA_REAPER_MODE").as_deref() == Ok("1") {
        let agent_pid: i32 = std::env::var("OBSCURA_REAPER_AGENT_PID")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let worker_pgid: i32 = std::env::var("OBSCURA_REAPER_WORKER_PGID")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        loop {
            unsafe {
                if agent_pid > 0 && libc::kill(agent_pid, 0) != 0 {
                    if worker_pgid > 0 {
                        unsafe {
                            libc::kill(-worker_pgid, libc::SIGKILL);
                            libc::kill(worker_pgid, libc::SIGKILL);
                        }
                    }
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        std::process::exit(0);
    }

    tracing_subscriber::fmt().with_env_filter("info").init();

    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let worker_bin = std::env::var("OBSCURA_WORKER_BIN")
        .unwrap_or_else(|_| "target/debug/obscura-runtime-worker".to_string());

    let cp = ControlPlane::connect(&db_url, 5).await.expect("cp connect");
    let nats = NatsClient::connect(&nats_url).await.expect("nats connect");
    nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
        .await
        .expect("stream");

    let tenant_id = std::env::var("TENANT_ID")
        .ok()
        .and_then(|s| Uuid::from_str(&s).ok());
    let worker_name = std::env::var("OBSCURA_WORKER_NAME").unwrap_or_else(|_| "worker".to_string());
    let worker_id = cp
        .register_worker(tenant_id, serde_json::json!({"name": worker_name}), 4)
        .await
        .expect("register worker");
    cp.worker_heartbeat(worker_id).await.ok();

    let lease_ttl: i64 = std::env::var("OBSCURA_LEASE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let lease_renew: u64 = std::env::var("OBSCURA_LEASE_RENEW_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let hb_interval: u64 = std::env::var("OBSCURA_HEARTBEAT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    // Start periodic heartbeat task
    let cp_hb = cp.clone();
    let _hb_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(hb_interval)).await;
            if cp_hb.worker_heartbeat(worker_id).await.is_err() {
                tracing::warn!("heartbeat failed");
            }
        }
    });

    let config = SupervisorConfig {
        worker_bin: PathBuf::from(worker_bin),
        pool_size: 1,
        ..Default::default()
    };
    let supervisor = WorkerPoolSupervisor::new(config);
    supervisor.start().await.expect("supervisor start");

    if let Ok(worker) = supervisor.pick_least_loaded() {
        if let Some(pid) = worker.pid {
            let _ = cp.update_worker_child_pid(worker_id, pid).await;
            // Spawn detached reaper for process containment on agent death.
            let agent_pid = std::process::id();
            if let Ok(exe) = std::env::current_exe() {
                let mut cmd = std::process::Command::new(exe);
                cmd.env("OBSCURA_REAPER_MODE", "1")
                    .env("OBSCURA_REAPER_AGENT_PID", agent_pid.to_string())
                    .env("OBSCURA_REAPER_WORKER_PGID", pid.to_string())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit());
                #[cfg(unix)]
                unsafe {
                    cmd.pre_exec(|| {
                        libc::setsid();
                        Ok(())
                    });
                }
                let _ = cmd.spawn();
            }
        }
    }

    // Job-scoped process handle registry
    let job_handles: Arc<StdMutex<HashMap<Uuid, Arc<obscura_supervisor::SupervisedWorker>>>> =
        Arc::new(StdMutex::new(HashMap::new()));

    let consumer_name = std::env::var("NATS_CONSUMER_NAME")
        .unwrap_or_else(|_| format!("worker-agent-{}", worker_id));
    let consumer = NatsConsumer::create(nats.jetstream(), "OBScura_JOBS", &consumer_name)
        .await
        .expect("consumer");

    tracing::info!(worker_id = %worker_id, "worker agent started");

    loop {
        match consumer.next_message().await {
            Ok(Some(msg)) => {
                if let Ok(env) =
                    serde_json::from_slice::<obscura_control_plane::EventEnvelope>(&msg.payload)
                {
                    if env.event_type == "job.dispatch" {
                        if let Ok(dispatch) = serde_json::from_value::<DispatchMessage>(env.payload)
                        {
                            if dispatch.worker_id == worker_id {
                                // Acquire lease before execution
                                let lease = cp
                                    .acquire_lease(
                                        "job",
                                        &dispatch.job_id.to_string(),
                                        worker_id,
                                        lease_ttl,
                                    )
                                    .await;
                                let (lease_id, fencing_token) = match lease {
                                    Ok(l) => (l.lease_id, l.fencing_token),
                                    Err(_) => {
                                        let _ = msg.ack().await;
                                        continue;
                                    }
                                };
                                let _ = cp
                                    .update_job_lease(
                                        dispatch.job_id,
                                        dispatch.tenant_id,
                                        lease_id,
                                        fencing_token,
                                    )
                                    .await;

                                // Pick a worker and register in job-scoped registry
                                let worker = match supervisor.pick_least_loaded() {
                                    Ok(w) => w,
                                    Err(_) => {
                                        let _ = msg.ack().await;
                                        continue;
                                    }
                                };
                                tracing::info!(
                                    worker_id = %worker_id,
                                    job_id = %dispatch.job_id,
                                    child_pid = ?worker.pid,
                                    "starting dispatched job execution"
                                );
                                // Register job-scoped handle
                                {
                                    let mut reg = job_handles.lock().unwrap();
                                    if reg.contains_key(&dispatch.job_id) {
                                        // Duplicate dispatch; skip
                                        let _ = msg.ack().await;
                                        continue;
                                    }
                                    reg.insert(dispatch.job_id, worker.clone());
                                }

                                // ensure session exists
                                let create_req = WorkerRequest {
                                    id: Some(format!("create-{}", dispatch.dispatch_id)),
                                    version: "1.0".to_string(),
                                    command: WorkerCommand::SessionCreate {
                                        session_id: format!("session-{}", dispatch.job_id),
                                        config: SessionConfig::default(),
                                    },
                                };
                                let _ = worker
                                    .send_request(create_req, std::time::Duration::from_secs(10))
                                    .await;

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
                                        tokio::time::sleep(std::time::Duration::from_secs(
                                            lease_renew,
                                        ))
                                        .await;
                                        if cp_renew.renew_lease(lease_id, lease_ttl).await.is_err()
                                        {
                                            let _ = loss_tx.send(());
                                            break;
                                        }
                                    }
                                });

                                let principal = cp
                                    .service_principal(
                                        "worker-agent",
                                        &std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET")
                                            .unwrap_or_default(),
                                    )
                                    .unwrap();
                                // Execute browser with select against lease loss
                                let worker_clone = worker.clone();
                                let exec_future = worker_clone
                                    .send_request(req, std::time::Duration::from_secs(35));
                                tokio::select! {
                                    resp = exec_future => {
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
                                        // Immediately kill process group of child browser worker
                                        #[cfg(unix)]
                                        if let Some(pid) = worker_clone.pid {
                                            unsafe {
                                                libc::kill(-(pid as i32), libc::SIGTERM);
                                                libc::kill(-(pid as i32), libc::SIGKILL);
                                            }
                                        }
                                        // ownership lost: fail job, don't persist success
                                        let _ = cp.fail_job(&principal, dispatch.job_id, dispatch.tenant_id, dispatch.attempt_id, fencing_token, "LEASE_LOST", "ownership lost", true).await;
                                        tracing::warn!(job_id = %dispatch.job_id, child_pid = ?worker_clone.pid, "lease lost during execution; child process group killed");
                                    }
                                }
                                // Remove from registry after execution
                                {
                                    let mut reg = job_handles.lock().unwrap();
                                    reg.remove(&dispatch.job_id);
                                }
                            } else {
                                tracing::debug!(worker_id = %worker_id, target = %dispatch.worker_id, "ignoring dispatch for another worker");
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
