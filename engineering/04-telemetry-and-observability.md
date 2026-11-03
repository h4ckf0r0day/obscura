# Telemetry & Observability Specification

## 1. Observability Architecture

Obscura Next integrates first-class telemetry:
- **Prometheus Metrics**: High-resolution metrics exposed via an HTTP `/metrics` endpoint.
- **OpenTelemetry Tracing**: Distributed trace propagation across supervisor, workers, and browser tasks.
- **Structured JSON Logging**: Leveled log events with contextual fields (`session_id`, `target_id`, `url`, `duration_ms`).

```
                    [Browser Session / Worker]
                               │
            ┌──────────────────┴──────────────────┐
            ▼                                     ▼
   [Prometheus Registry]               [OpenTelemetry Tracer]
   - Counters                          - Spans (session, nav, eval)
   - Gauges                            - Events & Attributes
   - Histograms                        - Context Propagation
            │                                     │
            ▼                                     ▼
    GET /metrics                        OTLP Exporter / Collector
```

---

## 2. Standard Metric Definitions

### Session & Worker Metrics
| Metric Name | Type | Labels | Description |
|---|---|---|---|
| `obscura_sessions_active` | Gauge | `worker_id` | Number of currently active browser sessions |
| `obscura_sessions_total` | Counter | `worker_id`, `status` | Total sessions created over time |
| `obscura_worker_processes_count` | Gauge | `state` (`ready`, `busy`, `draining`) | Number of managed worker processes |
| `obscura_worker_memory_rss_bytes` | Gauge | `worker_id`, `pid` | Resident memory of worker process in bytes |

### Latency & Performance Metrics
| Metric Name | Type | Labels | Description |
|---|---|---|---|
| `obscura_navigation_duration_seconds` | Histogram | `status_code`, `domain` | Page navigation wall-clock duration |
| `obscura_eval_duration_seconds` | Histogram | `outcome` (`ok`, `error`, `timeout`) | JavaScript evaluation latency |
| `obscura_render_duration_seconds` | Histogram | `output_type` (`png`, `pdf`) | Layout and paint latency |

### Error & Security Metrics
| Metric Name | Type | Labels | Description |
|---|---|---|---|
| `obscura_errors_total` | Counter | `category`, `code` | Total error count across all operations |
| `obscura_security_blocks_total` | Counter | `reason` (`ssrf`, `file_access`, `watchdog`) | Requests or operations blocked by security filters |
| `obscura_watchdog_terminations_total` | Counter | `worker_id` | Isolate watchdog terminations triggered |

---

## 3. Distributed Tracing Spans

### Span Hierarchy
```text
trace: session_request
  └── span: session.create [session_id=sess-123]
        └── span: page.navigate [url="https://example.com", wait="load"]
              ├── span: net.fetch [method="GET", url="https://example.com"]
              ├── span: dom.parse [bytes=12840]
              └── span: v8.eval [script="inline:0"]
```

### Trace Context Propagation
When client requests carry W3C Trace Context headers (`traceparent`, `tracestate`), Obscura Next extracts and attaches the parent context so client AI agents have full distributed traces across the agent framework and the browser runtime.
