//! Distributed job scheduler for Obscura Next.

use obscura_control_plane::{ControlPlane, NatsClient, NatsConsumer, Principal, WorkerRecord};
use obscura_telemetry::global_metrics;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("control plane error: {0}")]
    Control(#[from] obscura_control_plane::ControlPlaneError),
    #[error("nats error: {0}")]
    Nats(String),
    #[error("no eligible worker")]
    NoWorker,
}

pub type Result<T> = std::result::Result<T, SchedulerError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchMessage {
    pub version: String,
    pub dispatch_id: Uuid,
    pub job_id: Uuid,
    pub tenant_id: Uuid,
    pub attempt_id: Uuid,
    pub worker_id: Uuid,
    pub fencing_token: i64,
    pub url: String,
    pub operation: String,
    pub correlation_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct Scheduler {
    control: ControlPlane,
    nats: NatsClient,
    scheduler_id: Uuid,
}

impl Scheduler {
    pub fn new(control: ControlPlane, nats: NatsClient) -> Self {
        Self {
            control,
            nats,
            scheduler_id: Uuid::new_v4(),
        }
    }

    /// Select an eligible worker for a job.
    pub async fn select_worker(&self, tenant_id: Uuid) -> Result<WorkerRecord> {
        let workers = self.control.list_healthy_workers(Some(tenant_id)).await?;
        // filter capacity
        let mut eligible: Vec<WorkerRecord> = workers
            .into_iter()
            .filter(|w| w.active_jobs < w.capacity)
            .collect();
        if eligible.is_empty() {
            // try global workers
            let global = self.control.list_healthy_workers(None).await?;
            eligible = global
                .into_iter()
                .filter(|w| w.active_jobs < w.capacity)
                .collect();
        }
        if eligible.is_empty() {
            return Err(SchedulerError::NoWorker);
        }
        // least-loaded, stable tie-break by worker_id
        eligible.sort_by(|a, b| {
            a.active_jobs
                .cmp(&b.active_jobs)
                .then(a.worker_id.cmp(&b.worker_id))
        });
        Ok(eligible[0].clone())
    }

    /// Schedule one pending job: claim, select worker, dispatch.
    pub async fn schedule_one(&self) -> Result<bool> {
        self.schedule_one_for_tenant(None).await
    }

    /// Schedule one pending job optionally filtered by tenant.
    pub async fn schedule_one_for_tenant(&self, tenant_id: Option<Uuid>) -> Result<bool> {
        let job = match self.control.claim_pending_job_for_tenant(tenant_id).await? {
            Some(j) => j,
            None => {
                tracing::warn!(
                    "schedule_one_for_tenant: no job claimed for tenant {:?}",
                    tenant_id
                );
                return Ok(false);
            }
        };

        let metrics = global_metrics();
        if let Some(c) = metrics.counter("obscura_scheduler_jobs_scheduled_total") {
            c.inc();
        }

        let worker = match self.select_worker(job.tenant_id).await {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(
                    "schedule_one_for_tenant: select_worker failed for job {}: {:?}",
                    job.job_id,
                    e
                );
                let _ = self.control.release_job_to_pending(job.job_id).await;
                return Ok(false);
            }
        };

        let dispatch_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();

        let fencing_token = self
            .control
            .mark_dispatched(
                job.job_id,
                job.tenant_id,
                worker.worker_id,
                dispatch_id,
                attempt_id,
            )
            .await?;

        let dispatch_msg = DispatchMessage {
            version: "1.0".to_string(),
            dispatch_id,
            job_id: job.job_id,
            tenant_id: job.tenant_id,
            attempt_id,
            worker_id: worker.worker_id,
            fencing_token,
            url: job.url.clone(),
            operation: job.operation.clone(),
            correlation_id: None,
        };

        self.nats
            .publish(
                "obscura.jobs.dispatch",
                &obscura_control_plane::EventEnvelope {
                    event_id: dispatch_id,
                    event_type: "job.dispatch".to_string(),
                    schema_version: 1,
                    tenant_id: Some(job.tenant_id),
                    aggregate_type: "job".to_string(),
                    aggregate_id: job.job_id.to_string(),
                    occurred_at: chrono::Utc::now(),
                    correlation_id: None,
                    causation_id: None,
                    payload: serde_json::to_value(&dispatch_msg).unwrap(),
                },
            )
            .await
            .map_err(|e| SchedulerError::Nats(e.to_string()))?;

        if let Some(c) = metrics.counter("obscura_scheduler_jobs_dispatched_total") {
            c.inc();
        }

        Ok(true)
    }

    /// Handle job completion reported by worker.
    pub async fn handle_completion(
        &self,
        job_id: Uuid,
        tenant_id: Uuid,
        attempt_id: Uuid,
        fencing_token: i64,
        result: serde_json::Value,
    ) -> Result<()> {
        let principal = self.control.service_principal(
            "scheduler",
            &std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET").unwrap_or_default(),
        )?;
        self.control
            .complete_job(
                &principal,
                job_id,
                tenant_id,
                attempt_id,
                fencing_token,
                result,
            )
            .await?;
        Ok(())
    }

    /// Handle job failure.
    pub async fn handle_failure(
        &self,
        job_id: Uuid,
        tenant_id: Uuid,
        attempt_id: Uuid,
        fencing_token: i64,
        code: &str,
        msg: &str,
        retryable: bool,
    ) -> Result<()> {
        let principal = self.control.service_principal(
            "scheduler",
            &std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET").unwrap_or_default(),
        )?;
        self.control
            .fail_job(
                &principal,
                job_id,
                tenant_id,
                attempt_id,
                fencing_token,
                code,
                msg,
                retryable,
            )
            .await?;
        Ok(())
    }

    /// Cancel a job.
    pub async fn cancel(&self, job_id: Uuid, tenant_id: Uuid) -> Result<()> {
        let principal = self.control.service_principal(
            "scheduler",
            &std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET").unwrap_or_default(),
        )?;
        self.control
            .cancel_job_phase4(&principal, job_id, tenant_id)
            .await?;
        Ok(())
    }

    /// Startup reconciliation: reclaim scheduled/dispatched jobs and schedule pending.
    pub async fn reconcile(&self) -> Result<()> {
        // Reclaim stuck jobs via control plane
        let _ = self.control.reconcile_stuck_jobs().await;
        // schedule all pending
        while self.schedule_one().await.unwrap_or(false) {}
        Ok(())
    }
    /// Reconcile worker health, recover abandoned executions, and reconcile capacity with custom threshold.
    pub async fn reconcile_workers_with_threshold(&self, threshold_secs: i64) -> Result<u64> {
        // Expire stale workers
        let _ = self.control.expire_stale_workers(threshold_secs).await;
        // Recover abandoned executions
        let recovered = self
            .control
            .recover_abandoned_executions()
            .await
            .unwrap_or(0);
        // Reconcile capacity
        let _ = self.control.reconcile_worker_capacity().await;
        // Schedule pending jobs
        while self.schedule_one().await.unwrap_or(false) {}
        Ok(recovered)
    }

    /// Reconcile worker health and recover abandoned executions.
    pub async fn reconcile_workers(&self) -> Result<u64> {
        self.reconcile_workers_with_threshold(10).await
    }
    /// Run periodic reconciliation in a background task.
    pub async fn run_reconciliation_loop(&self, interval_secs: u64) {
        loop {
            let _ = self.reconcile_workers().await;
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    }

    /// Run scheduling loop consuming job.submitted events.
    pub async fn run(&self) -> Result<()> {
        // ensure stream
        self.nats
            .ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .map_err(|e| SchedulerError::Nats(e.to_string()))?;

        let consumer = NatsConsumer::create(self.nats.jetstream(), "OBScura_JOBS", "scheduler")
            .await
            .map_err(|e| SchedulerError::Nats(e.to_string()))?;

        // initial reconciliation of stuck jobs
        let _ = self.control.reconcile_stuck_jobs().await;

        // initial drain of pending
        while self.schedule_one().await? {}

        // Start reconciliation loop in background
        let sched = self.clone();
        tokio::spawn(async move {
            loop {
                let _ = sched.control.reconcile_stuck_jobs().await;
                let _ = sched.reconcile_workers().await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });

        loop {
            match consumer.next_message().await {
                Ok(Some(msg)) => {
                    // parse event envelope
                    if let Ok(env) =
                        serde_json::from_slice::<obscura_control_plane::EventEnvelope>(&msg.payload)
                    {
                        if env.event_type == "job.submitted" {
                            // schedule all pending
                            while self.schedule_one().await.unwrap_or(false) {}
                        }
                    }
                    let _ = msg.ack().await;
                }
                Ok(None) => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
            // periodic scheduling
            while self.schedule_one().await.unwrap_or(false) {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obscura_control_plane::ControlPlane;

    #[tokio::test]
    async fn test_worker_selection_and_dispatch() {
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let cp = ControlPlane::connect(&db_url, 5).await.unwrap();
        cp.migrate().await.unwrap();
        let nats = NatsClient::connect(&nats_url).await.unwrap();
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .unwrap();

        let tenant = cp
            .create_tenant(&format!("sched-t-{}", Uuid::new_v4()))
            .await
            .unwrap();
        let worker_id = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        let sched = Scheduler::new(cp.clone(), nats.clone());
        // submit job
        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("sk1-{}", Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();

        // schedule one
        let scheduled = sched.schedule_one().await.unwrap();
        assert!(scheduled);

        // job should be dispatched
        let j = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(j.status, "dispatched");
        assert_eq!(j.worker_id, Some(worker_id));
    }

    #[tokio::test]
    async fn test_multi_scheduler_no_duplicate_claim() {
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let cp = ControlPlane::connect(&db_url, 5).await.unwrap();
        cp.migrate().await.unwrap();
        let nats = NatsClient::connect(&nats_url).await.unwrap();
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .unwrap();

        let tenant = cp
            .create_tenant(&format!("sched-t2-{}", Uuid::new_v4()))
            .await
            .unwrap();
        cp.register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        // submit 5 jobs
        for i in 0..5 {
            cp.submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("mk{}-{}", i, Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();
        }

        let sched1 = Scheduler::new(cp.clone(), nats.clone());
        let sched2 = Scheduler::new(cp.clone(), nats.clone());

        // concurrent scheduling
        let tid = tenant.tenant_id;
        let (a, b) = tokio::join!(
            async {
                let mut c = 0;
                while sched1
                    .schedule_one_for_tenant(Some(tid))
                    .await
                    .unwrap_or(false)
                {
                    c += 1;
                }
                c
            },
            async {
                let mut c = 0;
                while sched2
                    .schedule_one_for_tenant(Some(tid))
                    .await
                    .unwrap_or(false)
                {
                    c += 1;
                }
                c
            }
        );
        // total dispatched should be 5
        assert_eq!(a + b, 5);
    }

    #[tokio::test]
    async fn test_cancel_and_failure() {
        std::env::set_var("OBSCURA_SERVICE_BOOTSTRAP_SECRET", "test-secret");
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let cp = ControlPlane::connect(&db_url, 5).await.unwrap();
        cp.migrate().await.unwrap();
        let nats = NatsClient::connect(&nats_url).await.unwrap();
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .unwrap();

        let tenant = cp
            .create_tenant(&format!("sched-t3-{}", Uuid::new_v4()))
            .await
            .unwrap();
        cp.register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();
        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("cancel1-{}", Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();
        let sched = Scheduler::new(cp.clone(), nats.clone());
        sched.schedule_one().await.unwrap();
        let _j = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .unwrap();
        // cancel
        sched.cancel(job.job_id, tenant.tenant_id).await.unwrap();
        // fail path
        let job2 = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("fail1-{}", Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();
        sched.schedule_one().await.unwrap();
        let j2 = cp
            .get_job_internal(job2.job_id, tenant.tenant_id)
            .await
            .unwrap();
        sched
            .handle_failure(
                j2.job_id,
                tenant.tenant_id,
                j2.execution_attempt_id.unwrap(),
                j2.fencing_token,
                "ERR",
                "boom",
                false,
            )
            .await
            .unwrap();
        let j2f = cp
            .get_job_internal(job2.job_id, tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(j2f.status, "failed");
    }

    #[tokio::test]
    async fn test_scheduler_worker_crash_takeover() {
        std::env::set_var("OBSCURA_SERVICE_BOOTSTRAP_SECRET", "test-secret");
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let cp = ControlPlane::connect(&db_url, 5).await.unwrap();
        cp.migrate().await.unwrap();
        let nats = NatsClient::connect(&nats_url).await.unwrap();
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .unwrap();

        let tenant = cp
            .create_tenant(&format!("crash-takeover-{}", Uuid::new_v4()))
            .await
            .unwrap();
        let w_a = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();
        let w_b = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        let sched = Scheduler::new(cp.clone(), nats.clone());

        // Submit job
        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("crash-key-{}", Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();

        // Initial schedule: dispatches to one of the workers (e.g. w_a)
        let scheduled = sched.schedule_one().await.unwrap();
        assert!(scheduled);

        let initial_job = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(initial_job.status, "dispatched");
        let initial_worker = initial_job.worker_id.unwrap();
        let initial_token = initial_job.fencing_token;
        let other_worker = if initial_worker == w_a { w_b } else { w_a };

        // Simulate Worker A crash: mark worker unavailable
        cp.mark_worker_unavailable(initial_worker).await.unwrap();

        // Reconcile workers: detects unavailable worker, reclaims job to pending, and reschedules to Worker B
        let recovered = sched.reconcile_workers().await.unwrap();
        assert!(recovered >= 1, "abandoned execution should be recovered");

        // Job should now be dispatched to Worker B with a strictly higher fencing token
        let reassigned_job = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(reassigned_job.status, "dispatched");
        assert_eq!(reassigned_job.worker_id, Some(other_worker));
        assert!(
            reassigned_job.fencing_token > initial_token,
            "fencing token must be monotonically increasing"
        );

        // Stale completion attempt from crashed Worker A with initial_token must fail
        let principal = cp
            .service_principal(
                "worker",
                &std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET").unwrap_or_default(),
            )
            .unwrap();
        let stale_res = cp
            .complete_job(
                &principal,
                job.job_id,
                tenant.tenant_id,
                initial_job.execution_attempt_id.unwrap(),
                initial_token,
                serde_json::json!({"from": "crashed_worker"}),
            )
            .await;
        assert!(
            stale_res.is_err(),
            "stale completion from crashed worker must be rejected"
        );

        // Successful completion from takeover Worker B
        let takeover_res = cp
            .complete_job(
                &principal,
                job.job_id,
                tenant.tenant_id,
                reassigned_job.execution_attempt_id.unwrap(),
                reassigned_job.fencing_token,
                serde_json::json!({"from": "takeover_worker"}),
            )
            .await;
        assert!(takeover_res.is_ok(), "takeover completion must succeed");

        let final_job = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(final_job.status, "completed");
    }
}
