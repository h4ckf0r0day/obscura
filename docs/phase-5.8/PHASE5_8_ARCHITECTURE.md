# Phase 5.8 Distributed Architecture & Fault Tolerance

## Status: VERIFIED & RESOLVED

### 1. Architectural Overview

```
                      +-----------------------------+
                      |       Scheduler Daemon      |
                      | (obscura-scheduler)         |
                      +--------------+--------------+
                                     |
               +---------------------+---------------------+
               | FOR UPDATE SKIP LOCKED                     | JetStream Dispatch
               v                                            v
     +-------------------+                        +-------------------+
     |    PostgreSQL     |                        |   NATS JetStream  |
     |   - jobs          |                        | - obscura.jobs.>  |
     |   - leases        |                        | - redelivery queue|
     |   - workers       |                        +---------+---------+
     +---------+---------+                                  |
               ^                                            | Pull Consumer
               | Lease Renewal & Status Heartbeats          v
      +--------+--------------------------------------------+--------+
      |                                                              |
+-----+-----------------------+                        +-------------+---------------+
|     Worker Agent A          |                        |        Worker Agent B       |
|  (obscura-worker-agent)     |                        |    (obscura-worker-agent)   |
|                             |                        |                             |
|  +-----------------------+  |                        |  +-----------------------+  |
|  | Supervisor Daemon     |  |                        |  | Supervisor Daemon     |  |
|  +-----------+-----------+  |                        |  +-----------+-----------+  |
|              | fork/exec    |                        |              | fork/exec    |
|              v              |                        |              v              |
|  +-----------------------+  |                        |  +-----------------------+  |
|  | Child Browser Worker  |  |                        |  | Child Browser Worker  |  |
|  | (PID A, PGID A)       |  |                        |  | (PID B, PGID B)       |  |
|  +-----------------------+  |                        |  +-----------------------+  |
+-----------------------------+                        +-----------------------------+
```

### 2. Core Components

#### A. Control Plane (`obscura-control-plane`)
- **Distributed Leases (`leases` table)**:
  - Tracks execution leases with `resource_type = 'job'`, `resource_id = job_id`, `owner_id = worker_id`, and `expires_at`.
  - Lease duration defaults to `10s` with worker heartbeats every `2s`.
- **Fencing Tokens**:
  - Each job has a monotonic `fencing_token BIGINT` incremented upon every state transition to `dispatched` or `running`.
  - Database-enforced atomic conditional updates:
    ```sql
    UPDATE jobs SET status = 'completed', ... WHERE job_id = $1 AND fencing_token = $2
    ```
  - Stale tokens return zero rows, resulting in `ControlPlaneError::Conflict("stale fencing token")`.
- **Abandoned Execution Recovery**:
  - `recover_abandoned_executions()` inspects jobs in `dispatched` or `running` state.
  - If the assigned worker is unavailable, or the distributed lease has expired, or unassigned > 10s:
    - Atomically resets job status to `pending`.
    - Clears worker assignment and increments attempt count.
    - Frees capacity on the dead worker.

#### B. Scheduler (`obscura-scheduler`)
- **Worker Capacity & Rebalance**:
  - Periodically reconciles workers whose heartbeats are older than threshold (default 10s), marking them `unavailable`.
  - Triggers `recover_abandoned_executions()` to reclaim orphaned jobs.
  - Reconciles worker capacity against active non-terminal jobs.
- **Fair Claiming**:
  - Uses `SELECT ... FOR UPDATE SKIP LOCKED` scoped per tenant to prevent starvation or race conditions among multi-scheduler instances.

#### C. Worker Agent (`obscura-worker-agent`)
- **Supervisor & Child Subtree**:
  - Spawns `obscura-runtime-worker` in a dedicated process group (`setpgid(0, 0)`).
  - Registers child worker PID with the control plane (`update_worker_child_pid`).
- **Lease Maintenance**:
  - Runs periodic lease heartbeat loop (`renew_lease`).
  - If renewal fails or lease expires:
    - Initiates emergency process group containment:
      `libc::kill(-(pid as i32), libc::SIGTERM)` and `libc::kill(-(pid as i32), libc::SIGKILL)`.
    - Marks job failed with error code `LEASE_LOST`.
