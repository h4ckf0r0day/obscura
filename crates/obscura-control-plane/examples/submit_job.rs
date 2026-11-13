use obscura_control_plane::{ControlPlane, Principal};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    obscura_telemetry::init_tracing("submit_job", &std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_else(|_| "http://otel-collector:4317".to_string()));
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let secret = std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET").expect("secret");
    let cp = ControlPlane::connect(&db_url, 5).await?;
    cp.migrate().await?;
    let tenant = cp.create_tenant(&format!("phase6-verify-{}", uuid::Uuid::new_v4())).await?;
    let tenant_id = tenant.tenant_id;
    let principal = cp.service_principal("verifier", &secret)?;
    let job = cp
        .submit_job(
            &principal,
            tenant_id,
            "https://example.com",
            "navigate",
            serde_json::json!({}),
            Some("phase6-verify-key"),
            3,
        )
        .await?;
    println!("TENANT_ID={}", tenant_id);
    println!("JOB_ID={}", job.job_id);
    // poll
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let j = cp.get_job(&principal, job.job_id, tenant_id).await?;
        println!("STATUS={}", j.status);
        if j.status == "completed" || j.status == "failed" {
            println!("RESULT={:?}", j);
            break;
        }
    }
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    Ok(())
}
