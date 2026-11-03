# Threat Model & Security Boundaries

## 1. Threat Profile

| Threat Category | Attack Vector | Impact | Mitigation in Obscura Next |
|---|---|---|---|
| **SSRF** | Script/Navigation fetches `http://169.254.169.254/latest/meta-data` or `http://127.0.0.1:6379` | Cloud credential theft, internal service compromise | Default blocking of loopback, RFC1918, link-local, multicast, and IPv6 private addresses via `obscura-ssrf`. DNS rebinding validation prior to socket connection. |
| **Local File Access** | Script requests `file:///etc/passwd` or `file:///proc/self/environ` | System file read, secret leakage | `allow_file_access: false` by default. Navigation to `file:` scheme rejected at the `Page` boundary and within subresource loaders. |
| **V8 CPU Exhaustion** | Web page executes `while(true){}` or heavy regex backtracking | Worker thread freeze, denial of service | Preemptive watchdog thread invoking `IsolateHandle::terminate_execution()` on expiration. Absolute process wall-clock deadline. |
| **V8 Memory Exhaustion** | Web page generates huge arrays or retaining closures | Worker OOM killer crash | Near-heap limit callback tripping termination and adding headroom for clean unwinding. Worker recycling on RSS ceiling. |
| **DOM Engine Crash** | Deep recursive trees or malicious DOM mutation cycles | Process SIGSEGV or stack overflow | `catch_unwind` anti-panic boundary on all DOM operations. Cycle detection in `tree.rs`. Cap on traversal depth. |
| **Cross-Tenant Data Leak** | Subsequent requests reusing cookies or localStorage | Data leakage across AI agents | Isolated `BrowserContext` per session with clean in-memory `CookieJar` and `OriginStorage`. Explicit session teardown wipes state. |

---

## 2. SSRF Enforcement Layer (`obscura-ssrf`)

Obscura Next verifies all outbound addresses against a strict network perimeter:
- **IPv4 Denied**: `0.0.0.0/8`, `10.0.0.0/8`, `127.0.0.0/8`, `169.254.0.0/16`, `172.16.0.0/12`, `192.168.0.0/16`, `224.0.0.0/4`, `240.0.0.0/4`.
- **IPv6 Denied**: `::1/128`, `::/128`, `fc00::/7`, `fe80::/10`, `ff00::/8`.

Only when the operator explicitly passes `--allow-private-network` (or `OBSCURA_ALLOW_PRIVATE_NETWORK=1`) is traffic to private ranges permitted.

---

## 3. Sandboxing & Process Boundaries

In production deployments, workers run inside isolated Linux namespaces or containers:
- **PID Namespace**: Worker only sees its own process hierarchy.
- **Network Namespace**: Optional network policy restricting egress to approved proxy egress gateways.
- **Read-Only Root Filesystem**: Worker container filesystem mounted read-only with a temporary in-memory tmpfs for scratch operations.
- **Dropping Privileges**: Workers run as non-root user `obscura:obscura` (UID 10001).
