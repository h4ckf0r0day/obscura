# Security Findings

| ID | Sev | Component | Finding | Status |
|---|---|---|---|---|
| SEC-01 | HIGH | isolation | No OS sandbox; in-process guard only | CONFIRMED |
| SEC-02 | MEDIUM | telemetry | Metrics auth opt-in (default open) | CONFIRMED |
| SEC-03 | MEDIUM | net | validate_url file:// redirect inconsistency | PARTIALLY CONFIRMED |
| SEC-04 | LOW | compose | Weak token removed; requires env | MITIGATED |
| SEC-05 | LOW | control-plane | Bootstrap secret fails closed | MITIGATED |
| SEC-06 | MEDIUM | browser | SSRF DNS rebinding needs audit | UNVERIFIED |

No hardcoded weak credentials remain in compose.
