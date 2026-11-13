# AUDIT VALIDATION 07 — PROCESS ISOLATION

## Mechanisms present
- Supervisor spawns child in new process group (`setpgid(0,0)`).
- `kill_on_drop(true)` on child.
- Linux only: `prctl(PR_SET_PDEATHSIG, SIGKILL)` with parent-death race check.
- On lease loss, worker-agent kills process group via `libc::kill(-pid, SIGTERM/SIGKILL)` then `worker.kill()`.
- Job-scoped process registry (`job_handles`) tracks per-job supervised workers.

## Mechanisms absent
- No seccomp, namespaces, cgroups, rlimits, chroot, or sandbox.
- No filesystem/network isolation.
- PDEATHSIG only on Linux; macOS lacks parent-death signal.
- No resource limits (memory/CPU) enforced by OS.

## Classification
1. Process supervision: YES.
2. Process isolation (separate process): YES (child process).
3. Resource isolation: NO (no rlimits/cgroups).
4. Filesystem isolation: NO.
5. Network isolation: NO (SSRF policy is application-level).
6. Security sandboxing: NO.
7. Process-tree containment: PARTIAL (process group + PDEATHSIG Linux only; lease-loss kills group).

## SIGKILL to agent
- If agent receives SIGKILL, heartbeat stops, lease expires, scheduler recovers job; browser child process may survive on macOS (no PDEATHSIG), and process group may be orphaned until reaped. On Linux, PDEATHSIG kills child. Job recovery still occurs via lease expiry.

## Status: CONFIRMED (F-07). Isolation is supervision, not OS sandbox. Document and/or implement real sandbox.
