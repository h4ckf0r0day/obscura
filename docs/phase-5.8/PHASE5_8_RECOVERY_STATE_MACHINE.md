# Phase 5.8 Recovery State Machine & Fencing

## Status: VERIFIED & RESOLVED

### 1. Job Lifecycle State Machine

```
              +---------------+
              |    pending    |<-----------------------+
              +-------+-------+                        |
                      |                                |
                      | claim & dispatch (token + 1)   | Abandoned Execution
                      v                                | (Worker crash or lease expired)
              +---------------+                        |
              |   dispatched  |------------------------+
              +-------+-------+                        |
                      |                                |
                      | execution started (token + 1)  |
                      v                                |
              +---------------+                        |
              |    running    |------------------------+
              +-------+-------+
                      |
        +-------------+-------------+
        |                           |
        v                           v
+---------------+           +---------------+
|   completed   |           |    failed     |
+---------------+           |  (LEASE_LOST, |
                            |   CRASH, etc.)|
                            +---------------+
```

### 2. Recovery Transitions

#### A. Abandoned Execution Detection
The control plane query evaluates:
```sql
SELECT j.job_id, j.status, j.tenant_id, j.worker_id, j.fencing_token, j.execution_attempt_id, j.attempts
FROM jobs j
LEFT JOIN leases l ON l.resource_type = 'job' AND l.resource_id = j.job_id::text
LEFT JOIN workers w ON w.worker_id = j.worker_id
WHERE j.status IN ('dispatched', 'running')
  AND (
      w.status = 'unavailable'
      OR (l.expires_at IS NOT NULL AND l.expires_at < NOW())
      OR (j.worker_id IS NULL AND j.updated_at < NOW() - INTERVAL '10 seconds')
  )
FOR UPDATE OF j SKIP LOCKED
```

#### B. Reset to Pending
When an abandoned job is detected:
1. Job status reset to `pending`.
2. Worker assignment cleared (`worker_id = NULL`).
3. Attempt count incremented (`attempts = attempts + 1`).
4. Dead worker capacity decremented (`active_jobs = GREATEST(0, active_jobs - 1)`).

#### C. Takeover & Monotonic Fencing Token Progression
1. The scheduler picks up the pending job.
2. Selects an alternative healthy worker.
3. Claims the job, bumping the `fencing_token` monotonically:
   ```
   token_takeover > token_original
   ```
4. If the crashed or split-brain worker subsequently attempts completion:
   ```sql
   UPDATE jobs SET status = 'completed' WHERE job_id = $1 AND fencing_token = $2
   ```
   Since `token_original < token_takeover`, the condition matches 0 rows and returns `Conflict("stale fencing token")`.
5. Takeover worker completes execution with `token_takeover`.
