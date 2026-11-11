# CI Regression Gate

## Workflow

`.github/workflows/obscura-next-ci.yml` gains a `f5-adversarial` job on `ubuntu-22.04`
(x86_64). Steps:

1. Checkout.
2. Install Rust stable.
3. Build `obscura-runtime-worker`, `obscura-worker-agent`, and the adversarial
   example.
4. Start PostgreSQL and NATS JetStream services.
5. Run the adversarial example with real services.
6. Upload logs on failure.

The job fails if any mandatory scenario fails. No test is skipped on V8
download failure because the worker binaries are built in the same job.

## Note

Linux x86_64 runtime verification is provided by this CI job; the local
verification baseline was Linux aarch64 (Docker Desktop).