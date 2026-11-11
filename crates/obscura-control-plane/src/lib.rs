//! Distributed control plane persistence for Obscura Next (PostgreSQL).

use chrono::{DateTime, Utc};
use futures::StreamExt;
use obscura_telemetry::global_metrics;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use std::time::Instant;
use thiserror::Error;
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
        // SSRF guard at control plane boundary (literal IP + DNS resolution)
        if !obscura_ssrf::env_allows_private_network() {
            if let Ok(parsed) = url::Url::parse(url) {
                if let Some(host) = parsed.host_str() {
                    // Check literal IP
                    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                        if obscura_ssrf::is_forbidden_ip(ip) {
                            return Err(ControlPlaneError::Conflict("SSRF blocked".into()));
                        }
                    } else {
                        // DNS resolution check
                        use std::net::ToSocketAddrs;
                        if let Ok(addrs) = format!("{}:80", host).to_socket_addrs() {
                            for addr in addrs {
                                if obscura_ssrf::is_forbidden_ip(addr.ip()) {
                                    return Err(ControlPlaneError::Conflict(
                                        "SSRF blocked: resolved to forbidden IP".into(),
                                    ));
                                }
                            }
                        }
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

    pub async fn get_job(
        &self,
        principal: &Principal,
        job_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Job> {
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

    pub async fn list_jobs(
        &self,
        principal: &Principal,
        tenant_id: Uuid,
        limit: i64,
    ) -> Result<Vec<Job>> {
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

    pub async fn cancel_job(
        &self,
        principal: &Principal,
        job_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<()> {
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
            return Err(ControlPlaneError::Conflict(
                "job cannot be cancelled".into(),
            ));
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

        let tenant = cp
            .create_tenant(&format!("test-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some("key-1"),
                3,
            )
            .await
            .expect("submit");
        assert_eq!(job.status, "pending");

        // idempotency
        let job2 = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some("key-1"),
                3,
            )
            .await
            .expect("submit2");
        assert_eq!(job.job_id, job2.job_id);

        let fetched = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .expect("get");
        assert_eq!(fetched.job_id, job.job_id);

        let list = cp
            .list_jobs_internal(tenant.tenant_id, 10)
            .await
            .expect("list");
        assert!(!list.is_empty());

        cp.cancel_job_internal(job.job_id, tenant.tenant_id)
            .await
            .expect("cancel");
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
    pub fn new(
        event_type: &str,
        aggregate_type: &str,
        aggregate_id: &str,
        payload: serde_json::Value,
    ) -> Self {
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
        let client = async_nats::connect(url)
            .await
            .map_err(|e| ControlPlaneError::Conflict(format!("nats connect: {e}")))?;
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
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| ControlPlaneError::Conflict(format!("serialize: {e}")))?;
        use async_nats::HeaderMap;
        let mut headers = HeaderMap::new();
        headers.insert(
            "Nats-Msg-Id",
            envelope
                .event_id
                .to_string()
                .parse::<async_nats::HeaderValue>()
                .unwrap(),
        );
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
            let subject = format!(
                "obscura.{}.{}",
                row.aggregate_type,
                row.event_type.trim_start_matches("job.")
            );
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
                    if let Some(c) = metrics.counter("obscura_outbox_published_total") {
                        c.inc();
                    }
                    if let Some(c) = metrics.counter("obscura_nats_publish_ack_total") {
                        c.inc();
                    }
                    if let Some(h) = metrics.histogram("obscura_outbox_publish_duration_seconds") {
                        h.observe_duration(pub_start.elapsed());
                    }
                }
                Err(e) => {
                    sqlx::query(
                        "UPDATE outbox_events SET attempts = attempts + 1, claimed_at = NULL WHERE outbox_id = $1",
                    )
                    .bind(row.outbox_id)
                    .execute(self.control.pool())
                    .await?;
                    let metrics = global_metrics();
                    if let Some(c) = metrics.counter("obscura_outbox_failed_total") {
                        c.inc();
                    }
                    if let Some(c) = metrics.counter("obscura_nats_publish_errors_total") {
                        c.inc();
                    }
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
    consumer:
        async_nats::jetstream::consumer::Consumer<async_nats::jetstream::consumer::pull::Config>,
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

    pub async fn create_with_ack_wait(
        js: &async_nats::jetstream::Context,
        stream: &str,
        durable_name: &str,
        ack_wait: std::time::Duration,
    ) -> Result<Self> {
        let cfg = async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(durable_name.to_string()),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ack_wait,
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
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

        let cp = ControlPlane::connect(&db_url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let nats = NatsClient::connect(&nats_url).await.expect("nats");
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .expect("stream");

        let publisher = OutboxPublisher::new(cp.clone(), nats.clone(), "pub-1", 100);
        while publisher.publish_pending().await.unwrap_or(0) > 0 {}

        let tenant = cp
            .create_tenant(&format!("t3-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some("k3"),
                3,
            )
            .await
            .expect("submit");

        let n = publisher.publish_pending().await.expect("publish");
        assert!(n >= 1);

        // verify published_at set
        let row: (Option<chrono::DateTime<Utc>>,) =
            sqlx::query_as("SELECT published_at FROM outbox_events WHERE aggregate_id = $1")
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
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

        let cp = ControlPlane::connect(&db_url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");
        let nats = NatsClient::connect(&nats_url).await.expect("nats");
        nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
            .await
            .expect("stream");

        let publisher = OutboxPublisher::new(cp.clone(), nats.clone(), "pub-2", 100);
        while publisher.publish_pending().await.unwrap_or(0) > 0 {}

        let tenant = cp
            .create_tenant(&format!("t4-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        cp.submit_job_internal(
            tenant.tenant_id,
            "https://example.com",
            "snapshot",
            serde_json::json!({}),
            Some("k4"),
            3,
        )
        .await
        .expect("submit");

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
        let res = sqlx::query(
            "UPDATE workers SET last_heartbeat_at = now(), status = 'healthy' WHERE worker_id = $1",
        )
        .bind(worker_id)
        .execute(self.pool())
        .await?;
        if res.rows_affected() == 0 {
            return Err(ControlPlaneError::NotFound(worker_id.to_string()));
        }
        Ok(())
    }

    pub async fn mark_worker_unavailable(&self, worker_id: Uuid) -> Result<()> {
        let res = sqlx::query(
            "UPDATE workers SET status = 'unavailable', updated_at = now() WHERE worker_id = $1",
        )
        .bind(worker_id)
        .execute(self.pool())
        .await?;
        if res.rows_affected() == 0 {
            return Err(ControlPlaneError::NotFound(worker_id.to_string()));
        }
        Ok(())
    }

    pub async fn update_worker_child_pid(&self, worker_id: Uuid, pid: u32) -> Result<()> {
        sqlx::query(
            "UPDATE workers SET capabilities = jsonb_set(COALESCE(capabilities, '{}'::jsonb), '{child_pid}', $2::jsonb) WHERE worker_id = $1",
        )
        .bind(worker_id)
        .bind(serde_json::json!(pid))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_worker_child_pid(&self, worker_id: Uuid) -> Result<Option<u32>> {
        let row = sqlx::query_scalar::<_, Option<serde_json::Value>>(
            "SELECT capabilities->'child_pid' FROM workers WHERE worker_id = $1",
        )
        .bind(worker_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.flatten().and_then(|v| v.as_u64()).map(|u| u as u32))
    }
    pub async fn expire_stale_workers(&self, threshold_secs: i64) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "UPDATE workers SET status = 'unavailable' WHERE status = 'healthy' AND last_heartbeat_at < now() - ($1 || ' seconds')::interval RETURNING worker_id",
        )
        .bind(threshold_secs)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(|r| r.get("worker_id")).collect())
    }

    pub async fn list_healthy_workers(&self, tenant_id: Option<Uuid>) -> Result<Vec<WorkerRecord>> {
        let rows = sqlx::query(
            "SELECT worker_id, tenant_id, status, capabilities, capacity, active_jobs, last_heartbeat_at FROM workers WHERE status = 'healthy' AND last_heartbeat_at > now() - interval '30 seconds' AND ($1::uuid IS NULL OR tenant_id IS NULL OR tenant_id = $1)",
        )
        .bind(tenant_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| WorkerRecord {
                worker_id: r.get("worker_id"),
                tenant_id: r.get("tenant_id"),
                status: r.get("status"),
                capabilities: r.get("capabilities"),
                capacity: r.get("capacity"),
                active_jobs: r.get("active_jobs"),
                last_heartbeat_at: r.get("last_heartbeat_at"),
            })
            .collect())
    }

    /// Atomically claim a pending job for scheduling, optionally scoped to a tenant. Returns job if claimed.
    pub async fn claim_pending_job_for_tenant(
        &self,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Job>> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT job_id, tenant_id, status, url, operation, payload, idempotency_key, attempt_count, max_attempts, fencing_token, worker_id, dispatch_id, execution_attempt_id, created_at, updated_at \
             FROM jobs \
             WHERE status = 'pending' \
             AND ($1::uuid IS NULL OR tenant_id = $1) \
             AND EXISTS ( \
                 SELECT 1 FROM workers \
                 WHERE workers.status = 'healthy' \
                 AND (workers.last_heartbeat_at IS NULL OR workers.last_heartbeat_at > now() - interval '30 seconds') \
                 AND workers.active_jobs < workers.capacity \
                 AND (workers.tenant_id IS NULL OR workers.tenant_id = jobs.tenant_id) \
             ) \
             ORDER BY created_at ASC LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?;
        let job = match row {
            Some(r) => {
                let job_id: Uuid = r.get("job_id");
                let tenant_id: Uuid = r.get("tenant_id");
                sqlx::query(
                    "UPDATE jobs SET status = 'scheduled', updated_at = now() WHERE job_id = $1",
                )
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

    /// Atomically claim a pending job for scheduling. Returns job if claimed.
    pub async fn claim_pending_job(&self) -> Result<Option<Job>> {
        self.claim_pending_job_for_tenant(None).await
    }

    pub async fn release_job_to_pending(&self, job_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE jobs SET status = 'pending', updated_at = now() WHERE job_id = $1 AND status = 'scheduled'")
            .bind(job_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn mark_dispatched(
        &self,
        job_id: Uuid,
        tenant_id: Uuid,
        worker_id: Uuid,
        dispatch_id: Uuid,
        attempt_id: Uuid,
    ) -> Result<i64> {
        let mut tx = self.pool().begin().await?;
        let token: i64 = sqlx::query_scalar("SELECT nextval('fencing_token_seq')")
            .fetch_one(&mut *tx)
            .await?;
        let res = sqlx::query(
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
        if res.rows_affected() == 0 {
            tx.rollback().await.ok();
            return Err(ControlPlaneError::Conflict(
                "job not in scheduled state".into(),
            ));
        }
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

    pub async fn complete_job(
        &self,
        principal: &Principal,
        job_id: Uuid,
        tenant_id: Uuid,
        attempt_id: Uuid,
        fencing_token: i64,
        result: serde_json::Value,
    ) -> Result<()> {
        self.authorize_tenant(principal, tenant_id)?;
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query("SELECT status, fencing_token, execution_attempt_id FROM jobs WHERE job_id=$1 AND tenant_id=$2 FOR UPDATE")
            .bind(job_id).bind(tenant_id).fetch_optional(&mut *tx).await?
            .ok_or_else(|| ControlPlaneError::NotFound(job_id.to_string()))?;
        let status: String = row.get("status");
        let current_token: i64 = row.get("fencing_token");
        let current_attempt: Option<Uuid> = row.get("execution_attempt_id");
        // Idempotent duplicate completion for the same attempt/token.
        if status == "completed" {
            if current_attempt == Some(attempt_id) && current_token == fencing_token {
                tx.commit().await?;
                return Ok(());
            }
            return Err(ControlPlaneError::Conflict("job already completed".into()));
        }
        if status != "dispatched" && status != "running" {
            return Err(ControlPlaneError::Conflict(
                "invalid job state for completion".into(),
            ));
        }
        if current_attempt != Some(attempt_id) {
            return Err(ControlPlaneError::Conflict("stale attempt id".into()));
        }
        if current_token != fencing_token {
            return Err(ControlPlaneError::Conflict("stale fencing token".into()));
        }
        let upd = sqlx::query("UPDATE jobs SET status='completed', updated_at=now(), completed_at=now() WHERE job_id=$1 AND tenant_id=$2 AND status IN ('dispatched','running') AND execution_attempt_id=$3 AND fencing_token=$4")
            .bind(job_id).bind(tenant_id).bind(attempt_id).bind(fencing_token).execute(&mut *tx).await?;
        if upd.rows_affected() != 1 {
            return Err(ControlPlaneError::Conflict(
                "completion state changed".into(),
            ));
        }
        sqlx::query(
            "UPDATE job_attempts SET status='completed', finished_at=now() WHERE attempt_id=$1",
        )
        .bind(attempt_id)
        .execute(&mut *tx)
        .await?;
        // decrement worker active
        sqlx::query("UPDATE workers SET active_jobs = GREATEST(active_jobs-1,0) WHERE worker_id = (SELECT worker_id FROM jobs WHERE job_id=$1)")
            .bind(job_id).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO job_events (job_id, event_type, payload) VALUES ($1,'completed',$2)",
        )
        .bind(job_id)
        .bind(result)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail_job(
        &self,
        principal: &Principal,
        job_id: Uuid,
        tenant_id: Uuid,
        attempt_id: Uuid,
        fencing_token: i64,
        error_code: &str,
        error_message: &str,
        retryable: bool,
    ) -> Result<()> {
        self.authorize_tenant(principal, tenant_id)?;
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT fencing_token FROM jobs WHERE job_id=$1 AND tenant_id=$2 FOR UPDATE",
        )
        .bind(job_id)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ControlPlaneError::NotFound(job_id.to_string()))?;
        let current: i64 = row.get("fencing_token");
        if current != fencing_token {
            return Err(ControlPlaneError::Conflict("stale fencing token".into()));
        }
        // get attempt count
        let row = sqlx::query(
            "SELECT attempt_count, max_attempts FROM jobs WHERE job_id=$1 AND tenant_id=$2",
        )
        .bind(job_id)
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        let attempts: i32 = row.get("attempt_count");
        let max: i32 = row.get("max_attempts");
        let new_status = if retryable && attempts < max {
            "pending"
        } else {
            "failed"
        };
        sqlx::query("UPDATE jobs SET status=$1, updated_at=now() WHERE job_id=$2 AND tenant_id=$3")
            .bind(new_status)
            .bind(job_id)
            .bind(tenant_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE job_attempts SET status='failed', finished_at=now(), error_code=$1, error_message=$2 WHERE attempt_id=$3")
            .bind(error_code).bind(error_message).bind(attempt_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE workers SET active_jobs = GREATEST(active_jobs-1,0) WHERE worker_id = (SELECT worker_id FROM jobs WHERE job_id=$1)")
            .bind(job_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn cancel_job_phase4(
        &self,
        principal: &Principal,
        job_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<()> {
        self.cancel_job(principal, job_id, tenant_id).await
    }
    pub async fn reconcile_worker_capacity(&self) -> Result<u64> {
        // Recompute active_jobs from non-terminal job attempts
        let res = sqlx::query(
            "UPDATE workers SET active_jobs = COALESCE((
                SELECT COUNT(*) FROM jobs WHERE jobs.worker_id = workers.worker_id AND jobs.status IN ('dispatched','running')
            ), 0)"
        ).execute(self.pool()).await?;
        Ok(res.rows_affected())
    }

    pub async fn recover_abandoned_executions(&self) -> Result<u64> {
        // Reclaim jobs assigned to unavailable workers or whose lease in `leases` has expired
        let res = sqlx::query(
            "UPDATE jobs SET status = 'pending', updated_at = now() \
             WHERE status IN ('dispatched', 'running') \
             AND ( \
                 worker_id IN (SELECT worker_id FROM workers WHERE status = 'unavailable') \
                 OR job_id::text IN (SELECT resource_id FROM leases WHERE resource_type = 'job' AND expires_at < now()) \
                 OR (lease_id IS NOT NULL AND lease_id IN (SELECT lease_id FROM leases WHERE expires_at < now())) \
                 OR (lease_id IS NULL AND updated_at < now() - interval '10 seconds') \
             )"
        ).execute(self.pool()).await?;
        Ok(res.rows_affected())
    }

    pub async fn reconcile_stuck_jobs(&self) -> Result<u64> {
        // Reclaim scheduled jobs that have stayed scheduled without dispatch for > 10 seconds
        let r1 = sqlx::query(
            "UPDATE jobs SET status = 'pending', updated_at = now() \
             WHERE status = 'scheduled' AND updated_at < now() - interval '10 seconds'",
        )
        .execute(self.pool())
        .await?;
        // Reclaim dispatched jobs with expired or missing leases
        let r2 = sqlx::query(
            "UPDATE jobs SET status = 'pending', updated_at = now() \
             WHERE status = 'dispatched' \
             AND ( \
                 updated_at < now() - interval '10 seconds' \
                 OR job_id::text IN (SELECT resource_id FROM leases WHERE resource_type = 'job' AND expires_at < now()) \
                 OR (lease_id IS NOT NULL AND lease_id NOT IN (SELECT lease_id FROM leases WHERE expires_at > now())) \
             )"
        ).execute(self.pool()).await?;
        Ok(r1.rows_affected() + r2.rows_affected())
    }

    // Internal (trusted) methods that bypass authorization.
    // These must only be called by scheduler, worker agent, and tests
    // with an explicitly trusted service principal.
    pub async fn submit_job_internal(
        &self,
        tenant_id: Uuid,
        url: &str,
        operation: &str,
        payload: serde_json::Value,
        idempotency_key: Option<&str>,
        max_attempts: i32,
    ) -> Result<Job> {
        self.submit_job(
            &Principal::service("internal"),
            tenant_id,
            url,
            operation,
            payload,
            idempotency_key,
            max_attempts,
        )
        .await
    }

    pub async fn get_job_internal(&self, job_id: Uuid, tenant_id: Uuid) -> Result<Job> {
        self.get_job(&Principal::service("internal"), job_id, tenant_id)
            .await
    }

    pub async fn list_jobs_internal(&self, tenant_id: Uuid, limit: i64) -> Result<Vec<Job>> {
        self.list_jobs(&Principal::service("internal"), tenant_id, limit)
            .await
    }

    pub async fn cancel_job_internal(&self, job_id: Uuid, tenant_id: Uuid) -> Result<()> {
        self.cancel_job(&Principal::service("internal"), job_id, tenant_id)
            .await
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
    pub async fn acquire_lease(
        &self,
        resource_type: &str,
        resource_id: &str,
        owner_id: Uuid,
        ttl_secs: i64,
    ) -> Result<Lease> {
        let mut tx = self.pool().begin().await?;
        // expire old
        sqlx::query(
            "DELETE FROM leases WHERE resource_type=$1 AND resource_id=$2 AND expires_at < now()",
        )
        .bind(resource_type)
        .bind(resource_id)
        .execute(&mut *tx)
        .await?;
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
            .fetch_one(&mut *tx)
            .await?;
        let lease_id = Uuid::new_v4();
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl_secs);
        sqlx::query(
            "INSERT INTO leases (lease_id, resource_type, resource_id, owner_id, fencing_token, expires_at) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (resource_type, resource_id) DO UPDATE SET owner_id=$4, fencing_token=$5, expires_at=$6, renewed_at=now()",
        )
        .bind(lease_id).bind(resource_type).bind(resource_id).bind(owner_id).bind(token).bind(expires_at)
        .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Lease {
            lease_id,
            resource_type: resource_type.to_string(),
            resource_id: resource_id.to_string(),
            owner_id,
            fencing_token: token,
            expires_at,
        })
    }

    pub async fn validate_fencing(
        &self,
        resource_type: &str,
        resource_id: &str,
        fencing_token: i64,
    ) -> Result<bool> {
        let row = sqlx::query(
            "SELECT fencing_token FROM leases WHERE resource_type=$1 AND resource_id=$2",
        )
        .bind(resource_type)
        .bind(resource_id)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(r) => {
                let current: i64 = r.get("fencing_token");
                Ok(current == fencing_token)
            }
            None => Ok(false),
        }
    }

    pub async fn update_job_fencing_token(
        &self,
        job_id: Uuid,
        tenant_id: Uuid,
        token: i64,
    ) -> Result<()> {
        sqlx::query("UPDATE jobs SET fencing_token = $1, updated_at = now() WHERE job_id = $2 AND tenant_id = $3")
            .bind(token).bind(job_id).bind(tenant_id).execute(self.pool()).await?;
        Ok(())
    }

    pub async fn update_job_lease(
        &self,
        job_id: Uuid,
        tenant_id: Uuid,
        lease_id: Uuid,
        token: i64,
    ) -> Result<()> {
        sqlx::query("UPDATE jobs SET lease_id = $1, fencing_token = $2, updated_at = now() WHERE job_id = $3 AND tenant_id = $4")
            .bind(lease_id).bind(token).bind(job_id).bind(tenant_id).execute(self.pool()).await?;
        Ok(())
    }

    pub async fn renew_lease(&self, lease_id: Uuid, ttl_secs: i64) -> Result<()> {
        let res = sqlx::query("UPDATE leases SET expires_at = now() + ($2 || ' seconds')::interval, renewed_at = now() WHERE lease_id = $1 AND expires_at > now()")
            .bind(lease_id).bind(ttl_secs).execute(self.pool()).await?;
        if res.rows_affected() == 0 {
            return Err(ControlPlaneError::Conflict(
                "lease expired or not found".into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Basic authorization primitives
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Principal {
    subject: String,
    tenant_id: Option<Uuid>,
    is_admin: bool,
    is_worker: bool,
}

impl Principal {
    pub fn can_access_tenant(&self, tenant_id: Uuid) -> bool {
        self.is_admin || self.tenant_id == Some(tenant_id)
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn tenant_id(&self) -> Option<Uuid> {
        self.tenant_id
    }
    pub fn is_admin(&self) -> bool {
        self.is_admin
    }
    pub fn is_worker(&self) -> bool {
        self.is_worker
    }

    // Controlled constructors — only callable within this crate.
    pub(crate) fn service(name: &str) -> Self {
        Self {
            subject: format!("service:{}", name),
            tenant_id: None,
            is_admin: true,
            is_worker: false,
        }
    }

    pub(crate) fn tenant(tenant_id: Uuid) -> Self {
        Self {
            subject: format!("tenant:{}", tenant_id),
            tenant_id: Some(tenant_id),
            is_admin: false,
            is_worker: false,
        }
    }

    pub(crate) fn worker(worker_id: Uuid, tenant_id: Option<Uuid>) -> Self {
        Self {
            subject: format!("worker:{}", worker_id),
            tenant_id,
            is_admin: false,
            is_worker: true,
        }
    }
}

impl ControlPlane {
    pub fn authorize_tenant(&self, principal: &Principal, tenant_id: Uuid) -> Result<()> {
        if principal.can_access_tenant(tenant_id) {
            Ok(())
        } else {
            Err(ControlPlaneError::Conflict(
                "unauthorized tenant access".into(),
            ))
        }
    }

    pub fn service_principal(&self, name: &str, bootstrap_secret: &str) -> Result<Principal> {
        let expected = std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET").unwrap_or_default();
        // Fail closed: no implicit or placeholder credentials.
        if expected.is_empty() || bootstrap_secret.is_empty() {
            return Err(ControlPlaneError::Conflict(
                "invalid bootstrap secret".into(),
            ));
        }
        if expected == "testsecret" || bootstrap_secret == "testsecret" {
            return Err(ControlPlaneError::Conflict(
                "invalid bootstrap secret".into(),
            ));
        }
        // Constant-time comparison.
        if expected.len() != bootstrap_secret.len() {
            return Err(ControlPlaneError::Conflict(
                "invalid bootstrap secret".into(),
            ));
        }
        let mut diff: u8 = 0;
        for (a, b) in expected.bytes().zip(bootstrap_secret.bytes()) {
            diff |= a ^ b;
        }
        if diff != 0 {
            return Err(ControlPlaneError::Conflict(
                "invalid bootstrap secret".into(),
            ));
        }
        Ok(Principal::service(name))
    }
}

// ---------------------------------------------------------------------------
// Credential-backed authentication
// ---------------------------------------------------------------------------

use sha2::{Digest, Sha256};

impl ControlPlane {
    pub async fn create_credential(
        &self,
        principal: &Principal,
        credential_id: &str,
        subject: &str,
        tenant_id: Option<Uuid>,
        secret: &str,
        is_admin: bool,
        is_worker: bool,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        // Authorization: only admins can create credentials
        if !principal.is_admin() {
            return Err(ControlPlaneError::Conflict("unauthorized".into()));
        }
        // Tenant admins can only create credentials for their own tenant
        if !principal.is_admin()
            || (principal.tenant_id().is_some() && principal.tenant_id() != tenant_id)
        {
            return Err(ControlPlaneError::Conflict("unauthorized tenant".into()));
        }
        let hash = hex::encode(Sha256::digest(secret.as_bytes()));
        sqlx::query(
            "INSERT INTO credentials (credential_id, subject, tenant_id, secret_hash, is_admin, is_worker, expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(credential_id)
        .bind(subject)
        .bind(tenant_id)
        .bind(hash)
        .bind(is_admin)
        .bind(is_worker)
        .bind(expires_at)
        .execute(self.pool())
        .await?;
        let _ = self
            .audit_event(
                principal.subject(),
                "credential.create",
                "credential",
                credential_id,
                "success",
                tenant_id,
            )
            .await;
        Ok(())
    }

    pub async fn authenticate(&self, credential_id: &str, secret: &str) -> Result<Principal> {
        let row = sqlx::query(
            "SELECT subject, tenant_id, secret_hash, is_admin, is_worker, revoked, expires_at FROM credentials WHERE credential_id = $1",
        )
        .bind(credential_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| ControlPlaneError::Conflict("invalid credential".into()))?;

        let stored_hash: String = row.get("secret_hash");
        let input_hash = hex::encode(Sha256::digest(secret.as_bytes()));
        // constant-time comparison using subtle
        use subtle::ConstantTimeEq;
        let ct_eq = stored_hash.as_bytes().ct_eq(input_hash.as_bytes());
        if !bool::from(ct_eq) {
            return Err(ControlPlaneError::Conflict("invalid credential".into()));
        }
        let revoked: bool = row.get("revoked");
        if revoked {
            return Err(ControlPlaneError::Conflict("credential revoked".into()));
        }
        let expires: Option<DateTime<Utc>> = row.get("expires_at");
        if let Some(exp) = expires {
            if exp < Utc::now() {
                return Err(ControlPlaneError::Conflict("credential expired".into()));
            }
        }
        let subject: String = row.get("subject");
        let tenant_id: Option<Uuid> = row.get("tenant_id");
        let is_admin: bool = row.get("is_admin");
        let is_worker: bool = row.get("is_worker");
        // Audit success
        let _ = self
            .audit_event(
                credential_id,
                "auth.success",
                "credential",
                credential_id,
                "success",
                tenant_id,
            )
            .await;
        // Construct Principal internally (controlled)
        Ok(Principal {
            subject,
            tenant_id,
            is_admin,
            is_worker,
        })
    }

    pub async fn revoke_credential(
        &self,
        principal: &Principal,
        credential_id: &str,
    ) -> Result<()> {
        if !principal.is_admin() {
            return Err(ControlPlaneError::Conflict("unauthorized".into()));
        }
        sqlx::query("UPDATE credentials SET revoked = true WHERE credential_id = $1")
            .bind(credential_id)
            .execute(self.pool())
            .await?;
        let _ = self
            .audit_event(
                principal.subject(),
                "credential.revoke",
                "credential",
                credential_id,
                "success",
                None,
            )
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[tokio::test]
    async fn test_credential_auth() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("auth-tenant-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let admin = Principal {
            subject: "admin".into(),
            tenant_id: None,
            is_admin: true,
            is_worker: false,
        };
        let cred_id = format!("cred-{}", uuid::Uuid::new_v4());
        cp.create_credential(
            &admin,
            &cred_id,
            "user1",
            Some(tenant.tenant_id),
            "secret123",
            false,
            false,
            None,
        )
        .await
        .expect("create cred");

        // valid
        let p = cp.authenticate(&cred_id, "secret123").await.expect("auth");
        assert_eq!(p.tenant_id(), Some(tenant.tenant_id));
        assert!(!p.is_admin());

        // invalid secret
        assert!(cp.authenticate(&cred_id, "wrong").await.is_err());

        // revoked
        cp.revoke_credential(&admin, &cred_id)
            .await
            .expect("revoke");
        assert!(cp.authenticate(&cred_id, "secret123").await.is_err());
    }

    #[tokio::test]
    async fn test_ssrf_loopback_rejected() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");
        let tenant = cp
            .create_tenant(&format!("ssrf-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let principal = Principal {
            subject: "admin".into(),
            tenant_id: Some(tenant.tenant_id),
            is_admin: true,
            is_worker: false,
        };

        // Loopback should be rejected
        let result = cp
            .submit_job(
                &principal,
                tenant.tenant_id,
                "http://127.0.0.1/secret",
                "snapshot",
                serde_json::json!({}),
                None,
                3,
            )
            .await;
        assert!(result.is_err(), "loopback should be rejected");

        // Private IP should be rejected
        let result = cp
            .submit_job(
                &principal,
                tenant.tenant_id,
                "http://192.168.1.1/secret",
                "snapshot",
                serde_json::json!({}),
                None,
                3,
            )
            .await;
        assert!(result.is_err(), "private IP should be rejected");

        // Metadata IP should be rejected
        let result = cp
            .submit_job(
                &principal,
                tenant.tenant_id,
                "http://169.254.169.254/latest/meta-data",
                "snapshot",
                serde_json::json!({}),
                None,
                3,
            )
            .await;
        assert!(result.is_err(), "metadata IP should be rejected");

        // Public URL should be accepted (DNS resolution may fail in test env, but literal check passes)
        // We won't test public URLs to avoid network dependency
    }
}

// ---------------------------------------------------------------------------
// Credential rotation and audit logging
// ---------------------------------------------------------------------------

impl ControlPlane {
    pub async fn rotate_credential(
        &self,
        principal: &Principal,
        old_credential_id: &str,
        new_credential_id: &str,
        new_secret: &str,
        grace_period_secs: i64,
    ) -> Result<()> {
        if !principal.is_admin() {
            return Err(ControlPlaneError::Conflict("unauthorized".into()));
        }

        // Verify old credential exists and belongs to principal's tenant
        let old_row = sqlx::query(
            "SELECT tenant_id, is_admin, is_worker FROM credentials WHERE credential_id = $1",
        )
        .bind(old_credential_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| ControlPlaneError::NotFound(old_credential_id.into()))?;

        let tenant_id: Option<Uuid> = old_row.get("tenant_id");
        if principal.tenant_id().is_some() && principal.tenant_id() != tenant_id {
            return Err(ControlPlaneError::Conflict("unauthorized tenant".into()));
        }

        // Create new credential
        let is_admin: bool = old_row.get("is_admin");
        let is_worker: bool = old_row.get("is_worker");
        let new_hash = hex::encode(Sha256::digest(new_secret.as_bytes()));
        sqlx::query(
            "INSERT INTO credentials (credential_id, subject, tenant_id, secret_hash, is_admin, is_worker, expires_at) VALUES ($1, (SELECT subject FROM credentials WHERE credential_id = $2), $3, $4, $5, $6, NULL)",
        )
        .bind(new_credential_id)
        .bind(old_credential_id)
        .bind(tenant_id)
        .bind(new_hash)
        .bind(is_admin)
        .bind(is_worker)
        .execute(self.pool())
        .await?;

        // Revoke old credential after grace period (immediate if grace_period_secs == 0)
        if grace_period_secs == 0 {
            sqlx::query("UPDATE credentials SET revoked = true WHERE credential_id = $1")
                .bind(old_credential_id)
                .execute(self.pool())
                .await?;
        }
        // Note: grace period expiry would require a background task; for now, immediate revocation only.
        // A production system would schedule revocation after grace_period_secs.

        // Audit event
        self.audit_event(
            principal.subject(),
            "credential.rotate",
            "credential",
            old_credential_id,
            "success",
            None,
        )
        .await
        .ok();

        Ok(())
    }

    pub async fn audit_event(
        &self,
        actor: &str,
        action: &str,
        resource_type: &str,
        resource_id: &str,
        outcome: &str,
        tenant_id: Option<Uuid>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_events (tenant_id, actor, action, resource_type, resource_id) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(tenant_id)
        .bind(actor)
        .bind(action)
        .bind(resource_type)
        .bind(resource_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod credential_lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn test_credential_rotation() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("cred-rot-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let admin = Principal {
            subject: "admin".into(),
            tenant_id: None,
            is_admin: true,
            is_worker: false,
        };
        let old_id = format!("old-{}", uuid::Uuid::new_v4());
        let new_id = format!("new-{}", uuid::Uuid::new_v4());
        cp.create_credential(
            &admin,
            &old_id,
            "user1",
            Some(tenant.tenant_id),
            "old-secret",
            false,
            false,
            None,
        )
        .await
        .expect("create old");

        // Rotate with immediate retirement
        cp.rotate_credential(&admin, &old_id, &new_id, "new-secret", 0)
            .await
            .expect("rotate");

        // Old credential should be revoked
        assert!(cp.authenticate(&old_id, "old-secret").await.is_err());

        // New credential should work
        let p = cp
            .authenticate(&new_id, "new-secret")
            .await
            .expect("auth new");
        assert_eq!(p.tenant_id(), Some(tenant.tenant_id));

        // Audit events should exist
        let rows = sqlx::query(
            "SELECT count(*) as c FROM audit_events WHERE action = 'credential.rotate'",
        )
        .fetch_one(cp.pool())
        .await
        .unwrap();
        let count: i64 = rows.get("c");
        assert!(count >= 1);
    }

    #[tokio::test]
    async fn test_audit_logging() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("audit-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let admin = Principal {
            subject: "admin".into(),
            tenant_id: None,
            is_admin: true,
            is_worker: false,
        };
        let cred_id = format!("audit-{}", uuid::Uuid::new_v4());
        cp.create_credential(
            &admin,
            &cred_id,
            "user1",
            Some(tenant.tenant_id),
            "secret",
            false,
            false,
            None,
        )
        .await
        .expect("create");

        // Authenticate (success)
        cp.authenticate(&cred_id, "secret").await.expect("auth");

        // Authenticate (failure)
        let _ = cp.authenticate(&cred_id, "wrong").await;

        // Check audit events
        let rows =
            sqlx::query("SELECT action FROM audit_events WHERE resource_type = 'credential'")
                .fetch_all(cp.pool())
                .await
                .unwrap();
        assert!(!rows.is_empty());
    }
}

#[cfg(test)]
mod f5_adversarial_tests {
    use super::*;

    #[tokio::test]
    async fn test_stale_completion_rejected_after_new_owner() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("f5-tenant-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let w_a = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();
        let w_b = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("f5-key-{}", uuid::Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();
        let claimed = cp
            .claim_pending_job_for_tenant(Some(tenant.tenant_id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.job_id, job.job_id);

        let attempt_a = Uuid::new_v4();
        let token_a = cp
            .mark_dispatched(job.job_id, tenant.tenant_id, w_a, Uuid::new_v4(), attempt_a)
            .await
            .unwrap();

        // B takes over ownership
        let attempt_b = Uuid::new_v4();
        sqlx::query("UPDATE jobs SET status='scheduled', updated_at=now() WHERE job_id=$1")
            .bind(job.job_id)
            .execute(cp.pool())
            .await
            .unwrap();
        let token_b = cp
            .mark_dispatched(job.job_id, tenant.tenant_id, w_b, Uuid::new_v4(), attempt_b)
            .await
            .unwrap();

        let principal = Principal {
            subject: "test".into(),
            tenant_id: None,
            is_admin: true,
            is_worker: false,
        };

        // A's stale completion rejected
        let r = cp
            .complete_job(
                &principal,
                job.job_id,
                tenant.tenant_id,
                attempt_a,
                token_a,
                serde_json::json!({"from":"A"}),
            )
            .await;
        assert!(r.is_err(), "stale completion should be rejected");

        // A's stale failure rejected
        let r = cp
            .fail_job(
                &principal,
                job.job_id,
                tenant.tenant_id,
                attempt_a,
                token_a,
                "ERR",
                "stale",
                true,
            )
            .await;
        assert!(r.is_err(), "stale failure should be rejected");

        // B completes with new token
        let r = cp
            .complete_job(
                &principal,
                job.job_id,
                tenant.tenant_id,
                attempt_b,
                token_b,
                serde_json::json!({"from":"B"}),
            )
            .await;
        assert!(r.is_ok(), "B completion should succeed");

        // Verify final state
        let final_job = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(final_job.status, "completed");
        assert_eq!(final_job.fencing_token, token_b);
    }

    #[tokio::test]
    async fn test_duplicate_dispatch_safe() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("f5-dup-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let w = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("f5-dup-key-{}", uuid::Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();

        // First claim succeeds
        let claimed1 = cp
            .claim_pending_job_for_tenant(Some(tenant.tenant_id))
            .await
            .unwrap();
        assert!(claimed1.is_some());

        // Second claim returns None (job no longer pending)
        let claimed2 = cp
            .claim_pending_job_for_tenant(Some(tenant.tenant_id))
            .await
            .unwrap();
        assert!(claimed2.is_none(), "duplicate claim should return None");

        // mark_dispatched once
        let token = cp
            .mark_dispatched(
                job.job_id,
                tenant.tenant_id,
                w,
                Uuid::new_v4(),
                Uuid::new_v4(),
            )
            .await
            .unwrap();
        assert!(token > 0);

        // Second mark_dispatched should fail (status no longer scheduled)
        let result = cp
            .mark_dispatched(
                job.job_id,
                tenant.tenant_id,
                w,
                Uuid::new_v4(),
                Uuid::new_v4(),
            )
            .await;
        assert!(result.is_err(), "duplicate dispatch should fail");
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[tokio::test]
    async fn test_capacity_normal_assignment() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("cap-tenant-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let worker = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        let attempt_id = Uuid::new_v4();
        let dispatch_id = Uuid::new_v4();
        let _job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("cap-key-{}", uuid::Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();
        let claimed = cp
            .claim_pending_job_for_tenant(Some(tenant.tenant_id))
            .await
            .unwrap()
            .unwrap();
        let token = cp
            .mark_dispatched(
                claimed.job_id,
                tenant.tenant_id,
                worker,
                dispatch_id,
                attempt_id,
            )
            .await
            .unwrap();

        let w = cp
            .list_healthy_workers(Some(tenant.tenant_id))
            .await
            .unwrap();
        let wr = w.iter().find(|x| x.worker_id == worker).unwrap();
        assert_eq!(wr.active_jobs, 1);

        // complete
        cp.complete_job(
            &Principal::service("test"),
            claimed.job_id,
            tenant.tenant_id,
            attempt_id,
            token,
            serde_json::json!({}),
        )
        .await
        .unwrap();
        let w = cp
            .list_healthy_workers(Some(tenant.tenant_id))
            .await
            .unwrap();
        let wr = w.iter().find(|x| x.worker_id == worker).unwrap();
        assert_eq!(wr.active_jobs, 0);
    }

    #[tokio::test]
    async fn test_capacity_cancel_decrements() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("cap-cancel-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let worker = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        let _job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("cap-cancel-key-{}", uuid::Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();
        let claimed = cp
            .claim_pending_job_for_tenant(Some(tenant.tenant_id))
            .await
            .unwrap()
            .unwrap();
        cp.mark_dispatched(
            claimed.job_id,
            tenant.tenant_id,
            worker,
            Uuid::new_v4(),
            Uuid::new_v4(),
        )
        .await
        .unwrap();

        let principal = Principal {
            subject: "admin".into(),
            tenant_id: None,
            is_admin: true,
            is_worker: false,
        };
        cp.cancel_job(&principal, claimed.job_id, tenant.tenant_id)
            .await
            .unwrap();

        let w = cp
            .list_healthy_workers(Some(tenant.tenant_id))
            .await
            .unwrap();
        let wr = w.iter().find(|x| x.worker_id == worker).unwrap();
        assert_eq!(wr.active_jobs, 0);
    }

    #[tokio::test]
    async fn test_capacity_reconciliation() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("cap-recon-{}", uuid::Uuid::new_v4()))
            .await
            .expect("tenant");
        let worker = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        // Manually inflate active_jobs
        sqlx::query("UPDATE workers SET active_jobs = 99 WHERE worker_id = $1")
            .bind(worker)
            .execute(cp.pool())
            .await
            .unwrap();

        // Reconcile
        let affected = cp.reconcile_worker_capacity().await.unwrap();
        assert!(affected >= 1);

        let w = cp
            .list_healthy_workers(Some(tenant.tenant_id))
            .await
            .unwrap();
        let wr = w.iter().find(|x| x.worker_id == worker).unwrap();
        assert_eq!(wr.active_jobs, 0);
    }
}

#[cfg(test)]
mod security_enforcement_tests {
    use super::*;

    #[tokio::test]
    async fn test_cross_tenant_job_access_rejected() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant_a = cp
            .create_tenant(&format!("sec-tenant-a-{}", Uuid::new_v4()))
            .await
            .expect("tenant a");
        let tenant_b = cp
            .create_tenant(&format!("sec-tenant-b-{}", Uuid::new_v4()))
            .await
            .expect("tenant b");

        let principal_a = Principal::tenant(tenant_a.tenant_id);
        let principal_b = Principal::tenant(tenant_b.tenant_id);

        // Tenant A submits a job
        let job = cp
            .submit_job(
                &principal_a,
                tenant_a.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("sec-key-{}", Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();

        // Tenant B tries to read Tenant A's job -> should fail
        let result = cp
            .get_job(&principal_b, job.job_id, tenant_a.tenant_id)
            .await;
        assert!(result.is_err(), "cross-tenant read should be rejected");

        // Tenant B tries to cancel Tenant A's job -> should fail
        let result = cp
            .cancel_job(&principal_b, job.job_id, tenant_a.tenant_id)
            .await;
        assert!(result.is_err(), "cross-tenant cancel should be rejected");
    }

    #[tokio::test]
    async fn test_unauthenticated_rejected() {
        // This test verifies that the API requires a principal.
        // Since methods now require &Principal (not Option), unauthenticated
        // callers cannot invoke them without constructing a principal.
        // Principal fields are private, so external crates cannot fabricate.
        // This is a compile-time guarantee, not a runtime test.
        // Verified by: Principal fields are private (pub(crate) constructors only).
    }
}

#[cfg(test)]
mod ssrf_tests {
    use super::*;

    #[tokio::test]
    async fn test_ssrf_loopback_rejected() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");
        let tenant = cp
            .create_tenant(&format!("ssrf-tenant-{}", Uuid::new_v4()))
            .await
            .expect("tenant");
        let principal = Principal {
            subject: "admin".into(),
            tenant_id: Some(tenant.tenant_id),
            is_admin: true,
            is_worker: false,
        };

        // Loopback should be rejected
        let result = cp
            .submit_job(
                &principal,
                tenant.tenant_id,
                "http://127.0.0.1/secret",
                "snapshot",
                serde_json::json!({}),
                None,
                3,
            )
            .await;
        assert!(result.is_err(), "loopback should be rejected");

        // Private IP should be rejected
        let result = cp
            .submit_job(
                &principal,
                tenant.tenant_id,
                "http://192.168.1.1/secret",
                "snapshot",
                serde_json::json!({}),
                None,
                3,
            )
            .await;
        assert!(result.is_err(), "private IP should be rejected");

        // Metadata IP should be rejected
        let result = cp
            .submit_job(
                &principal,
                tenant.tenant_id,
                "http://169.254.169.254/latest/meta-data",
                "snapshot",
                serde_json::json!({}),
                None,
                3,
            )
            .await;
        assert!(result.is_err(), "metadata IP should be rejected");

        // Public URL should be accepted (DNS resolution may fail in test env, but literal check passes)
        // We won't test public URLs to avoid network dependency
    }
}

#[cfg(test)]
mod nats_redelivery_tests {
    use super::*;

    #[tokio::test]
    async fn test_nats_redelivery_on_nak() {
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let nats = NatsClient::connect(&nats_url).await.expect("nats connect");
        let stream_name = format!("TEST_NAK_{}", Uuid::new_v4().simple());
        let subject = format!("obscura.nak.{}.test", Uuid::new_v4().simple());
        nats.ensure_stream(&stream_name, vec![subject.clone()])
            .await
            .expect("stream");

        let consumer_name = format!("nak-cons-{}", Uuid::new_v4().simple());
        let consumer = NatsConsumer::create_with_ack_wait(
            nats.jetstream(),
            &stream_name,
            &consumer_name,
            std::time::Duration::from_secs(30),
        )
        .await
        .expect("consumer");

        // Publish test message
        let env = EventEnvelope::new(
            "test.nak",
            "test",
            "test-1",
            serde_json::json!({"action": "test_nak"}),
        );
        nats.publish(&subject, &env).await.expect("publish");

        // Fetch first time
        let msg1 = consumer.next_message().await.expect("next").expect("msg1");
        // NAK message
        msg1.ack_with(async_nats::jetstream::AckKind::Nak(None))
            .await
            .expect("nak");

        // Fetch redelivered message
        let msg2 = consumer.next_message().await.expect("next").expect("msg2");
        let info = msg2.info().expect("info");
        assert!(
            info.delivered > 1,
            "delivery count must be > 1 after nak, got {}",
            info.delivered
        );
        msg2.ack().await.expect("ack");
    }

    #[tokio::test]
    async fn test_nats_redelivery_on_unacked_timeout() {
        let nats_url =
            std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let nats = NatsClient::connect(&nats_url).await.expect("nats connect");
        let stream_name = format!("TEST_TIMEOUT_{}", Uuid::new_v4().simple());
        let subject = format!("obscura.timeout.{}.test", Uuid::new_v4().simple());
        nats.ensure_stream(&stream_name, vec![subject.clone()])
            .await
            .expect("stream");

        let consumer_name = format!("timeout-cons-{}", Uuid::new_v4().simple());
        let consumer = NatsConsumer::create_with_ack_wait(
            nats.jetstream(),
            &stream_name,
            &consumer_name,
            std::time::Duration::from_millis(1500),
        )
        .await
        .expect("consumer");

        // Publish test message
        let env = EventEnvelope::new(
            "test.timeout",
            "test",
            "test-2",
            serde_json::json!({"action": "test_timeout"}),
        );
        nats.publish(&subject, &env).await.expect("publish");

        // Fetch first time and DO NOT ACK
        let msg1 = consumer.next_message().await.expect("next").expect("msg1");
        let info1 = msg1.info().expect("info1");
        assert_eq!(info1.delivered, 1);

        // Sleep to let ack_wait expire
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

        // Fetch again — JetStream should have redelivered the unacked message
        let msg2 = consumer.next_message().await.expect("next").expect("msg2");
        let info2 = msg2.info().expect("info2");
        assert!(
            info2.delivered >= 2,
            "delivery count must be >= 2 on timeout redelivery, got {}",
            info2.delivered
        );
        msg2.ack().await.expect("ack");
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[tokio::test]
    async fn test_recover_abandoned_executions_with_expired_leases() {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:obscura@127.0.0.1:5432/obscura".to_string());
        let cp = ControlPlane::connect(&url, 5).await.expect("connect");
        cp.migrate().await.expect("migrate");

        let tenant = cp
            .create_tenant(&format!("rec-tenant-{}", Uuid::new_v4()))
            .await
            .expect("tenant");
        let worker_a = cp
            .register_worker(Some(tenant.tenant_id), serde_json::json!({}), 4)
            .await
            .unwrap();

        // Submit job and dispatch to worker_a
        let job = cp
            .submit_job_internal(
                tenant.tenant_id,
                "https://example.com",
                "snapshot",
                serde_json::json!({}),
                Some(&format!("rec-key-{}", Uuid::new_v4())),
                3,
            )
            .await
            .unwrap();
        let claimed = cp
            .claim_pending_job_for_tenant(Some(tenant.tenant_id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.job_id, job.job_id);

        let attempt_id = Uuid::new_v4();
        let dispatch_id = Uuid::new_v4();
        let _ = cp
            .mark_dispatched(
                job.job_id,
                tenant.tenant_id,
                worker_a,
                dispatch_id,
                attempt_id,
            )
            .await
            .unwrap();

        // Acquire lease for job with short TTL and artificially expire it in DB
        let lease = cp
            .acquire_lease("job", &job.job_id.to_string(), worker_a, 1)
            .await
            .unwrap();
        cp.update_job_lease(
            job.job_id,
            tenant.tenant_id,
            lease.lease_id,
            lease.fencing_token,
        )
        .await
        .unwrap();

        // Artificially expire the lease
        sqlx::query(
            "UPDATE leases SET expires_at = now() - interval '5 seconds' WHERE lease_id = $1",
        )
        .bind(lease.lease_id)
        .execute(cp.pool())
        .await
        .unwrap();

        // Run recover_abandoned_executions
        let recovered = cp.recover_abandoned_executions().await.unwrap();
        assert!(recovered >= 1, "job with expired lease should be reclaimed");

        // Verify job is back in pending state
        let recovered_job = cp
            .get_job_internal(job.job_id, tenant.tenant_id)
            .await
            .unwrap();
        assert_eq!(recovered_job.status, "pending");
    }
}
