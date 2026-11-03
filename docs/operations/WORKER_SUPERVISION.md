# Worker Supervision

`obscura-supervisor` launches real `obscura-runtime-worker` child processes via Tokio.

## Lifecycle

CREATED -> STARTING -> HEALTHY -> BUSY -> HEALTHY -> STOPPING -> STOPPED
Failure: STARTING/HEALTHY/BUSY -> FAILED -> RESTARTING -> STARTING

## Configuration

`SupervisorConfig`: worker_bin, pool_size, heartbeat_interval, heartbeat_timeout, recycle_policy, restart_backoff_base, max_restarts.

## Restart policy

Bounded exponential backoff (`restart_backoff_base * attempt`) up to `max_restarts` per worker. Failed workers are detected via stdout EOF and pending requests receive `WorkerTerminated`.

## Pool routing

- New sessions: least-loaded ready worker.
- Existing sessions: affinity by session_to_worker map.
- Stateless commands: round-robin.

## Shutdown

Sends `shutdown` command, waits grace period, then kills.
