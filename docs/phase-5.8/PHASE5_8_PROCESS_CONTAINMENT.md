# Phase 5.8 Process Containment & Kernel-Level Termination

## Status: VERIFIED & RESOLVED

### 1. Process Group Containment Architecture

Every browser worker spawned by the supervisor runs in a dedicated UNIX process group:
```
[Worker Agent Daemon] (PID P_agent)
       |
       +--- fork() / exec()
              |
              +--- [Browser Runtime Worker] (PID P_child, PGID P_child)
                     |
                     +--- [Renderer / Child Process] (PGID P_child)
```

By assigning the child worker its own process group (`setpgid(0, 0)`), any signals sent to negative PID `-(P_child)` are dispatched to all descendant processes in that group, preventing orphan or zombie processes.

### 2. Termination Sequence on Lease Loss

1. **Heartbeat Loop**: The worker agent spawns an asynchronous heartbeat task maintaining an active lease record in PostgreSQL (`leases` table).
2. **Loss Detection**: If database renewal fails (e.g. lease expired due to network partition or scheduler reclamation):
   ```rust
   let renew_res = cp.renew_lease(&worker_principal, &lease.lease_id, lease_ttl).await;
   if renew_res.is_err() {
       // Lease lost
   }
   ```
3. **Kernel Signaling**: The worker agent immediately signals the process group:
   ```rust
   unsafe {
       libc::kill(-(pid as i32), libc::SIGTERM);
       tokio::time::sleep(Duration::from_millis(100)).await;
       libc::kill(-(pid as i32), libc::SIGKILL);
   }
   ```
4. **Child Handle Cleanup**: Invokes `worker_clone.kill().await` to reap the process handle and release OS file descriptors.
5. **State Transition**: The job is failed in the control plane with error code `LEASE_LOST`.

### 3. Verification Evidence

In Scenario 3 of `adversarial_e2e`:
- Worker Agent C was executing Job 4 with child browser PID `21080`.
- The lease was forcefully expired in PostgreSQL.
- The worker agent terminated the child process group.
- The test harness verified process termination using POSIX syscall `kill(pid, 0)`:
  ```rust
  let res = libc::kill(c_pid as i32, 0);
  let err = std::io::Error::last_os_error().raw_os_error();
  assert_eq!(res, -1);
  assert_eq!(err, Some(libc::ESRCH)); // No such process
  ```
- Result: Kernel verified child process `21080` does not exist (`ESRCH`), and job `job_attempts.error_code` recorded as `LEASE_LOST`.
