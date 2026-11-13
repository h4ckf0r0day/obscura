# Phase 6 Service Lifecycle

- control-plane: connects DB, migrates, serves health.
- scheduler: validates config, connects DB+NATS, reconciles, runs loops, handles SIGTERM.
- worker-agent: registers workers, heartbeats, spawns runtime workers.
- obscura-supervisor: multi-worker CDP server.

Readiness checks use HTTP endpoints; liveness via process + dependency checks.
