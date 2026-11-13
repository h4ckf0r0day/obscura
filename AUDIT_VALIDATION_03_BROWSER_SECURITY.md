# AUDIT VALIDATION 03 — BROWSER NETWORK SECURITY (F-08)

## Scope
Trace of network request lifecycle: URL parsing, scheme validation, DNS, redirects, request construction, connection, subresources, local file access, protocol dispatch.

## Source
- `crates/obscura-net/src/client.rs`: `validate_url`, `fetch_file_url`, `fetch_request_uncached`, redirect loop
- `crates/obscura-net/src/wreq_client.rs`: validate_url, redirect loop
- `crates/obscura-browser/src/page.rs`: `navigate_with_wait_post_inner`, `navigate_single`, `cross_scheme_to_file`, `subresource_allowed`
- `crates/obscura-isolation/src/lib.rs`: `IsolationPolicy::validate_url`

## validate_url behavior
- Allows schemes: http, https, file. Rejects others.
- If scheme == file OR allow_private_network → returns Ok (no IP checks).
- For http/https with private network blocked: checks literal IPs, localhost domains, DNS not re-checked here (resolver does).

## Redirect handling
Both reqwest and wreq paths call `validate_url(&next_url, ...)` on each redirect Location before following. So scheme/IP validation occurs per hop.

## file:// redirect analysis
- validate_url permits file:// scheme always.
- However, redirect loop does not invoke `fetch_file_url` on a file:// next_url; it continues the loop and attempts an HTTP request to a file:// URL, which fails at transport (reqwest/wreq don't support file). Navigation fails; no local file body returned.
- Initial navigation to file:// is gated by `context.allow_file_access` in `navigate_with_wait_post_inner`.
- Subresource `<script src="file://...">` blocked by `subresource_allowed` (cross-scheme gate).
- JS-initiated navigation to file:// gated by `cross_scheme_to_file`.

## Conclusion
F-08 "file:// redirect may allow local file read" is PARTIALLY CONFIRMED as a defense-in-depth gap: validate_url's blanket allowance of file:// during redirects is unsafe in principle and relies on transport failure + higher-level gates. No demonstrated local file read via redirect in current paths, but the validation boundary is inconsistent (file:// allowed without opt-in). Recommend: validate_url should reject file:// unless allow_file_access is true, and redirect loops should treat file:// redirect as blocked/terminal.

## Attack-path summary
1. Remote HTTP 302 Location: file:///etc/passwd → validate_url Ok → reqwest send to file:// fails → navigation error. Not a read.
2. If allow_file_access enabled for context, redirect to file:// still fails at transport (no fetch_file_url in redirect path). Not a read.
3. Subresource injection of file:// blocked.

## Status: PARTIALLY CONFIRMED (gap in validation consistency; no proven exploit path). Acceptance test: redirect-to-file must be rejected by validate_url.
