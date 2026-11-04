//! Prometheus metrics and OpenTelemetry tracing for Obscura Next.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Atomic counter metric.
#[derive(Debug, Default)]
pub struct Counter {
    val: AtomicU64,
}

impl Counter {
    pub fn new() -> Self {
        Self {
            val: AtomicU64::new(0),
        }
    }

    pub fn inc(&self) {
        self.val.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_by(&self, n: u64) {
        self.val.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.val.load(Ordering::Relaxed)
    }
}

/// Atomic gauge metric.
#[derive(Debug, Default)]
pub struct Gauge {
    val: AtomicI64,
}

impl Gauge {
    pub fn new() -> Self {
        Self {
            val: AtomicI64::new(0),
        }
    }

    pub fn set(&self, v: i64) {
        self.val.store(v, Ordering::Relaxed);
    }

    pub fn inc(&self) {
        self.val.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec(&self) {
        self.val.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.val.load(Ordering::Relaxed)
    }
}

/// Cumulative histogram for latency/duration tracking.
#[derive(Debug)]
pub struct Histogram {
    buckets: Vec<f64>,
    counts: Vec<AtomicU64>,
    sum: AtomicU64, // float as microsecond integer
    count: AtomicU64,
}

impl Histogram {
    pub fn with_buckets(buckets: Vec<f64>) -> Self {
        let n = buckets.len();
        let mut counts = Vec::with_capacity(n + 1);
        for _ in 0..=n {
            counts.push(AtomicU64::new(0));
        }
        Self {
            buckets,
            counts,
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, value_seconds: f64) {
        let us = (value_seconds * 1_000_000.0) as u64;
        self.sum.fetch_add(us, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);

        let mut placed = false;
        for (i, &bucket) in self.buckets.iter().enumerate() {
            if value_seconds <= bucket {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
                placed = true;
                break;
            }
        }
        if !placed {
            // Falls into +Inf
            self.counts[self.buckets.len()].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn observe_duration(&self, duration: Duration) {
        self.observe(duration.as_secs_f64());
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn sum(&self) -> f64 {
        (self.sum.load(Ordering::Relaxed) as f64) / 1_000_000.0
    }
}

/// Registry of Prometheus metrics.
#[derive(Clone)]
pub struct MetricsRegistry {
    counters: Arc<RwLock<HashMap<String, (String, Arc<Counter>)>>>,
    gauges: Arc<RwLock<HashMap<String, (String, Arc<Gauge>)>>>,
    histograms: Arc<RwLock<HashMap<String, (String, Arc<Histogram>)>>>,
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsRegistry {
    pub fn new() -> Self {
        let reg = Self {
            counters: Arc::new(RwLock::new(HashMap::new())),
            gauges: Arc::new(RwLock::new(HashMap::new())),
            histograms: Arc::new(RwLock::new(HashMap::new())),
        };
        reg.register_standard_metrics();
        reg
    }

    fn register_standard_metrics(&self) {
        self.register_gauge(
            "obscura_sessions_active",
            "Number of currently active browser sessions",
        );
        self.register_counter(
            "obscura_sessions_total",
            "Total number of browser sessions created",
        );
        self.register_gauge(
            "obscura_worker_processes_count",
            "Number of supervised worker processes",
        );
        self.register_counter(
            "obscura_worker_processes_total",
            "Total number of worker processes spawned",
        );
        self.register_gauge(
            "obscura_worker_processes_healthy",
            "Number of healthy supervised worker processes",
        );
        self.register_counter(
            "obscura_worker_processes_failed",
            "Total number of worker processes that failed or terminated unexpectedly",
        );
        self.register_counter(
            "obscura_worker_restarts_total",
            "Total number of worker process restarts",
        );
        self.register_gauge(
            "obscura_worker_active_requests",
            "Number of currently active worker requests",
        );
        self.register_counter(
            "obscura_sessions_failed_total",
            "Total number of browser sessions that failed",
        );
        self.register_histogram(
            "obscura_worker_request_duration_seconds",
            "Worker request handling duration in seconds",
            vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.5, 1.0, 5.0, 30.0],
        );
        self.register_gauge(
            "obscura_worker_memory_rss_bytes",
            "Resident memory allocated by workers in bytes",
        );
        self.register_counter(
            "obscura_errors_total",
            "Total count of errors encountered across operations",
        );
        self.register_counter(
            "obscura_security_blocks_total",
            "Total count of requests blocked by SSRF or security policies",
        );
        self.register_counter(
            "obscura_watchdog_terminations_total",
            "Total count of preemptive watchdog isolate terminations",
        );
        self.register_counter(
            "obscura_worker_spawn_total",
            "Total count of worker processes spawned",
        );
        self.register_counter(
            "obscura_worker_crash_total",
            "Total count of worker process crashes or unexpected terminations",
        );
        self.register_counter(
            "obscura_ipc_requests_total",
            "Total count of IPC requests dispatched between supervisor and workers",
        );
        self.register_counter("obscura_scheduler_jobs_received_total", "Total scheduler jobs received");
        self.register_counter("obscura_scheduler_jobs_scheduled_total", "Total scheduler jobs scheduled");
        self.register_counter("obscura_scheduler_jobs_dispatched_total", "Total scheduler jobs dispatched");
        self.register_counter("obscura_scheduler_jobs_rejected_total", "Total scheduler jobs rejected");
        self.register_counter("obscura_scheduler_dispatch_errors_total", "Total scheduler dispatch errors");
        self.register_gauge("obscura_scheduler_queue_depth", "Scheduler queue depth");
        self.register_gauge("obscura_scheduler_active_jobs", "Scheduler active jobs");
        self.register_histogram("obscura_scheduler_dispatch_duration_seconds", "Scheduler dispatch duration", vec![0.001,0.005,0.01,0.05,0.1,0.5,1.0,5.0]);
        self.register_histogram("obscura_scheduler_job_wait_duration_seconds", "Job wait duration", vec![0.01,0.05,0.1,0.5,1.0,5.0,30.0]);
        self.register_counter("obscura_scheduler_retries_total", "Total scheduler retries");
        self.register_counter("obscura_scheduler_worker_selection_total", "Total worker selections");
        self.register_gauge("obscura_worker_active", "Active workers");
        self.register_counter("obscura_worker_restarts_total", "Worker restarts");
        self.register_gauge("obscura_worker_heartbeat_age_seconds", "Worker heartbeat age");
        self.register_counter("obscura_lease_acquisitions_total", "Lease acquisitions");
        self.register_counter("obscura_lease_expirations_total", "Lease expirations");
        self.register_counter("obscura_stale_worker_rejections_total", "Stale worker rejections");

        self.register_gauge("obscura_outbox_pending", "Number of pending outbox events");
        self.register_counter("obscura_outbox_published_total", "Total outbox events published");
        self.register_counter("obscura_outbox_failed_total", "Total outbox publish failures");
        self.register_counter("obscura_outbox_retries_total", "Total outbox retries");
        self.register_histogram("obscura_outbox_publish_duration_seconds", "Outbox publish duration", vec![0.001,0.005,0.01,0.05,0.1,0.5,1.0,5.0]);
        self.register_counter("obscura_nats_publish_ack_total", "Total NATS publish acks");
        self.register_counter("obscura_nats_publish_errors_total", "Total NATS publish errors");
        self.register_counter("obscura_nats_redeliveries_total", "Total NATS redeliveries");
        self.register_gauge("obscura_nats_connection_status", "NATS connection status (1=connected)");

        self.register_counter(
            "obscura_worker_requests_total",
            "Total count of worker requests handled",
        );
        self.register_counter(
            "obscura_worker_request_errors_total",
            "Total count of worker request errors",
        );
        self.register_histogram(
            "obscura_ipc_latency_seconds",
            "IPC round-trip request latency in seconds",
            vec![0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.5, 1.0, 5.0],
        );
        self.register_histogram(
            "obscura_navigation_duration_seconds",
            "Page navigation duration in seconds",
            vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0],
        );
        self.register_histogram(
            "obscura_eval_duration_seconds",
            "JavaScript evaluation duration in seconds",
            vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0],
        );
    }

    pub fn register_counter(&self, name: &str, help: &str) -> Arc<Counter> {
        let counter = Arc::new(Counter::new());
        self.counters
            .write()
            .insert(name.to_string(), (help.to_string(), counter.clone()));
        counter
    }

    pub fn counter(&self, name: &str) -> Option<Arc<Counter>> {
        self.counters.read().get(name).map(|(_, c)| c.clone())
    }

    pub fn register_gauge(&self, name: &str, help: &str) -> Arc<Gauge> {
        let gauge = Arc::new(Gauge::new());
        self.gauges
            .write()
            .insert(name.to_string(), (help.to_string(), gauge.clone()));
        gauge
    }

    pub fn gauge(&self, name: &str) -> Option<Arc<Gauge>> {
        self.gauges.read().get(name).map(|(_, g)| g.clone())
    }

    pub fn register_histogram(&self, name: &str, help: &str, buckets: Vec<f64>) -> Arc<Histogram> {
        let histogram = Arc::new(Histogram::with_buckets(buckets));
        self.histograms
            .write()
            .insert(name.to_string(), (help.to_string(), histogram.clone()));
        histogram
    }

    pub fn histogram(&self, name: &str) -> Option<Arc<Histogram>> {
        self.histograms.read().get(name).map(|(_, h)| h.clone())
    }

    /// Renders all metrics in the Prometheus exposition text format.
    pub fn to_prometheus_text(&self) -> String {
        let mut out = String::new();

        // Counters
        let counters = self.counters.read();
        for (name, (help, counter)) in counters.iter() {
            out.push_str(&format!("# HELP {name} {help}\n"));
            out.push_str(&format!("# TYPE {name} counter\n"));
            out.push_str(&format!("{name} {}\n", counter.get()));
        }

        // Gauges
        let gauges = self.gauges.read();
        for (name, (help, gauge)) in gauges.iter() {
            out.push_str(&format!("# HELP {name} {help}\n"));
            out.push_str(&format!("# TYPE {name} gauge\n"));
            out.push_str(&format!("{name} {}\n", gauge.get()));
        }

        // Histograms
        let histograms = self.histograms.read();
        for (name, (help, hist)) in histograms.iter() {
            out.push_str(&format!("# HELP {name} {help}\n"));
            out.push_str(&format!("# TYPE {name} histogram\n"));
            let mut cumulative = 0;
            for (i, &bucket) in hist.buckets.iter().enumerate() {
                cumulative += hist.counts[i].load(Ordering::Relaxed);
                out.push_str(&format!("{name}_bucket{{le=\"{bucket}\"}} {cumulative}\n"));
            }
            cumulative += hist.counts[hist.buckets.len()].load(Ordering::Relaxed);
            out.push_str(&format!("{name}_bucket{{le=\"+Inf\"}} {cumulative}\n"));
            out.push_str(&format!("{name}_sum {:.6}\n", hist.sum()));
            out.push_str(&format!("{name}_count {}\n", hist.count()));
        }

        out
    }
}

/// Global metrics singleton helper.
static GLOBAL_REGISTRY: std::sync::OnceLock<MetricsRegistry> = std::sync::OnceLock::new();

pub fn global_metrics() -> &'static MetricsRegistry {
    GLOBAL_REGISTRY.get_or_init(MetricsRegistry::new)
}

/// OpenTelemetry trace context carrier and propagation utilities.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub trace_flags: String,
}

impl TraceContext {
    pub fn new_random() -> Self {
        use std::time::SystemTime;
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        Self {
            trace_id: format!("{ts:016x}{pid:016x}"),
            span_id: format!("{ts:016x}"),
            trace_flags: "01".to_string(),
        }
    }

    /// Formats as W3C `traceparent` header: `00-{trace_id}-{span_id}-{trace_flags}`
    pub fn to_traceparent(&self) -> String {
        format!("00-{}-{}-{}", self.trace_id, self.span_id, self.trace_flags)
    }

    /// Parses from W3C `traceparent` header
    pub fn from_traceparent(val: &str) -> Option<Self> {
        let parts: Vec<&str> = val.split('-').collect();
        if parts.len() == 4 && parts[0] == "00" {
            Some(Self {
                trace_id: parts[1].to_string(),
                span_id: parts[2].to_string(),
                trace_flags: parts[3].to_string(),
            })
        } else {
            None
        }
    }
}

use std::net::SocketAddr;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// Lightweight HTTP server exposing Prometheus metrics and a health check endpoint.
#[derive(Clone)]
pub struct MetricsServer {
    bind_addr: String,
    registry: MetricsRegistry,
    started_at: Instant,
}

impl MetricsServer {
    pub fn new(bind_addr: impl Into<String>, registry: MetricsRegistry) -> Self {
        Self {
            bind_addr: bind_addr.into(),
            registry,
            started_at: Instant::now(),
        }
    }

    /// Spawns the HTTP metrics server in a background Tokio task.
    /// Returns the local bound address and a cancellation trigger.
    pub async fn spawn(&self) -> std::io::Result<(SocketAddr, oneshot::Sender<()>)> {
        let listener = TcpListener::bind(&self.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let registry = self.registry.clone();
        let started_at = self.started_at;

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((stream, _)) => {
                                let reg = registry.clone();
                                tokio::spawn(async move {
                                    handle_http_connection(stream, reg, started_at).await;
                                });
                            }
                            Err(e) => {
                                tracing::error!("Metrics server accept error: {}", e);
                            }
                        }
                    }
                }
            }
        });

        Ok((local_addr, shutdown_tx))
    }
}

async fn handle_http_connection(mut stream: TcpStream, registry: MetricsRegistry, started_at: Instant) {
    let mut buf = [0u8; 2048];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let req_str = String::from_utf8_lossy(&buf[..n]);
    let first_line = req_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n").await;
        return;
    }

    let method = parts[0];
    let path = parts[1];

    if method != "GET" && method != "HEAD" {
        let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n").await;
        return;
    }

    match path {
        "/metrics" => {
            let body = registry.to_prometheus_text();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            if method == "GET" {
                let _ = stream.write_all(body.as_bytes()).await;
            }
        }
        "/health" | "/healthz" => {
            let uptime = started_at.elapsed().as_secs();
            let body = format!(
                r#"{{"status":"healthy","uptime_seconds":{uptime},"active_sessions":{},"workers":{}}}"#,
                registry.gauge("obscura_sessions_active").map(|g| g.get()).unwrap_or(0),
                registry.gauge("obscura_worker_processes_count").map(|g| g.get()).unwrap_or(0),
            );
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            if method == "GET" {
                let _ = stream.write_all(body.as_bytes()).await;
            }
        }
        _ => {
            let body = "Not Found\n";
            let header = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
        }
    }
    let _ = stream.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_and_gauge() {
        let counter = Counter::new();
        assert_eq!(counter.get(), 0);
        counter.inc();
        counter.inc_by(4);
        assert_eq!(counter.get(), 5);

        let gauge = Gauge::new();
        assert_eq!(gauge.get(), 0);
        gauge.set(100);
        gauge.inc();
        gauge.dec();
        assert_eq!(gauge.get(), 100);
    }

    #[test]
    fn test_histogram_observation() {
        let hist = Histogram::with_buckets(vec![0.1, 0.5, 1.0]);
        hist.observe(0.05);
        hist.observe(0.3);
        hist.observe(0.8);
        hist.observe(2.5);

        assert_eq!(hist.count(), 4);
        assert!(hist.sum() > 3.6 && hist.sum() < 3.7);
    }

    #[test]
    fn test_registry_prometheus_text() {
        let reg = MetricsRegistry::new();
        reg.counter("obscura_sessions_total").unwrap().inc_by(3);
        reg.gauge("obscura_sessions_active").unwrap().set(2);

        let text = reg.to_prometheus_text();
        assert!(text.contains("obscura_sessions_total 3"));
        assert!(text.contains("obscura_sessions_active 2"));
        assert!(text.contains("# TYPE obscura_sessions_total counter"));
        assert!(text.contains("# TYPE obscura_sessions_active gauge"));
    }

    #[tokio::test]
    async fn test_metrics_server_endpoints() {
        let reg = MetricsRegistry::new();
        reg.counter("obscura_sessions_total").unwrap().inc_by(7);
        reg.gauge("obscura_sessions_active").unwrap().set(3);
        reg.gauge("obscura_worker_processes_count").unwrap().set(2);

        let server = MetricsServer::new("127.0.0.1:0", reg);
        let (addr, shutdown_tx) = server.spawn().await.expect("bind server");

        // Test /metrics
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut resp = String::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = stream.read(&mut buf).await {
            if n == 0 { break; }
            resp.push_str(&String::from_utf8_lossy(&buf[..n]));
            if resp.contains("obscura_sessions_total 7") { break; }
        }
        assert!(resp.contains("HTTP/1.1 200 OK"));
        assert!(resp.contains("obscura_sessions_total 7"));

        // Test /health
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
        let mut resp = String::new();
        while let Ok(n) = stream.read(&mut buf).await {
            if n == 0 { break; }
            resp.push_str(&String::from_utf8_lossy(&buf[..n]));
            if resp.contains(r#""status":"healthy""#) { break; }
        }
        assert!(resp.contains("HTTP/1.1 200 OK"));
        assert!(resp.contains(r#""status":"healthy""#));
        assert!(resp.contains(r#""active_sessions":3"#));

        let _ = shutdown_tx.send(());
    }
}

