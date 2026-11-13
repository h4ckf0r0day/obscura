# AUDIT_14 — Dependencies and Supply Chain

## 1. Tooling availability

- `cargo-deny`: NOT installed.
- `cargo-audit`: NOT installed.
- Advisory scan could not be executed. Report advisory status from deny.toml ignore list only.

## 2. Manifest observations

- Workspace uses vendored taffy (path), cosmic-text, wreq-util (excluded).
- Dependencies: tokio full, reqwest (rustls), deno_core (V8), sqlx, async-nats, taffy, tiny-skia, ab_glyph.
- deny.toml ignores 4 unmaintained advisories (fxhash, paste, rustybuzz, ttf-parser) — accepted.
- Duplicate versions warned (bans.multiple-versions = warn).

## 3. Risks

- Heavy native deps (V8 via deno_core) — build complexity.
- Pinned cosmic-text/vendored taffy may lag upstream security fixes.
- No reproducible build verification performed (Docker builds but not tested here).

## 4. Recommendation

- Install cargo-deny/cargo-audit in CI and run; add to PR policy.
- Pin base image digest for supply-chain reproducibility (trade-off noted in Dockerfile).
