# Security Advisory (DRAFT — for private coordinated disclosure)

> **Do not publish this publicly.** Submit it privately to the maintainer via the
> repo's *Security → Report a vulnerability* (GitHub Security Advisory) or the
> contact in `SECURITY.md`. Fixes are ready on a branch (see §Fixes) and can be
> shared / merged under embargo, then released before public disclosure.

- **Project:** Obscura — headless browser engine (`h4ckf0r0day/obscura`)
- **Affected:** 0.1.0 and current `main` (all listed issues reproduce on the audited tree)
- **Reporter:** _<your name / handle>_
- **Date:** 2026-06-10
- **Overall severity:** Critical (multiple). Obscura executes hostile web content by design; several issues are reachable from **plain page JavaScript on any scraped site, with default flags**.

---

## TL;DR

A security review found multiple vulnerabilities, several Critical, in the engine's network egress, JS↔Rust op boundary, `file://` handling, CDP/MCP transports, cookie jar, and HTML parser. The highest-impact ones are reachable by **any malicious page Obscura is pointed at**, with no special flags:

1. **SSRF** to internal services / cloud metadata (DNS rebinding + incomplete IP denylist).
2. **Full privileged-op access from page JS** (`Deno.core.ops` left on the page global) → cross-origin authenticated reads, HttpOnly-cookie exfiltration, cookie poisoning.
3. **Arbitrary local file read** via `file://` on the MCP and CDP navigation paths (default flags).
4. **CDP/MCP ports drivable cross-origin** by a web page in the operator's browser (no Origin/Host validation).
5. **Cross-site cookie injection**, **header injection**, and **DoS** (process abort / host OOM) from a single hostile page.

All are fixed on the referenced branch (tests + CI, incl. the `--features stealth` path).

---

## Vulnerabilities

### 1. SSRF — DNS rebinding and incomplete IP denylist (Critical)
- **Attacker:** A1 (malicious page), default flags. **CVSS:3.1 ~9.1 (AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:L/A:N).**
- **Detail:** The egress guard inspected only the URL-string host *before* DNS resolution, with no custom resolver pinning the connection. A hostname that resolves to an internal address bypassed it entirely. The literal denylist also missed `0.0.0.0/8`, IPv4-mapped IPv6 (`::ffff:127.0.0.1`), IPv6 ULA (`fc00::/7`), CGNAT (`100.64/10`), NAT64, multicast and unspecified. Affected the navigation client, the page-JS `fetch()`/XHR op (`op_fetch_url`), the dynamic ES-module loader, and — with no guard at all — the `--stealth` client.
- **Impact:** A scraped page reads cloud-metadata credentials (`169.254.169.254`), internal admin panels, and loopback services (e.g. Redis), and exfiltrates them.
- **Repro:** Point Obscura at a page that runs
  `fetch('http://<rebind-host>/latest/meta-data/iam/security-credentials/')` where `<rebind-host>` has a short-TTL record flipping to `169.254.169.254`; or directly `fetch('http://[::ffff:127.0.0.1]:6379/')` / `fetch('http://0.0.0.0:<port>/')`.
- **Fix:** Canonical `is_forbidden_ip` + a resolve-time DNS resolver that rejects any forbidden resolved address and pins the connection (closes rebinding), installed on all four clients; redirect hops re-validated.

### 2. Privileged ops reachable from page JS (Critical)
- **Attacker:** A1, default flags. **CVSS ~9.3 (S:C/C:H/I:H/A:L).**
- **Detail:** The bootstrap environment ran in the same V8 realm that exposes `Deno.core.ops`, and never removed it, so any page `<script>` could call every registered op directly — bypassing all JS-layer guards (CORS, origin derivation, cookie semantics).
- **Impact (chained):** (a) credentialed cross-origin reads incl. responses gated by **HttpOnly** cookies (`op_fetch_url` with a spoofed/empty `origin`); (b) **cookie-jar exfiltration**; (c) **cookie poisoning** (`op_set_cookie`).
- **Repro:** A page runs `typeof Deno.core.ops.op_fetch_url` → `"function"`; then `Deno.core.ops.op_fetch_url('https://victim/api','GET','{}','','','no-cors')` returns the cookie-authenticated body.
- **Fix:** Wrap bootstrap in an IIFE (internals no longer leak via the shared global lexical scope), bridge through a private op table captured at runtime, and narrow page-reachable `Deno.core.ops` to the two non-sensitive ops the CDP layer needs.

### 3. Arbitrary local file read via `file://` (Critical / High)
- **Attacker:** A2 (CDP/MCP client, or a local web page reaching the port), **default flags**.
- **Detail:** `--allow-file-access` (off by default) was enforced only in the CDP `do_navigate` handler and not on the **MCP** tools nor on the **CDP `Page.navigate` interception** path — which is the path actually taken after a normal Puppeteer/Playwright attach. The net layer reads any `file://` path unconditionally.
- **Impact:** `browser_navigate {url:"file:///etc/passwd"}` + `browser_snapshot` (MCP), or `Page.navigate` + `Runtime.evaluate` (CDP), return arbitrary file contents.
- **Fix:** Gate `file://` on the MCP tools and the CDP interception entry; add `obscura mcp --allow-file-access`.

### 4. CDP/MCP transports drivable cross-origin (High)
- **Attacker:** A1/A2. **CVSS ~8.x.**
- **Detail:** The CDP WebSocket handshake and the MCP HTTP transport performed no Origin/Host validation and emitted wildcard CORS. A web page the operator visits could `new WebSocket('ws://127.0.0.1:9222/devtools/browser')` / `fetch('http://127.0.0.1:3000/mcp', …)` and drive the engine (→ file read per #3, cookie jar, arbitrary in-engine JS). DNS rebinding defeats Host assumptions.
- **Fix:** Reject non-allowlisted Origins (`OBSCURA_CDP_ALLOWED_ORIGINS` / `OBSCURA_MCP_ALLOWED_ORIGINS`), pin Host to loopback, drop `Access-Control-Allow-Origin: *`.

### 5. Cross-site cookie injection via `Domain=` (High)
- **Attacker:** A1. **Detail:** A page (or hostile response) could set `Domain=` to an unrelated parent/sibling/TLD with no host-relationship or public-suffix check → session fixation / jar poisoning across sites visited in the same run. **Fix:** validate `Domain=` against the request host + reject public suffixes.

### 6. Denial of service — process abort / host OOM (High)
- **Attacker:** A1. **Detail:** Unbounded recursion in the HTML serializer / `textContent` / innerHTML import overflowed the native stack (abort, uncatchable by the op-layer `catch_unwind`); the node arena and response bodies were unbounded (host OOM). A single page (`'<div>'.repeat(200000)`, or a huge/endless response) crashes the engine. **Fix:** depth-cap / iterative DOM walks, `MAX_NODES` arena cap, response-body size cap, MCP `Content-Length` cap.

### 7. Forbidden-header injection from page JS (High)
- **Attacker:** A1. **Detail:** `fetch()`/XHR forwarded page-controlled `Host`/`Cookie`/`Referer`/`Origin`/`Sec-*` headers verbatim → Host-header SSRF / vhost confusion, cookie override, referrer/origin spoofing. **Fix:** drop forbidden request headers.

> **Lower-severity** issues also fixed: proxy credentials logged in clear, a UTF-8 byte-slice panic in CDP log paths, release artifacts without checksums, and CI not enforcing `Cargo.lock`. A few residuals remain (SameSite egress enforcement, `file://` path jail, an accessibility-tree quadratic walk) and are documented in the fix branch.

---

## Fixes

A complete set of fixes (tested; CI green, including a new `--features stealth` job built with libclang) is staged as themed commits:

- Branch: `ci/audit-gate-and-hygiene` (12 commits) — happy to open a PR / share a patch bundle under embargo.
- Coverage: SSRF resolver + denylist (all four clients), op-realm isolation, `file://` gates (MCP+CDP), Origin/Host validation, cookie `Domain=` validation, DoS caps, header filter, log/credential hygiene, CI hardening.

## Suggested handling

1. Treat as embargoed; assign GHSA / request CVEs as appropriate.
2. Review/merge the fixes, cut a patched release.
3. Operator advisory: until patched, keep CDP/MCP on loopback only, never set `--allow-file-access` / `--allow-private-network`, and avoid scraping untrusted targets outside an isolated, egress-filtered container.

## Credit

Reported by _<your name / handle>_. Fixes contributed on the branch above.
