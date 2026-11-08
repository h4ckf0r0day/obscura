# Phase 5.3 Reproductions

## Reproduction 1: Process termination on lease loss

**Command:**
```bash
# Requires: PostgreSQL + NATS + 2 worker agents
# 1. Start PG + NATS
# 2. Start worker agent A
# 3. Start worker agent B
# 4. Submit job via control plane
# 5. Verify A receives and starts execution
# 6. Record A's child PID
# 7. kill -9 <A_PID>
# 8. Wait 35s (lease TTL)
# 9. Verify A's child is gone (ps check)
# 10. Verify B receives and completes
```

**Status:** NOT EXECUTED (Docker daemon unstable, no multi-process harness)

## Reproduction 2: NATS redelivery

**Command:**
```bash
# Requires: NATS running
# 1. Publish dispatch message
# 2. Start consumer
# 3. Receive without ack
# 4. Kill consumer
# 5. Restart consumer
# 6. Verify redelivery
```

**Status:** NOT EXECUTED (no test harness)

## Reproduction 3: Scheduler recovery

**Command:**
```bash
# Requires: PostgreSQL + NATS
# 1. Start scheduler
# 2. Submit job
# 3. Kill scheduler
# 4. Restart scheduler
# 5. Verify job recovered
```

**Status:** NOT EXECUTED (no test harness)
