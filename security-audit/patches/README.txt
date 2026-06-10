Obscura — security remediation patch series (PRIVATE / under embargo)
====================================================================

This directory contains the security fixes referenced in the accompanying
Security Advisory (obscura-security-advisory-DRAFT.md). Do not distribute
publicly until the maintainer has cut a patched release.

Contents
--------
  0001..0011-*.patch        11 themed commits (git format-patch series)
  obscura-audit-fixes.bundle  same commits as a git bundle (alternative)

Base commit
-----------
  These apply on top of upstream commit 24c95d6
  ("ci: add PR gate (test/clippy/deny/semgrep) + supply-chain hygiene").
  If your main has moved, a 3-way apply (git am -3) resolves cleanly in
  almost all cases since the changes are localized.

Apply (patch series)
--------------------
  git checkout -b security-fixes 24c95d6     # or your main
  git am -3 /path/to/0001-*.patch /path/to/0002-*.patch ... 0011-*.patch
  # or, all at once from this directory:
  git am -3 *.patch

Apply (git bundle, alternative)
-------------------------------
  git fetch /path/to/obscura-audit-fixes.bundle \
      ci/audit-gate-and-hygiene:security-fixes
  git checkout security-fixes

What's in it (high level)
-------------------------
  - SSRF: canonical is_forbidden_ip + resolve-time DNS guard (closes DNS
    rebinding) on the nav client, op_fetch_url, the module loader AND the
    stealth (wreq) client; redirect hops re-validated; response-body size cap.
  - Page-JS op isolation: Deno.core.ops removed from the page realm.
  - file:// gates on the MCP tools and the CDP Page.navigate interception path;
    new `obscura mcp --allow-file-access`.
  - CDP/MCP transport: Origin allow-list + Host pinning, Content-Length cap,
    no more wildcard CORS.
  - Cookie Domain= scope validation; forbidden-header filter on fetch().
  - DoS caps (DOM recursion/arena), proxy-credential redaction, UTF-8-safe logs.
  - CI: enforce Cargo.lock, publish release checksums, and a Linux job that
    builds+tests the --features stealth (wreq/BoringSSL) path.

Validation
----------
  All targeted suites pass; CI green on all 5 jobs including the new stealth
  job (which builds wreq with libclang and runs the stealth SSRF tests).

Known residuals (documented, not in this series)
------------------------------------------------
  SameSite egress enforcement; file:// path jail; an accessibility-tree
  quadratic ancestor walk; a streaming (vs Content-Length) body cap for wreq.
