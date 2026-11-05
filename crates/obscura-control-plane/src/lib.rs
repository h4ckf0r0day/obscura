//! Distributed control plane persistence for Obscura Next (PostgreSQL).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use thiserror::Error;
use std::time::Instant;
use futures::StreamExt;
use obscura_telemetry::global_metrics;
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum ControlPlaneError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

pub type Result<T> = std::result::Result<T, ControlPlaneError>;

#[derive(Clone)]
pub struct ControlPlane {
    pool: PgPool,
}

impl ControlPlane {
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations").run(&self.pool).await?;
        Ok(())
    }
}

// --- Domain models ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub tenant_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub job_id: Uuid,
    pub tenant_id: Uuid,
    pub status: String,
    pub url: String,
    pub operation: String,
    pub payload: serde_json::Value,
    pub idempotency_key: Option<String>,
    pub attempt_count: i32,
    pub max_attempts: i32,
    pub fencing_token: i64,
    pub worker_id: Option<Uuid>,
    pub dispatch_id: Option<Uuid>,
    pub execution_attempt_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// --- Repository operations ---

impl ControlPlane {
    pub async fn create_tenant(&self, name: &str) -> Result<Tenant> {
        let tenant_id = Uuid::new_v4();
        let row = sqlx::query(
            "INSERT INTO tenants (tenant_id, name) VALUES ($1, $2) RETURNING tenant_id, name, created_at",
        )
        .bind(tenant_id)
        .bind(name)
        .fetch_one(&self.pool)
        .await?;
        Ok(Tenant {
            tenant_id: row.get("tenant_id"),
            name: row.get("name"),
            created_at: row.get("created_at"),
        })
    }

    pub async fn submit_job(
        &self,
        principal: &Principal,
        tenant_id: Uuid,
        url: &str,
        operation: &str,
        payload: serde_json::Value,
        idempotency_key: Option<&str>,
        max_attempts: i32,
    ) -> Result<Job> {
        self.authorize_tenant(principal, tenant_id)?;
        // Basic SSRF literal-IP guard at control plane boundary
        if let Ok(parsed) = url::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                    if obscura_ssrf::is_forbidden_ip(ip) {
                        return Err(ControlPlaneError::Conflict("SSRF blocked".into()));
                    }
                }
            }
        }

        // Idempotency: return existing if key present
        if let Some(key) = idempotency_key {
            if let Some(existing) = sqlx::query_as::<_, (Uuid,)>(
                "SELECT job_id FROM jobs WHERE tenant_id = $1 AND idempotency_key = $2",
            )
            .bind(tenant_id)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?
            {
                return self.get_job_internal(existing.0, tenant_id).await;
            }
        }

        let job_id = Uuid::new_v4();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO jobs (job_id, tenant_id, url, operation, payload, idempotency_key, max_attempts) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(job_id)
        .bind(tenant_id)
        .bind(url)
        .bind(operation)
        .bind(&payload)
        .bind(idempotency_key)
        .bind(max_attempts)
        .execute(&mut *tx)
        .await?;

        // outbox
        sqlx::query(
            "INSERT INTO outbox_events (aggregate_type, aggregate_id, event_type, payload) VALUES ($1,$2,$3,$4)",
        )
        .bind("job")
        .bind(job_id.to_string())
        .bind("job.submitted")
        .bind(serde_json::json!({"job_id": job_id, "tenant_id": tenant_id}))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.get_job_internal(job_id, tenant_id).await
    }

    pub async fn get_job(&self, principal: &Principal, job_id: Uuid, tenant_id: Uuid) -> Result<Job> {
        self.authorize_tenant(principal, tenant_id)?;
        let row = sqlx::query(
            "SELECT job_id, tenant_id, status, url, operation, payload, idempotency_key, attempt_count, max_attempts, fencing_token, worker_id, dispatch_id, execution_attempt_id, created_at, updated_at FROM jobs WHERE job_id = $1 AND tenant_id = $2",
        )
        .bind(job_id)
        .bind(tenant_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| ControlPlaneError::NotFound(job_id.to_string()))?;

        Ok(Job {
            job_id: row.get("job_id"),
            tenant_id: row.get("tenant_id"),
            status: row.get("status"),
            url: row.get("url"),
            operation: row.get("operation"),
            payload: row.get("payload"),
            idempotency_key: row.get("idempotency_key"),
            attempt_count: row.get("attempt_count"),
            max_attempts: row.get("max_attempts"),
            fencing_token: row.get("fencing_token"),
            worker_id: row.get("worker_id"),
            dispatch_id: row.get("dispatch_id"),
            execution_attempt_id: row.get("execution_attempt_id"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    pub async fn list_jobs(&self, principal: &Principal, tenant_id: Uuid, limit: i64) -> Result<Vec<Job>> {
        self.authorize_tenant(principal, tenant_id)?;
        let rows = sqlx::query(
            "SELECT job_id, tenant_id, status, url, operation, payload, idempotency_key, attempt_count, max_attempts, fencing_token, worker_id, dispatch_id, execution_attempt_id, created_at, updated_at FROM jobs WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| Job {
                job_id: row.get("job_id"),
                tenant_id: row.get("tenant_id"),
                status: row.get("status"),
                url: row.get("url"),
                operation: row.get("operation"),
                payload: row.get("payload"),
                idempotency_key: row.get("idempotency_key"),
                attempt_count: row.get("attempt_count"),
                max_attempts: row.get("max_attempts"),
                fencing_token: row.get("fencing_token"),
                worker_id: row.get("worker_id"),
                dispatch_id: row.get("dispatch_id"),
                execution_attempt_id: row.get("execution_attempt_id"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            })
            .collect())
    }

    pub async fn cancel_job(&self, principal: &Principal, job_id: Uuid, tenant_id: Uuid) -> Result<()> {
        self.authorize_tenant(principal, tenant_id)?;
        let mut tx = self.pool().begin().await?;
        let res = sqlx::query(
            "UPDATE jobs SET status = 'cancelled', updated_at = now() WHERE job_id = $1 AND tenant_id = $2 AND status IN ('pending','queued','assigned','dispatched','running','scheduled')",
        )
        .bind(job_id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            tx.rollback().await.ok();
            return Err(ControlPlaneError::Conflict("job cannot be cancelled".into()));
        }
        sqlx::query("UPDATE workers SET active_jobs = GREATEST(active_jobs-1,0) WHERE worker_id = (SELECT worker_id FROM jobs WHERE job_id=$1)")
            .bind(job_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_migrate_and_submit_job() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp.create_tenant("test-tenant").await.expect("tenant");
        let job = cp
            .submit_job(None, tenant.tenant_id, "https://example.com", "snapshot", serde_json::json!({}), Some("key-1"), 3)
            .await
            .expect("submit");
        assert_eq!(job.status, "pending");

        // idempotency
        let job2 = cp
            .submit_job(None, tenant.tenant_id, "https://example.com", "snapshot", serde_json::json!({}), Some("key-1"), 3)
            .await
            .expect("submit2");
        assert_eq!(job.job_id, job2.job_id);

        let fetched = cp.get_job_internal(job.job_id, tenant.tenant_id).await.expect("get");
        assert_eq!(fetched.job_id, job.job_id);

        let list = cp.list_jobs_internal(tenant.tenant_id, 10).await.expect("list");
        assert!(!list.is_empty());

        cp.cancel_job_internal(job.job_id, tenant.tenant_id).await.expect("cancel");
    }
}

// ---------------------------------------------------------------------------
// Event envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: Uuid,
    pub event_type: String,
    pub schema_version: u32,
    pub tenant_id: Option<Uuid>,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub occurred_at: DateTime<Utc>,
    pub correlation_id: Option<Uuid>,
    pub causation_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

impl EventEnvelope {
    pub fn new(event_type: &str, aggregate_type: &str, aggregate_id: &str, payload: serde_json::Value) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            event_type: event_type.to_string(),
            schema_version: 1,
            tenant_id: None,
            aggregate_type: aggregate_type.to_string(),
            aggregate_id: aggregate_id.to_string(),
            occurred_at: Utc::now(),
            correlation_id: None,
            causation_id: None,
            payload,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(ControlPlaneError::Conflict(format!(
                "unsupported schema version: {}",
                self.schema_version
            )));
        }
        if self.event_type.is_empty() {
            return Err(ControlPlaneError::Conflict("event_type required".into()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NATS client wrapper
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct NatsClient {
    client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
}

impl NatsClient {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = async_nats::connect(url).await.map_err(|e| {
            ControlPlaneError::Conflict(format!("nats connect: {e}"))
        })?;
        let jetstream = async_nats::jetstream::new(client.clone());
        Ok(Self { client, jetstream })
    }

    pub fn jetstream(&self) -> &async_nats::jetstream::Context {
        &self.jetstream
    }

    pub async fn ensure_stream(&self, name: &str, subjects: Vec<String>) -> Result<()> {
        let cfg = async_nats::jetstream::stream::Config {
            name: name.to_string(),
            subjects,
            retention: async_nats::jetstream::stream::RetentionPolicy::Limits,
            storage: async_nats::jetstream::stream::StorageType::File,
            ..Default::default()
        };
        self.jetstream
            .get_or_create_stream(cfg)
            .await
            .map_err(|e| ControlPlaneError::Conflict(format!("stream: {e}")))?;
        Ok(())
    }

    pub async fn publish(&self, subject: &str, envelope: &EventEnvelope) -> Result<()> {
        envelope.validate()?;
        let bytes = serde_json::to_vec(envelope).map_err(|e| {
            ControlPlaneError::Conflict(format!("serialize: {e}"))
        })?;
        use async_nats::HeaderMap;
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", envelope.event_id.to_string().parse::<async_nats::HeaderValue>().unwrap());
        let ack = self
            .jetstream
            .publish_with_headers(subject.to_string(), headers, bytes.into())
            .await
            .map_err(|e| ControlPlaneError::Conflict(format!("publish: {e}")))?;
        ack.await
            .map_err(|e| ControlPlaneError::Conflict(format!("ack: {e}")))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Outbox publisher
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OutboxPublisher {
    control: ControlPlane,
    nats: NatsClient,
    claimer: String,
    batch_size: i64,
}

impl OutboxPublisher {
    pub fn new(control: ControlPlane, nats: NatsClient, claimer: &str, batch_size: i64) -> Self {
        Self {
            control,
            nats,
            claimer: claimer.to_string(),
            batch_size,
        }
    }

    pub async fn publish_pending(&self) -> Result<usize> {
        // Claim batch safely
        let claimed = self.claim_batch().await?;
        let mut published = 0;
        for row in claimed {
            let subject = format!("obscura.{}.{}", row.aggregate_type, row.event_type.trim_start_matches("job."));
            // map event_type to subject-ish; fallback
            let subject = if row.event_type == "job.submitted" {
                "obscura.jobs.submitted".to_string()
            } else {
                subject
            };

            let envelope = EventEnvelope {
                event_id: row.outbox_id_uuid(),
                event_type: row.event_type.clone(),
                schema_version: 1,
                tenant_id: None,
                aggregate_type: row.aggregate_type.clone(),
                aggregate_id: row.aggregate_id.clone(),
                occurred_at: row.created_at,
                correlation_id: None,
                causation_id: None,
                payload: row.payload.clone(),
            };

            let pub_start = Instant::now();
            match self.nats.publish(&subject, &envelope).await {
                Ok(()) => {
                    sqlx::query(
                        "UPDATE outbox_events SET published_at = now(), claimed_at = NULL WHERE outbox_id = $1",
                    )
                    .bind(row.outbox_id)
                    .execute(self.control.pool())
                    .await?;
                    published += 1;
                    let metrics = global_metrics();
                    if let Some(c) = metrics.counter("obscura_outbox_published_total") { c.inc(); }
                    if let Some(c) = metrics.counter("obscura_nats_publish_ack_total") { c.inc(); }
                    if let Some(h) = metrics.histogram("obscura_outbox_publish_duration_seconds") { h.observe_duration(pub_start.elapsed()); }
                }
                Err(e) => {
                    sqlx::query(
                        "UPDATE outbox_events SET attempts = attempts + 1, claimed_at = NULL WHERE outbox_id = $1",
                    )
                    .bind(row.outbox_id)
                    .execute(self.control.pool())
                    .await?;
                    let metrics = global_metrics();
                    if let Some(c) = metrics.counter("obscura_outbox_failed_total") { c.inc(); }
                    if let Some(c) = metrics.counter("obscura_nats_publish_errors_total") { c.inc(); }
                    tracing::warn!(outbox_id = row.outbox_id, error = %e, "outbox publish failed");
                }
            }
        }
        Ok(published)
    }

    async fn claim_batch(&self) -> Result<Vec<OutboxRow>> {
        let mut tx = self.control.pool().begin().await?;
        let rows = sqlx::query(
            "SELECT outbox_id, aggregate_type, aggregate_id, event_type, payload, created_at FROM outbox_events WHERE published_at IS NULL AND (claimed_at IS NULL OR claimed_at < now() - interval '1 minute') ORDER BY created_at ASC LIMIT $1 FOR UPDATE SKIP LOCKED",
        )
        .bind(self.batch_size)
        .fetch_all(&mut *tx)
        .await?;

        let mut out = Vec::new();
        for r in rows {
            let outbox_id: i64 = r.get("outbox_id");
            sqlx::query(
                "UPDATE outbox_events SET claimed_at = now(), claimed_by = $1 WHERE outbox_id = $2",
            )
            .bind(&self.claimer)
            .bind(outbox_id)
            .execute(&mut *tx)
            .await?;
            out.push(OutboxRow {
                outbox_id,
                aggregate_type: r.get("aggregate_type"),
                aggregate_id: r.get("aggregate_id"),
                event_type: r.get("event_type"),
                payload: r.get("payload"),
                created_at: r.get("created_at"),
            });
        }
        tx.commit().await?;
        Ok(out)
    }
}

#[derive(Debug, Clone)]
struct OutboxRow {
    outbox_id: i64,
    aggregate_type: String,
    aggregate_id: String,
    event_type: String,
    payload: serde_json::Value,
    created_at: DateTime<Utc>,
}

impl OutboxRow {
    fn outbox_id_uuid(&self) -> Uuid {
        // deterministic-ish: use uuid from outbox_id via nil + bytes not possible; generate stable by hashing? Use Uuid::new_v4 not stable.
        // For idempotency use a deterministic uuid v5 from outbox_id.
        Uuid::new_v5(&Uuid::NAMESPACE_OID, self.outbox_id.to_string().as_bytes())
    }
}


// ---------------------------------------------------------------------------
// Consumer foundation
// ---------------------------------------------------------------------------

pub struct NatsConsumer {
    consumer: async_nats::jetstream::consumer::Consumer<async_nats::jetstream::consumer::pull::Config>,
}

impl NatsConsumer {
    pub async fn create(
        js: &async_nats::jetstream::Context,
        stream: &str,
        durable_name: &str,
    ) -> Result<Self> {
        let cfg = async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(durable_name.to_string()),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            max_deliver: 5,
            ..Default::default()
        };
        let consumer = js
            .create_consumer_on_stream(cfg, stream)
            .await
            .map_err(|e| ControlPlaneError::Conflict(format!("consumer: {e}")))?;
        Ok(Self { consumer })
    }

    pub async fn next_message(&self) -> Result<Option<async_nats::jetstream::Message>> {
        let mut messages = self
            .consumer
            .messages()
            .await
            .map_err(|e| ControlPlaneError::Conflict(format!("messages: {e}")))?;
        match messages.next().await {
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(ControlPlaneError::Conflict(format!("next: {e}"))),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod phase3_tests {
    use super::*;

    #[tokio::test]
    async fn test_nats_stream_and_outbox_publish() {
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5433/obscura".to_string());
        let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

        let cp = ControlPlane::connect(&db_url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let nats = NatsClient::connect(&nats_url).await.expect("nats");
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .expect("stream");

        let tenant = cp.create_tenant("t3").await.expect("tenant");
        let job = cp
            .submit_job(None, tenant.tenant_id, "https://example.com", "snapshot", serde_json::json!({}), Some("k3"), 3)
            .await
            .expect("submit");

        let publisher = OutboxPublisher::new(cp.clone(), nats.clone(), "pub-1", 10);
        let n = publisher.publish_pending().await.expect("publish");
        assert!(n >= 1);

        // verify published_at set
        let row: (Option<chrono::DateTime<Utc>>, ) = sqlx::query_as(
            "SELECT published_at FROM outbox_events WHERE aggregate_id = $1",
        )
        .bind(job.job_id.to_string())
        .fetch_one(cp.pool())
        .await
        .expect("row");
        assert!(row.0.is_some());

        // consumer foundation
        let consumer = NatsConsumer::create(nats.jetstream(), "OBScura_JOBS", "test-consumer")
            .await
            .expect("consumer");
        let msg = consumer.next_message().await.expect("next");
        assert!(msg.is_some());
        if let Some(m) = msg {
            m.ack().await.expect("ack");
        }
    }

    #[tokio::test]
    async fn test_duplicate_publication_safe() {
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5433/obscura".to_string());
        let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

        let cp = ControlPlane::connect(&db_url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");
        let nats = NatsClient::connect(&nats_url).await.expect("nats");
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .expect("stream");

        let tenant = cp.create_tenant("t4").await.expect("tenant");
        cp.submit_job_internal(tenant.tenant_id, "https://example.com", "snapshot", serde_json::json!({}), Some("k4"), 3)
            .await
            .expect("submit");

        let publisher = OutboxPublisher::new(cp.clone(), nats.clone(), "pub-2", 10);
        let n1 = publisher.publish_pending().await.expect("p1");
        let n2 = publisher.publish_pending().await.expect("p2");
        // second run publishes nothing new
        assert_eq!(n2, 0);
        assert!(n1 >= 1);
    }
}

// ---------------------------------------------------------------------------
// Phase 4: scheduler support
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRecord {
    pub worker_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub status: String,
    pub capabilities: serde_json::Value,
    pub capacity: i32,
    pub active_jobs: i32,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

impl ControlPlane {
    pub async fn register_worker(
        &self,
        tenant_id: Option<Uuid>,
        capabilities: serde_json::Value,
        capacity: i32,
    ) -> Result<Uuid> {
        let worker_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO workers (worker_id, tenant_id, status, capabilities, capacity, last_heartbeat_at) VALUES ($1,$2,'healthy',$3,$4,now())",
        )
        .bind(worker_id)
        .bind(tenant_id)
        .bind(&capabilities)
        .bind(capacity)
        .execute(self.pool())
        .await?;
        Ok(worker_id)
    }

    pub async fn worker_heartbeat(&self, worker_id: Uuid) -> Result<()> {
        let res = sqlx::query("UPDATE workers SET last_heartbeat_at = now(), status = 'healthy' WHERE worker_id = $1")
            .bind(worker_id)
            .execute(self.pool())
            .await?;
        if res.rows_affected() == 0 {
            return Err(ControlPlaneError::NotFound(worker_id.to_string()));
        }
        Ok(())
    }

    pub async fn list_healthy_workers(&self, tenant_id: Option<Uuid>) -> Result<Vec<WorkerRecord>> {
        let rows = sqlx::query(
            "SELECT worker_id, tenant_id, status, capabilities, capacity, active_jobs, last_heartbeat_at FROM workers WHERE status = 'healthy' AND last_heartbeat_at > now() - interval '30 seconds' AND ($1::uuid IS NULL OR tenant_id IS NULL OR tenant_id = $1)",
        )
        .bind(tenant_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|r| WorkerRecord {
            worker_id: r.get("worker_id"),
            tenant_id: r.get("tenant_id"),
            status: r.get("status"),
            capabilities: r.get("capabilities"),
            capacity: r.get("capacity"),
            active_jobs: r.get("active_jobs"),
            last_heartbeat_at: r.get("last_heartbeat_at"),
        }).collect())
    }

    /// Atomically claim a pending job for scheduling. Returns job if claimed.
    pub async fn claim_pending_job(&self) -> Result<Option<Job>> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT job_id, tenant_id, status, url, operation, payload, idempotency_key, attempt_count, max_attempts, fencing_token, worker_id, dispatch_id, execution_attempt_id, created_at, updated_at FROM jobs WHERE status = 'pending' ORDER BY created_at ASC LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let job = match row {
            Some(r) => {
                let job_id: Uuid = r.get("job_id");
                let tenant_id: Uuid = r.get("tenant_id");
                sqlx::query("UPDATE jobs SET status = 'scheduled', updated_at = now() WHERE job_id = $1")
                    .bind(job_id)
                    .execute(&mut *tx)
                    .await?;
                Some(Job {
                    job_id,
                    tenant_id,
                    status: "scheduled".to_string(),
                    url: r.get("url"),
                    operation: r.get("operation"),
                    payload: r.get("payload"),
                    idempotency_key: r.get("idempotency_key"),
                    attempt_count: r.get("attempt_count"),
                    max_attempts: r.get("max_attempts"),
                    fencing_token: r.get("fencing_token"),
                    worker_id: r.get("worker_id"),
                    dispatch_id: r.get("dispatch_id"),
                    execution_attempt_id: r.get("execution_attempt_id"),
                    created_at: r.get("created_at"),
                    updated_at: Utc::now(),
                })
            }
            None => None,
        };
        tx.commit().await?;
        Ok(job)
    }

    pub async fn mark_dispatched(&self, job_id: Uuid, tenant_id: Uuid, worker_id: Uuid, dispatch_id: Uuid, attempt_id: Uuid) -> Result<i64> {
        let mut tx = self.pool().begin().await?;
        let token: i64 = sqlx::query_scalar("SELECT nextval('fencing_token_seq')")
            .fetch_one(&mut *tx).await?;
        sqlx::query(
            "UPDATE jobs SET status='dispatched', worker_id=$1, dispatch_id=$2, execution_attempt_id=$3, fencing_token=$4, attempt_count=attempt_count+1, updated_at=now() WHERE job_id=$5 AND tenant_id=$6 AND status='scheduled'",
        )
        .bind(worker_id)
        .bind(dispatch_id)
        .bind(attempt_id)
        .bind(token)
        .bind(job_id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE workers SET active_jobs = active_jobs + 1 WHERE worker_id = $1")
            .bind(worker_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO job_attempts (attempt_id, job_id, worker_id, fencing_token, status) VALUES ($1,$2,$3,$4,'dispatched')",
        )
        .bind(attempt_id)
        .bind(job_id)
        .bind(worker_id)
        .bind(token)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(token)
    }

    pub async fn mark_running(&self, job_id: Uuid, tenant_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE jobs SET status='running', updated_at=now() WHERE job_id=$1 AND tenant_id=$2 AND status='dispatched'")
            .bind(job_id).bind(tenant_id).execute(self.pool()).await?;
        Ok(())
    }

    pub async fn complete_job(&self, principal: &Principal, job_id: Uuid, tenant_id: Uuid, attempt_id: Uuid, fencing_token: i64, result: serde_json::Value) -> Result<()> {
        self.authorize_tenant(principal, tenant_id)?;
        let mut tx = self.pool().begin().await?;
        // validate fencing token
        let row = sqlx::query("SELECT fencing_token FROM jobs WHERE job_id=$1 AND tenant_id=$2 FOR UPDATE")
            .bind(job_id).bind(tenant_id).fetch_optional(&mut *tx).await?
            .ok_or_else(|| ControlPlaneError::NotFound(job_id.to_string()))?;
        let current: i64 = row.get("fencing_token");
        if current != fencing_token {
            return Err(ControlPlaneError::Conflict("stale fencing token".into()));
        }
        sqlx::query("UPDATE jobs SET status='completed', updated_at=now(), completed_at=now() WHERE job_id=$1 AND tenant_id=$2")
            .bind(job_id).bind(tenant_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE job_attempts SET status='completed', finished_at=now() WHERE attempt_id=$1")
            .bind(attempt_id).execute(&mut *tx).await?;
        // decrement worker active
        sqlx::query("UPDATE workers SET active_jobs = GREATEST(active_jobs-1,0) WHERE worker_id = (SELECT worker_id FROM jobs WHERE job_id=$1)")
            .bind(job_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload) VALUES ($1,'completed',$2)")
            .bind(job_id).bind(result).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail_job(&self, principal: &Principal, job_id: Uuid, tenant_id: Uuid, attempt_id: Uuid, fencing_token: i64, error_code: &str, error_message: &str, retryable: bool) -> Result<()> {
        self.authorize_tenant(principal, tenant_id)?;
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query("SELECT fencing_token FROM jobs WHERE job_id=$1 AND tenant_id=$2 FOR UPDATE")
            .bind(job_id).bind(tenant_id).fetch_optional(&mut *tx).await?
            .ok_or_else(|| ControlPlaneError::NotFound(job_id.to_string()))?;
        let current: i64 = row.get("fencing_token");
        if current != fencing_token {
            return Err(ControlPlaneError::Conflict("stale fencing token".into()));
        }
        // get attempt count
        let row = sqlx::query("SELECT attempt_count, max_attempts FROM jobs WHERE job_id=$1 AND tenant_id=$2")
            .bind(job_id).bind(tenant_id).fetch_one(&mut *tx).await?;
        let attempts: i32 = row.get("attempt_count");
        let max: i32 = row.get("max_attempts");
        let new_status = if retryable && attempts < max { "pending" } else { "failed" };
        sqlx::query("UPDATE jobs SET status=$1, updated_at=now() WHERE job_id=$2 AND tenant_id=$3")
            .bind(new_status).bind(job_id).bind(tenant_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE job_attempts SET status='failed', finished_at=now(), error_code=$1, error_message=$2 WHERE attempt_id=$3")
            .bind(error_code).bind(error_message).bind(attempt_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE workers SET active_jobs = GREATEST(active_jobs-1,0) WHERE worker_id = (SELECT worker_id FROM jobs WHERE job_id=$1)")
            .bind(job_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn cancel_job_phase4(&self, principal: &Principal, job_id: Uuid, tenant_id: Uuid) -> Result<()> {
        self.cancel_job(principal, job_id, tenant_id).await
    }

    // Internal (trusted) methods that bypass authorization.
    // These must only be called by scheduler, worker agent, and tests
    // with an explicitly trusted service principal.
    pub async fn submit_job_internal(&self, tenant_id: Uuid, url: &str, operation: &str, payload: serde_json::Value, idempotency_key: Option<&str>, max_attempts: i32) -> Result<Job> {
        self.submit_job(&Principal::service("internal"), tenant_id, url, operation, payload, idempotency_key, max_attempts).await
    }

    pub async fn get_job_internal(&self, job_id: Uuid, tenant_id: Uuid) -> Result<Job> {
        self.get_job(&Principal::service("internal"), job_id, tenant_id).await
    }

    pub async fn list_jobs_internal(&self, tenant_id: Uuid, limit: i64) -> Result<Vec<Job>> {
        self.list_jobs(&Principal::service("internal"), tenant_id, limit).await
    }

    pub async fn cancel_job_internal(&self, job_id: Uuid, tenant_id: Uuid) -> Result<()> {
        self.cancel_job(&Principal::service("internal"), job_id, tenant_id).await
    }

}

// ---------------------------------------------------------------------------
// Phase: Lease & Fencing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Lease {
    pub lease_id: Uuid,
    pub resource_type: String,
    pub resource_id: String,
    pub owner_id: Uuid,
    pub fencing_token: i64,
    pub expires_at: DateTime<Utc>,
}

impl ControlPlane {
    pub async fn acquire_lease(&self, resource_type: &str, resource_id: &str, owner_id: Uuid, ttl_secs: i64) -> Result<Lease> {
        let mut tx = self.pool().begin().await?;
        // expire old
        sqlx::query("DELETE FROM leases WHERE resource_type=$1 AND resource_id=$2 AND expires_at < now()")
            .bind(resource_type).bind(resource_id).execute(&mut *tx).await?;
        // check existing
        let existing = sqlx::query("SELECT lease_id, owner_id, fencing_token, expires_at FROM leases WHERE resource_type=$1 AND resource_id=$2 FOR UPDATE")
            .bind(resource_type).bind(resource_id).fetch_optional(&mut *tx).await?;
        if let Some(r) = existing {
            let expires: DateTime<Utc> = r.get("expires_at");
            if expires > Utc::now() {
                return Err(ControlPlaneError::Conflict("lease held".into()));
            }
        }
        let token: i64 = sqlx::query_scalar("SELECT nextval('fencing_token_seq')")
            .fetch_one(&mut *tx).await?;
        let lease_id = Uuid::new_v4();
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl_secs);
        sqlx::query(
            "INSERT INTO leases (lease_id, resource_type, resource_id, owner_id, fencing_token, expires_at) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (resource_type, resource_id) DO UPDATE SET owner_id=$4, fencing_token=$5, expires_at=$6, renewed_at=now()",
        )
        .bind(lease_id).bind(resource_type).bind(resource_id).bind(owner_id).bind(token).bind(expires_at)
        .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Lease { lease_id, resource_type: resource_type.to_string(), resource_id: resource_id.to_string(), owner_id, fencing_token: token, expires_at })
    }

    pub async fn validate_fencing(&self, resource_type: &str, resource_id: &str, fencing_token: i64) -> Result<bool> {
        let row = sqlx::query("SELECT fencing_token FROM leases WHERE resource_type=$1 AND resource_id=$2")
            .bind(resource_type).bind(resource_id).fetch_optional(self.pool()).await?;
        match row {
            Some(r) => {
                let current: i64 = r.get("fencing_token");
                Ok(current == fencing_token)
            }
            None => Ok(false),
        }
    }

    pub async fn renew_lease(&self, lease_id: Uuid, ttl_secs: i64) -> Result<()> {
        let res = sqlx::query("UPDATE leases SET expires_at = now() + ($2 || ' seconds')::interval, renewed_at = now() WHERE lease_id = $1 AND expires_at > now()")
            .bind(lease_id).bind(ttl_secs).execute(self.pool()).await?;
        if res.rows_affected() == 0 {
            return Err(ControlPlaneError::Conflict("lease expired or not found".into()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Basic authorization primitives
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Principal {
    pub subject: String,
    pub tenant_id: Option<Uuid>,
    pub is_admin: bool,
    pub is_worker: bool,
}

impl Principal {
    pub fn can_access_tenant(&self, tenant_id: Uuid) -> bool {
        self.is_admin || self.tenant_id == Some(tenant_id)
    }

    pub fn service(name: &str) -> Self {
        Self {
            subject: format!("service:{}", name),
            tenant_id: None,
            is_admin: true,
            is_worker: false,
        }
    }

    pub fn tenant(tenant_id: Uuid) -> Self {
        Self {
            subject: format!("tenant:{}", tenant_id),
            tenant_id: Some(tenant_id),
            is_admin: false,
            is_worker: false,
        }
    }
}

impl ControlPlane {
    pub fn authorize_tenant(&self, principal: &Principal, tenant_id: Uuid) -> Result<()> {
        if principal.can_access_tenant(tenant_id) {
            Ok(())
        } else {
            Err(ControlPlaneError::Conflict("unauthorized tenant access".into()))
        }
    }
}
