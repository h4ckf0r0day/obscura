use obscura_control_plane::{ControlPlane, NatsClient};
use obscura_scheduler::Scheduler;
use std::sync::Arc;
use std::net::TcpListener as StdTcpListener;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_else(|_| "http://otel-collector:4317".to_string());
    obscura_telemetry::init_tracing("obscura-scheduler", &endpoint);

    let db_url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL must be set")?;
    let nats_url = std::env::var("NATS_URL")
        .map_err(|_| "NATS_URL must be set")?;

    if std::env::var("OBSCURA_SERVICE_BOOTSTRAP_SECRET")
        .unwrap_or_default()
        .is_empty()
    {
        return Err("OBSCURA_SERVICE_BOOTSTRAP_SECRET must be set".into());
    }

    let cp = ControlPlane::connect(&db_url, 10).await?;
    cp.migrate().await?;
    let nats = NatsClient::connect(&nats_url).await?;
    nats.ensure_stream("OBScura_JOBS", vec!["obscura.jobs.>".to_string()])
        .await?;

    let cp_health = cp.clone();
    let nats_health = nats.clone();
    let scheduler = Scheduler::new(cp, nats);
    scheduler.reconcile().await?;

    let sched = Arc::new(scheduler);
    let sched_clone = sched.clone();
    tokio::spawn(async move {
        let _ = sched_clone.run_reconciliation_loop(10).await;
    });

    // readiness: simple log
    tracing::info!("obscura-scheduler ready");
    let ms = obscura_telemetry::MetricsServer::new("0.0.0.0:9092", obscura_telemetry::global_metrics().clone());
    let _keep = ms.spawn().await.ok();
    let health_listener = StdTcpListener::bind("0.0.0.0:8082")?;
    health_listener.set_nonblocking(true)?;
    let health_listener = tokio::net::TcpListener::from_std(health_listener)?;
    let health_task = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = health_listener.accept().await {
                let cp = cp_health.clone();
                let nats = nats_health.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8;256];
                    let _ = stream.read(&mut buf).await;
                    let db_ok = cp.health_check().await.is_ok();
                    let nats_ok = nats.ping().await.is_ok();
                    if db_ok && nats_ok {
                        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
                    } else {
                        let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;
                    }
                });
            }
        }
    });

    // run scheduling loop
    let sched_run = sched.clone();
    let run_task = tokio::spawn(async move {
        let _ = sched_run.run().await;
    });

    signal::ctrl_c().await?;
    tracing::info!("shutting down scheduler");
    run_task.abort();
    health_task.abort();
    Ok(())
}
