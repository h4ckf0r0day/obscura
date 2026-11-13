# Observability Operations

## Stack
- Prometheus: http://127.0.0.1:9090
- Grafana: http://127.0.0.1:3000
- OTel Collector: 4317/4318
- Metrics endpoints: control-plane:9091, scheduler:9092, worker-agent:9093

## Logs
Services emit structured JSON logs via tracing-subscriber.

## Targets
Prometheus scrapes control-plane, scheduler, worker-agent (all up).
