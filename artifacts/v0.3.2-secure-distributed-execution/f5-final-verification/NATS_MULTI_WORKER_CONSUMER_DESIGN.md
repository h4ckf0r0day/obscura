# NATS Multi-Worker Consumer Design

## Delivery Model
- Shared JetStream stream (OBScura_JOBS) with subjects obscura.jobs.>.
- Each worker agent creates an independent durable consumer with a unique name (worker-agent-{worker_id}).
- Each consumer receives all dispatch messages.
- Worker agent filters by dispatch.worker_id == its own worker_id.
- Non-assigned workers ack and skip.
- This preserves at-least-once delivery and prevents duplicate execution (only the assigned worker executes).

## Why Not Shared Consumer
- A shared durable consumer would deliver each message to one consumer, but worker assignment is determined by the scheduler (least-loaded). The scheduler already selects the worker and puts worker_id in the dispatch message. A shared consumer would require the selected worker to be the one that receives the message, which NATS doesn't guarantee with round-robin (it delivers to any available consumer). Independent consumers + filtering is simpler and correct.

## Consumer Name
- Configurable via NATS_CONSUMER_NAME env var.
- Default: worker-agent-{worker_id}.
