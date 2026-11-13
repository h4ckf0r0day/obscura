use obscura_control_plane::ControlPlane;
use obscura_telemetry::MetricsServer;
use std::net::TcpListener;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_else(|_| "http://otel-collector:4317".to_string());
    obscura_telemetry::init_tracing("obscura-control-plane", &endpoint);

    let db_url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL must be set")?;
    let cp = ControlPlane::connect(&db_url, 5).await.map_err(|e| format!("db connect: {e}"))?;
    cp.migrate().await.map_err(|e| format!("migrate: {e}"))?;

    // Liveness/readiness: bind a tiny HTTP health endpoint.
    let listener = TcpListener::bind("0.0.0.0:8081")?;
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    tracing::info!("control-plane ready on 8081");
    let metrics = obscura_telemetry::global_metrics().clone();
    let ms = MetricsServer::new("0.0.0.0:9091", metrics);
    let _metrics_shutdown = ms.spawn().await.ok();
    tracing::info!("metrics server started on 9091");

    let cp_health = cp.clone();
    loop {
        tokio::select! {
            _ = signal::ctrl_c() => break,
            accept = listener.accept() => {
                if let Ok((mut stream, _)) = accept {
                    let cp = cp_health.clone();
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut buf = [0u8; 256];
                        let _ = stream.read(&mut buf).await;
                        let ok = sqlx::query("SELECT 1").execute(cp.pool()).await.is_ok();
                        if ok {
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
                        } else {
                            let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;
                        }
                    });
                }
            }
        }
    }
    Ok(())
}
