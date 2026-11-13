# AUDIT_06 — Process Supervision and Isolation

## 1. Supervisor

- `obscura-supervisor`: process pool, IPC via stdin/stdout JSON, kill_on_drop, process groups (`setpgid(0,0)`), Linux PDEATHSIG (`prctl(PR_SET_PDEATHSIG, SIGKILL)`), race check on ppid.
- Graceful shutdown: SIGTERM to group, wait, SIGKILL.
- Restart with bounded backoff and max_restarts.
- Process group termination targets `-(pid)`.

## 2. Isolation crate

- `obscura-isolation` (1 file) — likely OS sandboxing abstraction. Need inspect; probably minimal. (Not deeply audited.)

## 3. Gaps

- PDEATHSIG only on Linux; macOS relies on process groups + kill_on_drop (parent death may orphan until GC). 
- `obscura-isolation` not integrated into worker spawn (no seccomp/namespace). Actual isolation is process supervision, not OS sandboxing.
- Child stderr drainer prevents buffer lockups but logs at debug.
- stdout reader task fails all pending on EOF; okay.
- No zombie reaping beyond tokio Child::wait (called in kill).
- Worker binary resolution searches relative paths; production image must place binary correctly.

## 4. Claim vs reality

Process-tree containment is real (process groups), but "isolation" is supervision, not sandbox. No seccomp/AppArmor/namespace. Document as process supervision, not security isolation.
