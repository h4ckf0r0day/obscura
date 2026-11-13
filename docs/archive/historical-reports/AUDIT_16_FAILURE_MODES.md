# AUDIT_16 — Failure Modes and Recovery

| Failure | Detection | Recovery | Tested? |
|---|---|---|---|
| Postgres down | connection errors | none automatic; jobs stuck | No (not in CI) |
| NATS down | publish/consume errors | retry loops | Partial |
| Scheduler crash | none | manual restart; pending jobs remain | No |
| Worker-agent crash | heartbeat expiry (10s) | recover_abandoned_executions reclaims | Partial |
| Browser child crash | stdout EOF → pending fail | job fails, retryable | Partial |
| Browser descendant survives parent | process group kill | group kill targets descendants | Unit tests only |
| Network interruption | reqwest errors | job fails, retryable | No |
| DNS failure | resolver error | job fails | No |
| Lease renewal failure | renew_lease err → lease loss | kills child, fails job | Code path exists |
| Lease expiry | reconcile reclaims | job returns pending | Yes (unit) |
| Duplicate dispatch | claim SKIP LOCKED | second claim should return None | Flaky in parallel |
| Duplicate completion | fencing token check | stale rejected | Yes (unit) |
| Outbox publisher crash | none | unpublished rows remain; republish on restart | Partial |
| Consumer restart | durable consumer | redelivery (max_deliver 5) | Test doesn't compile (nak) |
| Scheduler restart | none | reconcile() on start drains pending | Partial |
| DB restart | connection pool | retries needed | No |
| Partial deployment | N/A | risk of mixed versions | No |
| Graceful shutdown | SIGTERM | cleanup | Unit (supervisor) |
| Forced shutdown | kill | kill_on_drop | Partial |
| Disk exhaustion | OS errors | none | No |
| Memory exhaustion | none | none | No |
| CPU saturation | none | none | No |
| Malformed message | parse error | ack + skip | Yes (worker-agent) |
| Poison message | max_deliver 5 then? | no DLQ observed | No |

## Key observations

- Many failure paths rely on manual operator intervention.
- No DLQ for poison messages (NATS max_deliver 5 then message dropped? Actually stops delivery after max_deliver; message remains in stream unacked).
- Recovery tested only in unit/integration with services; not in CI default.
- Fencing enforced at complete/fail but not at all state writes (e.g., cancel_job?).
