# Phase 6 Deployment Test Evidence

## Environment
- OS: macOS (Docker Desktop Linux VM)
- Docker: 29.8.1
- Compose: v2

## Image builds
- compose-control-plane:latest — built, exit 0
- compose-scheduler:latest — built, exit 0
- compose-worker-agent:latest — built, exit 0
- compose-obscura-supervisor:latest — built, exit 0

## Compose runtime
All services reached healthy:
- postgres (healthy)
- nats (healthy)
- control-plane (healthy)
- scheduler (healthy)
- worker-agent (healthy)
- obscura-supervisor (healthy)

## Real browser job
- Job ID: 8a30b4f8-2741-4943-96e0-2ff3887b232e
- Tenant ID: ba52d72e-5900-44d5-9027-ff6dd7b07ef8
- Worker ID: 4327fc4b-37ff-40fb-bc77-b8d1ab7f0ae9
- Status: completed
- Attempt count: 1
- Fencing token: 2
- URL: https://example.com

## Persistence verification
PostgreSQL query confirmed status=completed, attempt_count=1.

## Recovery
- Scheduler restart: healthy after restart.
- Worker-agent restart: healthy after restart.

## Root cause fixed
- Linux container worker readiness handshake failed because worker_main exited when getppid()==1 (container PID 1). Removed that early exit; PR_SET_PDEATHSIG remains.

## Phase 6.1.2 updates
- Control-plane integration tests: 18 passed; 0 failed (exit code 0) against Compose PostgreSQL + NATS.
- Scheduler readiness now checks PostgreSQL and NATS.
- Worker-agent readiness checks NATS connectivity after initialization.
- Compose services all healthy after rebuild.
- F5 adversarial suite not executed in this session (limitation).

## Phase 6 Final Gate (F5)
- F5 adversarial suite: ALL SCENARIOS PASSED (exit 0).
- Scenario 1 multi-process coexistence: PASS.
- Scenario 2 worker crash recovery: PASS.
  - Original worker: f7ad5834-be6b-4a9f-a7e0-c40a7d6f2f53
  - Replacement worker: b68af794-06c3-4af2-be5b-0e83361c60eb
  - Job: 8b6cf4dc-594d-407e-8cfd-6e7c0b164ddd
  - Original fencing token: 5
  - Replacement fencing token: 6
  - Stale completion: rejected ("stale fencing token")
- Scenario 3 lease-loss process termination: PASS.
- Scenario 4 NATS redelivery: PASS.
- Scenario 5 browser containment: PASS.
- Scenario 6 combined: PASS.
- Control-plane integration tests: 18 passed (exit 0).
