# Observability Test Evidence

## Prometheus targets
- control-plane: up
- scheduler: up
- worker-agent: up

## JSON logging
- Services emit JSON structured logs (verified in container logs).

## OTel collector
- Running and accepting OTLP (config validated).

## Grafana
- Running (http://127.0.0.1:3000).

## Limitations
- Dashboards/alerts not yet provisioned.
- Distributed tracing not implemented (OTLP collector present but no SDK export).
- Metrics are basic (counters from telemetry crate); not all specified metrics emitted.

## Phase 6.2.1
- Jaeger service registered: submit_job.
- Trace ID example: 3bf0b7feb78ce6cc3feef12643e2a726 (1 span).
- Prometheus targets: control-plane, scheduler, worker-agent UP.
- JSON logging verified.
