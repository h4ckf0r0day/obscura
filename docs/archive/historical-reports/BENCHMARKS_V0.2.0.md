# Benchmarks v0.2.0

## Environment
- macOS darwin, aarch64
- Rust debug build
- tokio multi-thread

## Workload
- Supervisor pool of 1-2 workers.
- Session creation, navigation to data: URLs, JS evaluation.

## Results
- obscura-supervisor spawn+ping test: ~2.2s (includes V8 startup).
- session lifecycle test: ~2.8s.
- Python SDK E2E: 3 tests passed in 1.65s.
- TypeScript SDK E2E: 1 test passed in 14.9s? (process startup).

## Limitations
- Debug builds; release numbers differ.
- No distributed load tested.
