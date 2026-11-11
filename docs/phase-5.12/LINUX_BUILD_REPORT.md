# Phase 5.12 Linux Build Report

## Environment

- OS: Linux (container `rust:latest`, Debian bookworm, aarch64)
- Kernel: 6.x (Linux 6.x aarch64, Docker Desktop on macOS host)
- Rust toolchain: 1.99.1 (rustc/cargo as in container)
- V8: deno_core/rusty_v8 150.4.0 prebuilt
  `librusty_v8_simdutf_release_aarch64-unknown-linux-gnu.a.gz`

## V8 artifact investigation

- Initial failure: HTTP 504 from GitHub release asset.
- Distinguished as network/transient, not checksum or compile error: direct
  `curl -sI` later returned 302 and full download succeeded (35.8 MB gzip).
- No V8 version substitution or V8_FROM_SOURCE was used.

## Build commands

```bash
cargo build --release -p obscura-worker --bin obscura-runtime-worker
cargo build --release -p obscura-worker-agent
cargo build --release -p obscura-scheduler --example adversarial_e2e
```

All builds finished with exit code 0.

## Linux-specific correction

`PR_SET_PDEATHSIG` was originally set in the supervisor's `pre_exec` closure.
In a multi-threaded runtime the creating thread can exit before the process,
prematurely SIGKILLing the child. The prctl block was removed; containment is
provided by the detached external reaper and by the worker setting its own
`PR_SET_PDEATHSIG` at startup (single-threaded context). A 100 ms startup delay
was added before the supervisor handshake to avoid a spawn race.