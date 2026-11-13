# Phase 5.10 Completion Integrity

complete_job validates status, attempt_id, fencing token with FOR UPDATE and SQL predicate. Duplicate completion idempotent for same attempt/token. Cross-tenant and stale rejected.