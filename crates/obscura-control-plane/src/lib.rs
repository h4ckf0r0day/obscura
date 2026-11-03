//! Distributed control plane persistence for Obscura Next (PostgreSQL).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
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
        tenant_id: Uuid,
        url: &str,
        operation: &str,
        payload: serde_json::Value,
        idempotency_key: Option<&str>,
        max_attempts: i32,
    ) -> Result<Job> {
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
                return self.get_job(existing.0, tenant_id).await;
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
        self.get_job(job_id, tenant_id).await
    }

    pub async fn get_job(&self, job_id: Uuid, tenant_id: Uuid) -> Result<Job> {
        let row = sqlx::query(
            "SELECT job_id, tenant_id, status, url, operation, payload, idempotency_key, attempt_count, max_attempts, fencing_token, worker_id, created_at, updated_at FROM jobs WHERE job_id = $1 AND tenant_id = $2",
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
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    pub async fn list_jobs(&self, tenant_id: Uuid, limit: i64) -> Result<Vec<Job>> {
        let rows = sqlx::query(
            "SELECT job_id, tenant_id, status, url, operation, payload, idempotency_key, attempt_count, max_attempts, fencing_token, worker_id, created_at, updated_at FROM jobs WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2",
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
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            })
            .collect())
    }

    pub async fn cancel_job(&self, job_id: Uuid, tenant_id: Uuid) -> Result<()> {
        let res = sqlx::query(
            "UPDATE jobs SET status = 'cancelled', updated_at = now() WHERE job_id = $1 AND tenant_id = $2 AND status IN ('pending','queued','assigned')",
        )
        .bind(job_id)
        .bind(tenant_id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(ControlPlaneError::Conflict("job cannot be cancelled".into()));
        }
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
            .submit_job(tenant.tenant_id, "https://example.com", "snapshot", serde_json::json!({}), Some("key-1"), 3)
            .await
            .expect("submit");
        assert_eq!(job.status, "pending");

        // idempotency
        let job2 = cp
            .submit_job(tenant.tenant_id, "https://example.com", "snapshot", serde_json::json!({}), Some("key-1"), 3)
            .await
            .expect("submit2");
        assert_eq!(job.job_id, job2.job_id);

        let fetched = cp.get_job(job.job_id, tenant.tenant_id).await.expect("get");
        assert_eq!(fetched.job_id, job.job_id);

        let list = cp.list_jobs(tenant.tenant_id, 10).await.expect("list");
        assert!(!list.is_empty());

        cp.cancel_job(job.job_id, tenant.tenant_id).await.expect("cancel");
    }
}
