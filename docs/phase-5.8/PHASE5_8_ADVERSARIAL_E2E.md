# Phase 5.8 Adversarial End-to-End Verification

## Status: VERIFIED & RESOLVED

### 1. Test Architecture
The Phase 5.8 Adversarial Test Suite runs against real backend infrastructure:
- **Database**: PostgreSQL 16 on port 5432
- **Message Broker**: NATS JetStream 2.10 on port 4222
- **Supervisor & Workers**: Independent operating system processes launched via `tokio::process::Command` without in-memory mocks:
  - Supervisor daemon: `obscura-worker-agent`
  - Browser worker: `obscura-runtime-worker`
  - Scheduler & harness: `target/release/examples/adversarial_e2e`

### 2. Verified Adversarial Scenarios

#### Scenario 1: Multi-Process Worker Coexistence
- **Objective**: Verify that multiple independent worker-agent processes can register, poll, and execute jobs in parallel without race conditions or shared-state corruption.
- **Execution Flow**:
  1. Worker Agent A (PID 21074) and Worker Agent B (PID 21075) spawned as distinct background processes.
  2. Both agents initialized child browser workers (PIDs 21077 and 21076) and established distinct JetStream consumers.
  3. Job 1 and Job 2 submitted to control plane.
  4. Scheduler dispatched Job 1 to Agent A and Job 2 to Agent B.
  5. Both jobs completed concurrently with status `completed`.
- **Result**: **PASS**

#### Scenario 2: Worker-Agent Crash Recovery & Stale Worker Rejection
- **Objective**: Verify that if a worker agent process crashes mid-execution:
  1. Abandoned executions are detected and reclaimed to `pending`.
  2. The job is rescheduled to a healthy surviving worker with a monotonically higher fencing token.
  3. Any late or stale completion attempts from the crashed worker are strictly rejected by PostgreSQL DB-level fencing.
- **Execution Flow**:
  1. Job 3 dispatched to Worker Agent A with fencing token `198`.
  2. Worker Agent A killed abruptly with OS `SIGKILL`.
  3. `recover_abandoned_executions()` identified the worker as unavailable and lease expired, resetting Job 3 to `pending`.
  4. Scheduler rescheduled Job 3 to Worker Agent B with new fencing token `199`.
  5. Adversarial test simulated late completion from Worker A using token `198`:
     - Database responded with `Conflict("stale fencing token")`.
  6. Worker Agent B executed and completed Job 3 with token `199`.
- **Result**: **PASS**

#### Scenario 3: Process-Level Termination on Lease Loss (Kernel ESRCH)
- **Objective**: Verify that when a worker agent loses its execution lease (heartbeat timeout or database lease expiration), it immediately terminates its child browser worker process group at the OS kernel level, and transitions the job to `failed` with error code `LEASE_LOST`.
- **Execution Flow**:
  1. Worker Agent C spawned with child browser worker PID `21080`.
  2. Job 4 submitted with a mock slow HTTP navigation request.
  3. Worker C dispatched navigation to child worker PID `21080`.
  4. Test directly expired the lease row in PostgreSQL:
     ```sql
     UPDATE leases SET expires_at = NOW() - INTERVAL '10 seconds' WHERE resource_type = 'job' AND resource_id = $1
     ```
  5. Worker C failed lease renewal on its heartbeat ticker and invoked process group kill:
     ```rust
     libc::kill(-(pid as i32), libc::SIGKILL);
     ```
  6. Harness checked child PID via kernel syscall:
     ```rust
     let res = libc::kill(c_pid as i32, 0);
     let err = std::io::Error::last_os_error().raw_os_error();
     ```
     Returned: `res = -1`, `err = Some(ESRCH)` (No such process).
  7. Job 4 status verified as `failed` with recorded attempt error code `LEASE_LOST`.
- **Result**: **PASS**

#### Scenario 4: NATS Redelivery & Unacked Timeout Recovery
- **Objective**: Verify NATS JetStream consumer redelivery on explicit negative acknowledgment (`NAK`) and unacked timeout (`ack_wait`).
- **Execution Flow**:
  1. Created JetStream consumer with `ack_wait = 800ms`.
  2. Published test envelope.
  3. First consumer delivery: sent `AckKind::Nak(None)`.
  4. Message redelivered immediately upon NAK.
  5. Second delivery: allowed `1500ms` to elapse without sending ACK.
  6. JetStream timed out unacknowledged message and redelivered it a third time.
  7. Consumer acknowledged message successfully.
- **Result**: **PASS**

---

### 3. Harness Command & Logs
Run command:
```bash
./tests/integration/harness/run_adversarial_multiproc.sh
```
Output:
```
=======================================================
ALL 4 ADVERSARIAL MULTI-PROCESS SCENARIOS PASSED!
1. Multi-process coexistence: VERIFIED
2. Worker-agent crash recovery & takeover: VERIFIED
3. Process-level termination on lease loss (ESRCH): VERIFIED
4. NATS redelivery/recovery: VERIFIED
=======================================================
```
