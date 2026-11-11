# Startup Handshake Audit

## Previous behavior

Supervisor spawned the child and slept 100 ms before sending the handshake ping.
This was timing-based and not a reliable synchronization primitive.

## New behavior

1. Child installs `PR_SET_PDEATHSIG(SIGKILL)` (Linux, single-threaded startup).
2. Child validates parent identity (`getppid() != 1`).
3. Child writes `{"ready":true}` to stdout and flushes.
4. Supervisor reads one line with a 5 s timeout.
5. If the line contains `ready`, supervisor proceeds to the handshake ping.
6. On timeout or missing readiness, supervisor kills the child and fails spawn.

## Race handling

- Parent-death race: child checks `getppid() == 1` after installing the signal.
- PID reuse: readiness is an explicit message, not a PID check.
- Timeout: bounded 5 s; failure fails spawn and prevents untracked processes.