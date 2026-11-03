# Architecture

Obscura Next v0.2.0: supervised multi-process worker engine.

- obscura-worker: framed JSON IPC, per-session Tokio mutexes.
- obscura-supervisor: real child process spawning, IPC, restart.
- obscura-telemetry: Prometheus metrics HTTP server.
- SDKs: Python, TypeScript.
