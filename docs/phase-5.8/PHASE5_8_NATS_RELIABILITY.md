# Phase 5.8 NATS JetStream Reliability & Redelivery

## Status: VERIFIED & RESOLVED

### 1. Overview
NATS JetStream provides persistent, ordered, and fault-tolerant message streaming for job dispatches between the scheduler and worker agents. Phase 5.8 ensures that message losses, transient network hiccups, or unacknowledged execution failures trigger automatic redelivery.

### 2. Stream & Consumer Architecture

- **Stream Definition**:
  - Name: `OBSCURA_JOBS`
  - Subjects: `obscura.jobs.>`
  - Storage: File or Memory backed
- **Consumer Configuration**:
  - Worker agents and scheduler consumers use explicit durable or ephemeral JetStream pull consumers.
  - Implemented `NatsConsumer::create_with_ack_wait(js, stream, durable_name, ack_wait)`.
  - Configures `ack_wait` (acknowledgment deadline) so that unacknowledged messages are automatically requeued.

### 3. Redelivery Scenarios Verified

#### A. Explicit Negative Acknowledgment (`NAK`)
When a worker receives a dispatch but encounters an immediate transient initialization error (e.g., temporary child process pool saturation), it issues:
```rust
msg.ack_with(async_nats::jetstream::AckKind::Nak(None)).await;
```
JetStream immediately makes the message available for redelivery to another consumer or retry.

#### B. Unacknowledged Timeout Redelivery (`ack_wait`)
If a worker crashes or hangs prior to acknowledging the message, JetStream waits for `ack_wait` to expire. Upon expiration, the message is automatically redelivered.
- Verified in Scenario 4:
  - Consumer configured with `ack_wait = 800ms`.
  - Message received and intentionally left unacknowledged for `1500ms`.
  - JetStream timed out the unacknowledged delivery and redelivered the message.
  - Worker acknowledged the redelivered message successfully.

#### C. Duplicate Dispatch Idempotency
- Worker agents inspect incoming dispatch envelopes for `worker_id`.
- If an agent receives a message targeted at a different worker (e.g. shared subject wildcard), it acknowledges the message and safely drops processing without side effects.
- If a duplicate message for an already active or completed job is received, database fencing rejects stale status mutations.
