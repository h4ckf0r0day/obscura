# Phase 5.12 Linux F5 Evidence

## Exact command

```bash
docker run --rm \
  -v "/path/obscura-next:/src" \
  -v "/path/obscura-next-target-linux:/src/target" \
  -w /src \
  -e DATABASE_URL="postgres://postgres:obscura@host.docker.internal:5432/obscura" \
  -e NATS_URL="nats://host.docker.internal:4222" \
  -e OBSCURA_SERVICE_BOOTSTRAP_SECRET="phase5-11-integration-secret" \
  -e OBSCURA_ALLOW_PRIVATE_NETWORK="true" \
  rust:latest bash -c "/src/target/release/examples/adversarial_e2e"
```

## Result

All adversarial multi-process scenarios passed on Linux:
- Scenario 1: multi-process coexistence — PASS
- Scenario 2: worker-agent crash recovery and takeover — PASS
- Scenario 3: process-level termination on lease loss — PASS
- Scenario 5: browser process containment on agent SIGKILL — PASS
- Scenario 6: end-to-end F5 combined — PASS
- Scenario 4: NATS JetStream redelivery and recovery — PASS

## Evidence (from /tmp/linux_final.log)

- Worker Agent A: `277ef76b-16c2-4005-9470-83014b9b875c`
- Worker Agent B: `9e2b6ce8-2750-4cf8-b4d4-c6c9c1cf2f3e`
- Job 3 original fencing token: `5`
- Job 3 replacement fencing token: `6` (strictly newer)
- Stale completion: rejected (`Conflict("stale fencing token")`)
- Worker Agent C child PID: `42` (terminated on lease loss)
- Scenario 6 combined: PASS

## Regression

- `cargo test -p obscura-supervisor`: 12 passed
- `cargo test -p obscura-scheduler` / `obscura-control-plane`: pass on macOS;
  Linux runtime verified via adversarial suite.