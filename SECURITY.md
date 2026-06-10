# Security Policy

## Reporting a vulnerability

Please report security issues **privately** to the maintainers (e.g. via a
GitHub security advisory) rather than opening a public issue.

## Threat model

Obscura **fetches and executes untrusted web content by design** — treat every
scraped site as adversarial. The trust boundaries that matter:

- **Page JavaScript → engine ops** (the V8 sandbox and the Rust ops it can call).
- **The CDP and MCP servers** — these have **no authentication**; anyone who can
  reach the port controls the engine. Bind to loopback only.
- **Outbound network egress** — guarded against SSRF (private/loopback/link-local
  targets, including via DNS resolution).

## Operator hardening

- Do **not** expose the CDP/MCP port beyond `127.0.0.1` on untrusted networks
  (avoid `--host 0.0.0.0` outside a controlled setup).
- Leave `--allow-file-access` **off** (the default) unless serving local HTML on
  a trusted network.
- Do **not** set `OBSCURA_ALLOW_PRIVATE_NETWORK` / `--allow-private-network` in
  production; it disables the SSRF guard.
- A web page in the victim's browser cannot drive the CDP/MCP HTTP port
  cross-origin by default: a handshake/request carrying an `Origin` is rejected
  unless allow-listed, and the `Host` is pinned to loopback to blunt DNS
  rebinding. Allow specific browser origins via `OBSCURA_CDP_ALLOWED_ORIGINS`
  (CDP WebSocket) / `OBSCURA_MCP_ALLOWED_ORIGINS` (MCP HTTP).
- Restrict permissions on any `--storage-dir` — cookies are persisted in clear
  text.
- Run scraping of unknown targets in an isolated container/user with filtered
  egress.

## Supply chain

- The prebuilt **V8 static library** is downloaded at build time from the `v8`
  crate's GitHub releases over HTTPS and is **not** independently
  checksum-verified (see denoland/rusty_v8#545). For high-assurance builds, set
  `V8_FROM_SOURCE=1` or vendor a checksummed library.
- Verify the published checksums of release binaries before use.
- Dependency advisories are gated in CI via `cargo deny` (see `deny.toml`).
