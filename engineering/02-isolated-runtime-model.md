# Isolated Runtime & Session Lifecycle Model

## 1. Threat Model & Isolation Principles

Headless browsers executing untrusted JavaScript from external websites represent a severe attack surface:
1. **Memory Exhaustion (DoS)**: Malicious scripts allocating unbounded arrays or retaining DOM objects.
2. **CPU Starvation**: Infinite synchronous loops (`while(true){}`) locking threads.
3. **SSRF & Network Pivoting**: Directing the browser engine to fetch loopback or internal metadata services (`169.254.169.254`, `localhost`, RFC1918).
4. **Local File Disclosure**: Navigating to `file:///etc/passwd` or reading local profile storage.
5. **Cross-Session Contamination**: Shared cookie jars, localStorage, or V8 isolate state leaking across distinct tenant workflows.

To address these threats, Obscura Next enforces a multi-tier defense:

```
[Host / Supervisor Process]
      │
      ├── (Fork / Exec)
      ▼
[Isolated Worker Process] (Process Boundary, UID/GID sandboxing)
      │
      ├── [BrowserContext] (Isolated CookieJar, LocalStorage, Proxy config)
      │         │
      │         └── [SessionManager] (Lease expiration, resource accounting)
      │                   │
      │                   └── [Page / V8 Isolate]
      │                             ├── Watchdog Thread (preemptive termination)
      │                             ├── Near-Heap Limit Guard (+64MB recovery)
      │                             ├── DOM Op anti-panic boundary (catch_unwind)
      │                             └── Network SSRF Gate (IPv4/IPv6 private block)
```

---

## 2. Session Lifecycle State Machine

Each session follows a deterministic lifecycle:

```
           ┌──────────────┐
           │  ALLOCATED   │
           └──────┬───────┘
                  │ initialize()
                  ▼
           ┌──────────────┐
           │ INITIALIZING │
           └──────┬───────┘
                  │ ready
                  ▼
        ┌──────────────────┐
   ┌───►│      READY       │◄───┐
   │    └─────────┬────────┘    │
   │              │ acquire()   │ release()
   │              ▼             │
   │    ┌──────────────────┐    │
   │    │      BUSY        ├────┘
   │    └─────────┬────────┘
   │              │ suspend()
   │              ▼
   │    ┌──────────────────┐
   └────┤    SUSPENDED     │
        └─────────┬────────┘
                  │ terminate()
                  ▼
        ┌──────────────────┐
        │     DRAINING     │
        └─────────┬────────┘
                  │ cleanup()
                  ▼
        ┌──────────────────┐
        │    TERMINATED    │
        └──────────────────┘
```

### State Definitions
- **ALLOCATED**: Session record created, worker slot assigned, configuration validated.
- **INITIALIZING**: Browser context created, storage directories initialized, V8 isolate initialized from startup snapshot.
- **READY**: Session idle, waiting for agent or CDP commands.
- **BUSY**: Actively executing navigation, JS evaluation, DOM mutation, or render pipeline.
- **SUSPENDED**: Inactive session swapped or hibernated to conserve memory.
- **DRAINING**: Session completing in-flight requests, rejecting new commands, preparing for teardown.
- **TERMINATED**: Isolate exited, memory freed, storage flushed or wiped, worker released.

---

## 3. Preemptive Watchdog & Resource Isolation

### Watchdog Mechanism
Synchronous JavaScript execution can monopolize the CPU. Tokio timeouts are async cancellation points and cannot interrupt a tight loop running inside a C++ V8 FFI call.
Obscura Next utilizes a dedicated watchdog thread that holds an `IsolateHandle`. If the execution budget expires:
1. The watchdog invokes `IsolateHandle::terminate_execution()`.
2. V8 safely unrolls execution and throws a non-catchable termination exception.
3. The Rust caller intercepts this error, returns a structured timeout response, and calls `cancel_termination()` to restore isolate sanity.

### Near-Heap Limit Guard
To prevent sudden process abortion on V8 heap exhaustion:
1. `add_near_heap_limit_callback` registers a callback when heap memory approaches the quota.
2. The callback triggers `terminate_execution()` and temporarily extends headroom by 64 MB.
3. This allows V8 to unwind cleanly, return an error to Rust, and avoid a process crash.
