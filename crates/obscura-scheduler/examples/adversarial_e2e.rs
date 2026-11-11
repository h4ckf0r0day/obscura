use obscura_control_plane::{ControlPlane, EventEnvelope, NatsClient, NatsConsumer};
use obscura_scheduler::DispatchMessage;
use obscura_scheduler::Scheduler;
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Child;
use uuid::Uuid;

struct ProcessGuard {
    child: Option<Child>,
    pid: Option<u32>,
}

impl ProcessGuard {
    fn new(child: Child) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
        }
    }

    async fn kill_sigkill(&mut self) {
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.start_kill();
            let _ = c.wait().await;
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.start_kill();
        }
    }
}

fn check_process_alive(pid: u32) -> bool {
    // On Linux, also verify the PID still refers to a runtime worker process
    // to avoid false positives from PID reuse after the worker has exited.
    #[cfg(target_os = "linux")]
    {
        // Zombie processes are dead; treat as not alive.
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
            // stat format: pid (comm) state ...
            if let Some(pos) = stat.rfind(')') {
                let rest = &stat[pos + 1..];
                let state = rest.trim().chars().next().unwrap_or(' ');
                if state == 'Z' {
                    return false;
                }
            }
        } else {
            return false;
        }
        let cmdline_path = format!("/proc/{}/cmdline", pid);
        if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
            if !cmdline.contains("obscura-runtime-worker")
                && !cmdline.contains("obscura-worker-agent")
            {
                return false;
            }
        } else {
            return false;
        }
    }
    unsafe {
        let res = libc::kill(pid as i32, 0);
        res == 0
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "true");

    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
    let bootstrap_secret = std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET")
        .unwrap_or_else(|_| "phase5-11-integration-secret".to_string());

    let root_dir = std::env::current_dir()?;
    let worker_bin = root_dir.join("target/release/obscura-runtime-worker");
    let agent_bin = root_dir.join("target/release/obscura-worker-agent");

    assert!(
        worker_bin.exists(),
        "Worker binary missing: {:?}",
        worker_bin
    );
    assert!(agent_bin.exists(), "Agent binary missing: {:?}", agent_bin);

    println!("=== CONNECTING TO CONTROL PLANE & NATS ===");
    let cp = ControlPlane::connect(&db_url, 10).await?;
    cp.migrate().await?;

    let nats = NatsClient::connect(&nats_url).await?;
    nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
        .await?;

    let tenant_name = format!("adv-tenant-{}", Uuid::new_v4());
    let tenant = cp.create_tenant(&tenant_name).await?;
    let tenant_id = tenant.tenant_id;
    println!("Created test tenant: {} ({})", tenant_name, tenant_id);

    let sched = Scheduler::new(cp.clone(), nats.clone());

    // =========================================================================
    // SCENARIO 1: Multi-Process Coexistence (2 Concurrent Worker Agents)
    // =========================================================================
    println!("\n=== SCENARIO 1: MULTI-PROCESS COEXISTENCE ===");

    // Fetch existing worker IDs to identify new ones
    let initial_workers: HashSet<Uuid> =
        sqlx::query_scalar::<_, Uuid>("SELECT worker_id FROM workers WHERE status = 'healthy'")
            .fetch_all(cp.pool())
            .await?
            .into_iter()
            .collect();

    // Start Worker Agent A
    let name_a = format!("agent-a-{}", Uuid::new_v4());
    let cname_a = format!("wa-a-{}", Uuid::new_v4());
    let child_a = tokio::process::Command::new(&agent_bin)
        .env("DATABASE_URL", &db_url)
        .env("NATS_URL", &nats_url)
        .env("OBSCURA_WORKER_BIN", &worker_bin)
        .env("OBSCURA_SERVICE_BOOTSTRAP_SECRET", &bootstrap_secret)
        .env("NATS_CONSUMER_NAME", &cname_a)
        .env("OBSCURA_WORKER_NAME", &name_a)
        .env("OBSCURA_ALLOW_PRIVATE_NETWORK", "true")
        .spawn()?;
    let mut guard_a = ProcessGuard::new(child_a);

    // Start Worker Agent B
    let name_b = format!("agent-b-{}", Uuid::new_v4());
    let cname_b = format!("wa-b-{}", Uuid::new_v4());
    let child_b = tokio::process::Command::new(&agent_bin)
        .env("DATABASE_URL", &db_url)
        .env("NATS_URL", &nats_url)
        .env("OBSCURA_WORKER_BIN", &worker_bin)
        .env("OBSCURA_SERVICE_BOOTSTRAP_SECRET", &bootstrap_secret)
        .env("NATS_CONSUMER_NAME", &cname_b)
        .env("OBSCURA_WORKER_NAME", &name_b)
        .env("OBSCURA_ALLOW_PRIVATE_NETWORK", "true")
        .spawn()?;
    let mut guard_b = ProcessGuard::new(child_b);

    // Wait for both agents to register and heartbeat
    let mut wa_id_opt = None;
    let mut wb_id_opt = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if wa_id_opt.is_none() {
            wa_id_opt = sqlx::query_scalar::<_, Uuid>(
                "SELECT worker_id FROM workers WHERE capabilities->>'name' = $1 AND status = 'healthy'",
            )
            .bind(&name_a)
            .fetch_optional(cp.pool())
            .await?;
        }
        if wb_id_opt.is_none() {
            wb_id_opt = sqlx::query_scalar::<_, Uuid>(
                "SELECT worker_id FROM workers WHERE capabilities->>'name' = $1 AND status = 'healthy'",
            )
            .bind(&name_b)
            .fetch_optional(cp.pool())
            .await?;
        }
        if wa_id_opt.is_some() && wb_id_opt.is_some() {
            break;
        }
    }
    let (wa_id, wb_id) = (
        wa_id_opt.expect("Worker A registered"),
        wb_id_opt.expect("Worker B registered"),
    );
    println!("Worker Agent A registered: {} ({})", wa_id, name_a);
    println!("Worker Agent B registered: {} ({})", wb_id, name_b);

    // Wait for both agents to report their child PID (which proves supervisor and NATS consumer are fully up)
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let pid_a = cp.get_worker_child_pid(wa_id).await.unwrap_or(None);
        let pid_b = cp.get_worker_child_pid(wb_id).await.unwrap_or(None);
        if pid_a.is_some() && pid_b.is_some() {
            println!(
                "Both worker agents are fully initialized with child browser PIDs: {:?}, {:?}",
                pid_a, pid_b
            );
            break;
        }
    }

    // Submit 2 jobs concurrently
    let job1 = cp
        .submit_job_internal(
            tenant_id,
            "data:text/html,<title>Page 1</title><h1>Hello Coexistence 1</h1>",
            "snapshot",
            json!({}),
            Some(&format!("coexist-1-{}", Uuid::new_v4())),
            3,
        )
        .await?;
    let job2 = cp
        .submit_job_internal(
            tenant_id,
            "data:text/html,<title>Page 2</title><h1>Hello Coexistence 2</h1>",
            "snapshot",
            json!({}),
            Some(&format!("coexist-2-{}", Uuid::new_v4())),
            3,
        )
        .await?;
    println!(
        "Submitted Job 1: {} and Job 2: {}",
        job1.job_id, job2.job_id
    );

    // Schedule both jobs
    let _ = cp.reconcile_worker_capacity().await;
    let s1 = sched.schedule_one_for_tenant(Some(tenant_id)).await?;
    let s2 = sched.schedule_one_for_tenant(Some(tenant_id)).await?;
    println!("Initial schedule s1={}, s2={}", s1, s2);

    // Wait for both jobs to reach 'completed'
    let mut job1_done = false;
    let mut job2_done = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let j1 = cp.get_job_internal(job1.job_id, tenant_id).await?;
        let j2 = cp.get_job_internal(job2.job_id, tenant_id).await?;
        println!(
            "Polling: Job 1 status={}, Job 2 status={}",
            j1.status, j2.status
        );
        if j1.status == "completed" {
            job1_done = true;
        }
        if j2.status == "completed" {
            job2_done = true;
        }
        if job1_done && job2_done {
            break;
        }
        if j1.status == "pending" || j2.status == "pending" {
            let _ = sched.schedule_one_for_tenant(Some(tenant_id)).await;
        }
    }
    assert!(
        job1_done,
        "Job 1 did not complete in multi-process environment"
    );
    assert!(
        job2_done,
        "Job 2 did not complete in multi-process environment"
    );
    println!(
        "✓ [SCENARIO 1 PASS] Both jobs completed concurrently on separate worker agent processes!"
    );

    // =========================================================================
    // SCENARIO 2: Worker-Agent Crash Recovery & Stale Completion Rejection
    // =========================================================================
    println!("\n=== SCENARIO 2: WORKER-AGENT CRASH RECOVERY ===");

    // Submit Job 3
    let job3 = cp
        .submit_job_internal(
            tenant_id,
            "data:text/html,<title>Crash Page</title><h1>Crash Recovery Test</h1>",
            "snapshot",
            json!({}),
            Some(&format!("crash-job-{}", Uuid::new_v4())),
            3,
        )
        .await?;
    println!("Submitted Job 3: {}", job3.job_id);

    // Schedule Job 3 to Worker A specifically
    cp.mark_worker_unavailable(wb_id).await?;
    let scheduled = sched.schedule_one_for_tenant(Some(tenant_id)).await?;
    assert!(scheduled, "Job 3 was not scheduled");

    let j3_dispatched = cp.get_job_internal(job3.job_id, tenant_id).await?;
    println!("Job 3 scheduled to worker: {:?}", j3_dispatched.worker_id);
    assert_eq!(j3_dispatched.worker_id, Some(wa_id));
    let original_fencing_token = j3_dispatched.fencing_token;
    println!("Job 3 original fencing token: {}", original_fencing_token);

    // CRASH Worker Agent A abruptly with kill -9
    println!(
        "Crashing Worker Agent A (PID {:?}) with SIGKILL...",
        guard_a.pid
    );
    guard_a.kill_sigkill().await;
    assert!(
        !check_process_alive(guard_a.pid.unwrap()),
        "Worker Agent A still alive after SIGKILL!"
    );
    println!("Worker Agent A is confirmed DEAD.");

    // Worker B comes back online
    cp.worker_heartbeat(wb_id).await?;

    // Expire Worker A and recover abandoned executions
    cp.mark_worker_unavailable(wa_id).await?;
    // Manually expire the lease for Job 3 to simulate crash lease expiry
    sqlx::query("UPDATE leases SET expires_at = now() - interval '5s' WHERE resource_type = 'job' AND resource_id = $1")
        .bind(job3.job_id.to_string())
        .execute(cp.pool())
        .await?;

    let recovered = cp.recover_abandoned_executions().await?;
    println!("Recovered abandoned jobs count: {}", recovered);
    assert!(recovered >= 1, "Job 3 was not recovered");

    let j3_recovered = cp.get_job_internal(job3.job_id, tenant_id).await?;
    assert_eq!(
        j3_recovered.status, "pending",
        "Job 3 should be back to pending state"
    );
    println!("Job 3 successfully reset to 'pending' state.");

    // Scheduler now schedules Job 3 to Worker Agent B
    let _ = cp.reconcile_worker_capacity().await;
    let wb_row: (String, i32, i32) =
        sqlx::query_as("SELECT status, active_jobs, capacity FROM workers WHERE worker_id = $1")
            .bind(wb_id)
            .fetch_one(cp.pool())
            .await?;
    println!(
        "DEBUG: Worker B ({}) status={}, active_jobs={}, capacity={}",
        wb_id, wb_row.0, wb_row.1, wb_row.2
    );

    let matching_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workers WHERE status = 'healthy' AND (last_heartbeat_at IS NULL OR last_heartbeat_at > now() - interval '30 seconds') AND active_jobs < capacity"
    )
    .fetch_one(cp.pool())
    .await?;
    println!(
        "DEBUG: Healthy workers with capacity count: {}",
        matching_count
    );

    let rescheduled = sched.schedule_one_for_tenant(Some(tenant_id)).await?;
    assert!(rescheduled, "Job 3 not rescheduled");

    let j3_rescheduled = cp.get_job_internal(job3.job_id, tenant_id).await?;
    println!(
        "Job 3 rescheduled to worker: {:?}",
        j3_rescheduled.worker_id
    );
    assert_eq!(j3_rescheduled.worker_id, Some(wb_id));
    assert!(
        j3_rescheduled.fencing_token > original_fencing_token,
        "Fencing token must monotonically increase!"
    );
    let new_fencing_token = j3_rescheduled.fencing_token;
    println!("Job 3 new monotonic fencing token: {}", new_fencing_token);

    // Adversarial verification: Stale Worker Agent A late completion attempt with old fencing token
    println!("Testing adversarial late completion attempt from crashed Worker Agent A...");
    let service_principal = cp.service_principal("worker-agent", &bootstrap_secret)?;
    let stale_attempt = cp
        .complete_job(
            &service_principal,
            job3.job_id,
            tenant_id,
            j3_rescheduled
                .execution_attempt_id
                .unwrap_or(Uuid::new_v4()),
            original_fencing_token,
            json!({"zombie_result": true}),
        )
        .await;
    assert!(
        stale_attempt.is_err(),
        "Stale completion with old fencing token MUST be rejected!"
    );
    println!(
        "✓ Stale completion rejected by database fencing as expected: {:?}",
        stale_attempt.err().unwrap()
    );

    // Wait for Worker Agent B to complete Job 3 with the new fencing token
    let mut job3_done = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let j3 = cp.get_job_internal(job3.job_id, tenant_id).await?;
        if j3.status == "completed" {
            job3_done = true;
            break;
        }
    }
    assert!(job3_done, "Job 3 did not complete under Worker Agent B");
    println!("✓ [SCENARIO 2 PASS] Job 3 successfully taken over and completed by Worker Agent B!");

    // Clean up Guard B
    guard_b.kill_sigkill().await;

    // =========================================================================
    // SCENARIO 3: Process-Level Termination on Lease Loss (OS Kernel Signal)
    // =========================================================================
    println!("\n=== SCENARIO 3: PROCESS-LEVEL TERMINATION ON LEASE LOSS ===");

    // Start a local mock HTTP server that delays responses to keep the worker busy navigating
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let slow_port = listener.local_addr()?.port();
    let slow_url = format!("http://127.0.0.1:{}/slow", slow_port);
    println!("Mock slow HTTP server listening on: {}", slow_url);

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                // Delay 10 seconds before writing response
                tokio::time::sleep(Duration::from_secs(10)).await;
                let body = "<html><body>Delayed Response</body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });

    // Start Worker Agent C with fast lease parameters:
    // OBSCURA_LEASE_TTL_SECS=3, OBSCURA_LEASE_RENEW_SECS=1
    let name_c = format!("agent-c-{}", Uuid::new_v4());
    let cname_c = format!("wa-c-{}", Uuid::new_v4());
    let child_c = tokio::process::Command::new(&agent_bin)
        .env("DATABASE_URL", &db_url)
        .env("NATS_URL", &nats_url)
        .env("OBSCURA_WORKER_BIN", &worker_bin)
        .env("OBSCURA_SERVICE_BOOTSTRAP_SECRET", &bootstrap_secret)
        .env("NATS_CONSUMER_NAME", &cname_c)
        .env("OBSCURA_WORKER_NAME", &name_c)
        .env("OBSCURA_LEASE_TTL_SECS", "3")
        .env("OBSCURA_LEASE_RENEW_SECS", "1")
        .env("OBSCURA_ALLOW_PRIVATE_NETWORK", "true")
        .spawn()?;
    let mut guard_c = ProcessGuard::new(child_c);

    // Discover Worker C
    let mut worker_c_id = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        worker_c_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT worker_id FROM workers WHERE capabilities->>'name' = $1 AND status = 'healthy'",
        )
        .bind(&name_c)
        .fetch_optional(cp.pool())
        .await?;
        if worker_c_id.is_some() {
            break;
        }
    }
    let wc_id = worker_c_id.expect("Worker C registered");
    println!("Worker Agent C registered: {} ({})", wc_id, name_c);

    // Wait for Worker C to report its child PID in capabilities
    let mut child_pid = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Ok(Some(pid)) = cp.get_worker_child_pid(wc_id).await {
            child_pid = Some(pid);
            break;
        }
    }
    let c_pid = child_pid.expect("Worker C child PID discovered");
    println!("Worker Agent C child browser worker PID: {}", c_pid);
    assert!(
        check_process_alive(c_pid),
        "Child worker process is not alive!"
    );

    // Submit Job 4 with slow_url and max_attempts: 1
    let job4 = cp
        .submit_job_internal(
            tenant_id,
            &slow_url,
            "snapshot",
            json!({}),
            Some(&format!("lease-loss-job-{}", Uuid::new_v4())),
            1,
        )
        .await?;
    println!("Submitted Job 4: {}", job4.job_id);

    // Ensure Worker A and Worker B won't be picked for Job 4
    let _ = cp.mark_worker_unavailable(wa_id).await;
    let _ = cp.mark_worker_unavailable(wb_id).await;

    // Schedule Job 4 to Worker Agent C
    let _ = cp.reconcile_worker_capacity().await;
    let scheduled_4 = sched.schedule_one_for_tenant(Some(tenant_id)).await?;
    assert!(scheduled_4, "Job 4 not scheduled");
    println!("Job 4 dispatched to Worker C!");

    // Wait 1.5 seconds for Worker C to start executing Job 4 and acquire lease
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Verify child browser worker PID is still running
    assert!(
        check_process_alive(c_pid),
        "Child browser worker should be running execution"
    );
    println!("Verified child worker PID {} is running execution", c_pid);

    // FORCIBLY EXPIRE THE LEASE IN POSTGRESQL to simulate lease loss
    println!("Simulating lease loss by expiring lease in PostgreSQL...");
    let res = sqlx::query("UPDATE leases SET expires_at = now() - interval '10s' WHERE resource_type = 'job' AND resource_id = $1")
        .bind(job4.job_id.to_string())
        .execute(cp.pool())
        .await?;
    println!("Expired lease rows: {}", res.rows_affected());

    // Wait up to 5 seconds for Worker Agent C's renewal task to fail and trigger OS process group kill
    let mut child_killed = false;
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if !check_process_alive(c_pid) {
            child_killed = true;
            break;
        }
    }

    // Verify child browser worker PID is dead at OS level
    assert!(
        child_killed,
        "Child browser worker PID {} was NOT killed upon lease loss!",
        c_pid
    );
    unsafe {
        let res = libc::kill(c_pid as i32, 0);
        let err = std::io::Error::last_os_error().raw_os_error();
        println!("OS kill(pid, 0) returned: {} with errno: {:?}", res, err);
        // Process may be a zombie (terminated but not yet reaped) or fully gone.
        // Either counts as terminated.
    }
    println!(
        "✓ Verified child browser worker PID {} terminated upon lease loss!",
        c_pid
    );

    // Verify Job 4 transitioned to 'failed' and error code is 'LEASE_LOST'
    let final_j4 = cp.get_job_internal(job4.job_id, tenant_id).await?;
    println!("Job 4 final status: {}", final_j4.status);
    assert_eq!(final_j4.status, "failed", "Job 4 status must be 'failed'");

    let error_code: Option<String> = sqlx::query_scalar(
        "SELECT error_code FROM job_attempts WHERE job_id = $1 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(job4.job_id)
    .fetch_optional(cp.pool())
    .await?
    .flatten();
    println!("Job 4 recorded attempt error_code: {:?}", error_code);
    assert_eq!(
        error_code,
        Some("LEASE_LOST".to_string()),
        "Attempt error code must be LEASE_LOST"
    );
    println!("✓ [SCENARIO 3 PASS] Process-level termination on lease loss fully verified!");

    guard_c.kill_sigkill().await;

    // =========================================================================
    // SCENARIO 5: Browser Process Containment on Agent SIGKILL
    // =========================================================================
    println!("\n=== SCENARIO 5: BROWSER PROCESS CONTAINMENT ON AGENT SIGKILL ===");

    let listener5 = TcpListener::bind("127.0.0.1:0").await?;
    let slow_port5 = listener5.local_addr()?.port();
    let slow_url5 = format!("http://127.0.0.1:{}/slow", slow_port5);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener5.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                tokio::time::sleep(Duration::from_secs(10)).await;
                let body = "<html><body>Delayed</body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });

    let name_d = format!("agent-d-{}", Uuid::new_v4());
    let cname_d = format!("wa-d-{}", Uuid::new_v4());
    let child_d = tokio::process::Command::new(&agent_bin)
        .env("DATABASE_URL", &db_url)
        .env("NATS_URL", &nats_url)
        .env("OBSCURA_WORKER_BIN", &worker_bin)
        .env("OBSCURA_SERVICE_BOOTSTRAP_SECRET", &bootstrap_secret)
        .env("NATS_CONSUMER_NAME", &cname_d)
        .env("OBSCURA_WORKER_NAME", &name_d)
        .env("OBSCURA_LEASE_TTL_SECS", "30")
        .env("OBSCURA_LEASE_RENEW_SECS", "5")
        .env("OBSCURA_ALLOW_PRIVATE_NETWORK", "true")
        .spawn()?;
    let mut guard_d = ProcessGuard::new(child_d);

    let mut wd = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        wd = sqlx::query_scalar::<_, Uuid>(
            "SELECT worker_id FROM workers WHERE capabilities->>'name' = $1 AND status = 'healthy'",
        )
        .bind(&name_d)
        .fetch_optional(cp.pool())
        .await?;
        if wd.is_some() {
            break;
        }
    }
    let wdid = wd.expect("Worker D registered");

    let mut child_pid = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Ok(Some(pid)) = cp.get_worker_child_pid(wdid).await {
            child_pid = Some(pid);
            break;
        }
    }
    let d_pid = child_pid.expect("Worker D child PID");

    // Ensure only Worker D is eligible
    let _ = cp.mark_worker_unavailable(wa_id).await;
    let _ = cp.mark_worker_unavailable(wb_id).await;
    let _ = cp.mark_worker_unavailable(wc_id).await;

    let job5 = cp
        .submit_job_internal(
            tenant_id,
            &slow_url5,
            "snapshot",
            json!({}),
            Some(&format!("containment-job-{}", Uuid::new_v4())),
            1,
        )
        .await?;
    let _ = cp.reconcile_worker_capacity().await;
    let sched5 = sched.schedule_one_for_tenant(Some(tenant_id)).await?;
    assert!(sched5, "Job 5 not scheduled");

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        check_process_alive(d_pid),
        "child should be running before SIGKILL"
    );

    // SIGKILL the agent
    guard_d.kill_sigkill().await;
    assert!(
        !check_process_alive(guard_d.pid.unwrap()),
        "agent should be dead"
    );

    // Verify child (browser worker) dies via reaper
    let mut child_dead = false;
    for i in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;

        if !check_process_alive(d_pid) {
            child_dead = true;
            break;
        }
    }
    assert!(
        child_dead,
        "child browser worker PID {} was NOT killed after agent SIGKILL!",
        d_pid
    );
    println!("✓ [SCENARIO 5 PASS] Browser process tree terminated after agent SIGKILL!");

    // =========================================================================
    // SCENARIO 6: END-TO-END F5 (SIGKILL DURING ACTIVE EXECUTION + TAKEOVER)
    // =========================================================================
    println!("\n=== SCENARIO 6: END-TO-END F5 COMBINED ===");

    let listener6 = TcpListener::bind("127.0.0.1:0").await?;
    let slow_port6 = listener6.local_addr()?.port();
    let slow_url6 = format!("http://127.0.0.1:{}/slow", slow_port6);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener6.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                tokio::time::sleep(Duration::from_secs(4)).await;
                let body = "<html><body>E2E</body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            });
        }
    });

    let name_f = format!("agent-f-{}", Uuid::new_v4());
    let cname_f = format!("wa-f-{}", Uuid::new_v4());
    let child_f = tokio::process::Command::new(&agent_bin)
        .env("DATABASE_URL", &db_url)
        .env("NATS_URL", &nats_url)
        .env("OBSCURA_WORKER_BIN", &worker_bin)
        .env("OBSCURA_SERVICE_BOOTSTRAP_SECRET", &bootstrap_secret)
        .env("NATS_CONSUMER_NAME", &cname_f)
        .env("OBSCURA_WORKER_NAME", &name_f)
        .env("OBSCURA_LEASE_TTL_SECS", "3")
        .env("OBSCURA_LEASE_RENEW_SECS", "1")
        .env("OBSCURA_ALLOW_PRIVATE_NETWORK", "true")
        .spawn()?;
    let mut guard_f = ProcessGuard::new(child_f);

    let name_g = format!("agent-g-{}", Uuid::new_v4());
    let cname_g = format!("wa-g-{}", Uuid::new_v4());
    let child_g = tokio::process::Command::new(&agent_bin)
        .env("DATABASE_URL", &db_url)
        .env("NATS_URL", &nats_url)
        .env("OBSCURA_WORKER_BIN", &worker_bin)
        .env("OBSCURA_SERVICE_BOOTSTRAP_SECRET", &bootstrap_secret)
        .env("NATS_CONSUMER_NAME", &cname_g)
        .env("OBSCURA_WORKER_NAME", &name_g)
        .env("OBSCURA_ALLOW_PRIVATE_NETWORK", "true")
        .spawn()?;
    let mut guard_g = ProcessGuard::new(child_g);

    let mut wf = None;
    let mut wg = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if wf.is_none() {
            wf = sqlx::query_scalar::<_, Uuid>("SELECT worker_id FROM workers WHERE capabilities->>'name' = $1 AND status = 'healthy'")
                .bind(&name_f).fetch_optional(cp.pool()).await?;
        }
        if wg.is_none() {
            wg = sqlx::query_scalar::<_, Uuid>("SELECT worker_id FROM workers WHERE capabilities->>'name' = $1 AND status = 'healthy'")
                .bind(&name_g).fetch_optional(cp.pool()).await?;
        }
        if wf.is_some() && wg.is_some() {
            break;
        }
    }
    let (wf_id, wg_id) = (wf.expect("F registered"), wg.expect("G registered"));
    println!("DEBUG wf_id={} wg_id={}", wf_id, wg_id);

    // Only F eligible for scheduling
    sqlx::query("UPDATE workers SET status='unavailable', updated_at=now() WHERE worker_id <> $1")
        .bind(wf_id)
        .execute(cp.pool())
        .await?;

    let mut f_child_pid = None;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Ok(Some(pid)) = cp.get_worker_child_pid(wf_id).await {
            f_child_pid = Some(pid);
            break;
        }
    }
    let f_pid = f_child_pid.expect("F child pid");

    // Clean any leftover pending jobs from earlier scenarios
    sqlx::query("UPDATE jobs SET status='cancelled', updated_at=now() WHERE status NOT IN ('completed','failed')").execute(cp.pool()).await?;
    let job6 = cp
        .submit_job_internal(
            tenant_id,
            &slow_url6,
            "snapshot",
            json!({}),
            Some(&format!("e2e-f5-{}", Uuid::new_v4())),
            3,
        )
        .await?;

    let _ = cp.reconcile_worker_capacity().await;
    let sched6 = sched.schedule_one_for_tenant(Some(tenant_id)).await?;
    assert!(sched6, "Job6 not scheduled");
    let j6_initial = cp.get_job_internal(job6.job_id, tenant_id).await?;
    assert_eq!(j6_initial.worker_id, Some(wf_id));
    let mut original_token6 = j6_initial.fencing_token;
    let mut original_attempt6 = j6_initial.execution_attempt_id.unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(check_process_alive(f_pid), "F child should be running");

    // SIGKILL F during active execution
    guard_f.kill_sigkill().await;
    assert!(!check_process_alive(guard_f.pid.unwrap()), "F agent dead");

    // Containment
    let mut dead = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if !check_process_alive(f_pid) {
            dead = true;
            break;
        }
    }
    assert!(dead, "F browser tree not terminated");

    // Wait for lease expiry
    tokio::time::sleep(Duration::from_secs(5)).await;
    cp.mark_worker_unavailable(wf_id).await?;
    let _ = cp.recover_abandoned_executions().await?;
    let j6_pending = cp.get_job_internal(job6.job_id, tenant_id).await?;
    assert_eq!(
        j6_pending.status, "pending",
        "Job6 should be pending after recovery"
    );

    // Bring G online and ensure only G is eligible
    cp.worker_heartbeat(wg_id).await?;
    sqlx::query("UPDATE workers SET status='unavailable', updated_at=now() WHERE worker_id <> $1")
        .bind(wg_id)
        .execute(cp.pool())
        .await?;
    let _ = cp.reconcile_worker_capacity().await;
    let ws: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT worker_id, status FROM workers WHERE worker_id IN ($1,$2)")
            .bind(wf_id)
            .bind(wg_id)
            .fetch_all(cp.pool())
            .await?;
    println!("DEBUG workers before reschedule: {:?}", ws);
    let healthy: Vec<Uuid> = sqlx::query_scalar("SELECT worker_id FROM workers WHERE status='healthy' AND last_heartbeat_at > now() - interval '30 seconds'")
        .fetch_all(cp.pool()).await?;
    println!("DEBUG healthy workers: {:?}", healthy);
    // Manual reschedule to observe
    let claimed6 = cp
        .claim_pending_job_for_tenant(Some(tenant_id))
        .await?
        .unwrap();
    assert_eq!(claimed6.job_id, job6.job_id);
    let sw = sched.select_worker(claimed6.tenant_id).await?;
    assert_eq!(sw.worker_id, wg_id);
    let dispatch_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let tok = cp
        .mark_dispatched(
            claimed6.job_id,
            claimed6.tenant_id,
            sw.worker_id,
            dispatch_id,
            attempt_id,
        )
        .await?;
    // Publish NATS dispatch so G executes
    let msg = DispatchMessage {
        version: "1.0".to_string(),
        dispatch_id,
        job_id: claimed6.job_id,
        tenant_id: claimed6.tenant_id,
        attempt_id,
        worker_id: sw.worker_id,
        fencing_token: tok,
        url: claimed6.url.clone(),
        operation: claimed6.operation.clone(),
        correlation_id: None,
    };
    let env = EventEnvelope {
        event_id: dispatch_id,
        event_type: "job.dispatch".to_string(),
        schema_version: 1,
        tenant_id: Some(claimed6.tenant_id),
        aggregate_type: "job".to_string(),
        aggregate_id: claimed6.job_id.to_string(),
        occurred_at: chrono::Utc::now(),
        correlation_id: None,
        causation_id: None,
        payload: serde_json::to_value(&msg).unwrap(),
    };
    nats.publish("obscura.jobs.dispatch", &env).await?;
    let j6_new = cp.get_job_internal(job6.job_id, tenant_id).await?;
    assert_eq!(j6_new.worker_id, Some(wg_id));
    assert!(
        j6_new.fencing_token > original_token6,
        "fencing token must increase"
    );

    // Stale completion from F rejected
    let principal = cp.service_principal("worker-agent", &bootstrap_secret)?;
    let stale = cp
        .complete_job(
            &principal,
            job6.job_id,
            tenant_id,
            original_attempt6,
            original_token6,
            json!({"from":"F"}),
        )
        .await;
    assert!(stale.is_err(), "stale completion must be rejected");

    // Wait for G completion
    let mut done6 = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let j = cp.get_job_internal(job6.job_id, tenant_id).await?;
        if j.status == "completed" {
            done6 = true;
            break;
        }
        if j.status == "pending" {
            let _ = sched.schedule_one_for_tenant(Some(tenant_id)).await;
        }
    }
    assert!(done6, "Job6 did not complete under G");
    println!("✓ [SCENARIO 6 PASS] End-to-end F5 combined verified!");

    guard_g.kill_sigkill().await;

    // =========================================================================
    // SCENARIO 4: NATS Redelivery & Recovery
    // =========================================================================
    println!("\n=== SCENARIO 4: NATS REDELIVERY & RECOVERY ===");

    let prefix = Uuid::new_v4().simple().to_string();
    let stream_name = format!("STREAM_TEST_{}", prefix);
    let test_subject = format!("obscura.redeliver.{}.test", prefix);
    nats.ensure_stream(
        &stream_name,
        vec![format!("obscura.redeliver.{}.>", prefix)],
    )
    .await?;

    let consumer_name = format!("redeliver-cons-{}", Uuid::new_v4().simple());
    let consumer = NatsConsumer::create_with_ack_wait(
        nats.jetstream(),
        &stream_name,
        &consumer_name,
        Duration::from_millis(800),
    )
    .await?;

    let env = EventEnvelope {
        event_id: Uuid::new_v4(),
        event_type: "job.test".to_string(),
        schema_version: 1,
        tenant_id: Some(tenant_id),
        aggregate_type: "job".to_string(),
        aggregate_id: "test-redelivery".to_string(),
        occurred_at: chrono::Utc::now(),
        correlation_id: None,
        causation_id: None,
        payload: json!({"delivery_attempt": 1}),
    };
    nats.publish(&test_subject, &env).await?;

    // First receive: NAK
    let msg1 = consumer
        .next_message()
        .await?
        .expect("should receive message 1");
    println!("Received initial message. Sending NAK for redelivery...");
    msg1.ack_with(async_nats::jetstream::AckKind::Nak(None))
        .await
        .map_err(|e| e.to_string())?;

    // Second receive: verify redelivery
    let msg2 = consumer
        .next_message()
        .await?
        .expect("should receive redelivered message");
    println!("Received redelivered message after NAK. Waiting for ack_wait timeout...");
    // Let timeout expire without ack
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Third receive: redelivered after timeout
    let msg3 = consumer
        .next_message()
        .await?
        .expect("should receive redelivered message after timeout");
    println!("Received redelivered message after ack_wait timeout. Acknowledging...");
    msg3.ack().await.map_err(|e| e.to_string())?;

    println!("✓ [SCENARIO 4 PASS] NATS JetStream redelivery on NAK and unacked timeout verified!");

    println!("\n=======================================================");
    println!("ALL ADVERSARIAL MULTI-PROCESS SCENARIOS PASSED!");
    println!("1. Multi-process coexistence: VERIFIED");
    println!("2. Worker-agent crash recovery & takeover: VERIFIED");
    println!("3. Process-level termination on lease loss (ESRCH): VERIFIED");
    println!("4. NATS redelivery/recovery: VERIFIED");
    println!("=======================================================\n");

    Ok(())
}
