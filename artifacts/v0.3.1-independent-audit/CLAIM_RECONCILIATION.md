# Claim Reconciliation — v0.3.1 Independent Audit

| Original Claim | Reported Implementation | Independent Evidence | Actual Assessment | Verdict |
|---|---|---|---|---|
| Monotonic fencing tokens | fencing_token_seq generates tokens | Token generated in mark_dispatched, validated in complete_job/fail_job | Tokens exist and validated | PARTIALLY CONFIRMED |
| Transactional result validation | complete_job validates token in same tx | Code inspection confirms | Verified at DB level | CONFIRMED |
| Authentication | Principal struct exists | Never called by any operation | Not enforced | NOT CONFIRMED |
| Authorization | authorize_tenant method exists | Never called | Not enforced | NOT CONFIRMED |
| Tenant isolation | tenant_id filtering in queries | Query-level only, no auth | Partial | PARTIALLY CONFIRMED |
| NATS deduplication | Nats-Msg-Id header set | Header present in publish | Broker-level dedup configured | CONFIRMED |
| Consumer lifecycle | NatsConsumer exists | Creates new stream per next_message() | Inefficient, needs fix | PARTIALLY CONFIRMED |
| Scheduler reconciliation | reconcile() exists | Only schedules pending, no stuck-job recovery | Incomplete | PARTIALLY CONFIRMED |
| Worker crash recovery | Not claimed | No crash-injection tests | Not tested | BLOCKED |
| Capacity accounting | active_jobs incremented/decremented | Cancel now decrements; no reconciliation on restart | Can drift | PARTIALLY CONFIRMED |
| Heartbeat staleness | Filter in list_healthy_workers | Filter exists but agent doesn't check expiry | Partial | PARTIALLY CONFIRMED |
| SSRF protection | Literal-IP guard in submit_job | Only checks IP literals, no DNS/redirect | Incomplete | PARTIALLY CONFIRMED |
| Metrics registration | Scheduler/outbox metrics registered | Registration done; not all updated at runtime | Partial | PARTIALLY CONFIRMED |
| Distributed tracing | TraceContext parsing exists | No exporter, no span creation | Not implemented | NOT CONFIRMED |
| E2E browser execution | Worker agent implemented | E2E test passed (job completed) | Verified (single-host) | CONFIRMED |
| Multi-machine readiness | Not claimed | Single-host only | Not tested | BLOCKED |
