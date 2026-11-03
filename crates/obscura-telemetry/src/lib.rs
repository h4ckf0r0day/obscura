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

    #[test]
    fn test_w3c_trace_context() {
        let ctx = TraceContext::new_random();
        let parent = ctx.to_traceparent();
        let parsed = TraceContext::from_traceparent(&parent).expect("valid traceparent");
        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.span_id, ctx.span_id);
    }
}
