# Phase 6 Security Remediation

## Implemented in this phase
- `.env.example` added with required env vars and safe placeholders.
- `CODE_OF_CONDUCT.md` added.
- CDP token weak value removed from compose; compose now requires `${OBSCURA_CDP_TOKEN:?...}`.
- Metrics `/metrics` now requires `Authorization: Bearer` when `OBSCURA_METRICS_TOKEN` is set (constant-time compare).
- SSRF policy tests added in `crates/obscura-ssrf` (6 tests).
- `rust-toolchain.toml` pinned to 1.99.1.

## Remaining
- OS-level sandbox not implemented; in-process isolation only. Design documented below.
- `validate_url` file:// redirect consistency: partially fixed; needs redirect-path audit.
- Metrics auth is opt-in; default open (documented as private-network only).

## Browser isolation architecture (design)
Current model: V8 isolates + process supervision + PR_SET_PDEATHSIG + external reaper. This is containment, not an OS sandbox.

Proposed Linux architecture:
- Dedicated unprivileged user per worker.
- Per-job process (runtime worker already separate process).
- Linux namespaces (mount, pid, net, ipc, uts) for each worker.
- seccomp-bpf allowlist for syscalls.
- cgroup v2 limits (memory, cpu, pids).
- Read-only root filesystem with minimal tmpfs.
- Network egress via allowlist proxy.
- macOS: documented limitations (no namespaces/seccomp); rely on process containment + resource limits.

Adversarial tests required before claiming sandbox.
