# Obscura Next: Master Architecture & Platform Specification

## 1. Executive Summary

**Obscura Next** is the high-performance, security-hardened headless browser engine and distributed runtime platform specifically engineered for autonomous AI agents, web intelligence, and large-scale web automation.

Building upon the native Rust/V8 architecture of upstream Obscura, Obscura Next elevates the project into an enterprise-grade, distributed browser infrastructure platform organized across three core tracks:

```
+----------------------------------------------------------------------------------+
|                                  OBSCURA NEXT                                    |
+----------------------------------------------------------------------------------+
|  TRACK 1: Native Engine       |  TRACK 2: Isolated Infrastructure | TRACK 3: AI Agent  |
|  - Rust/V8 headless engine    |  - Process-level Worker Isolation | - Action Protocol  |
|  - Custom DOM & CSS cascade   |  - Supervisor & Pool Lifecycle    | - Semantic Tree    |
|  - Anti-panic DOM ops         |  - Prometheus & OpenTelemetry     | - LLM Distillation |
|  - Stealth TLS & fingerprint  |  - Distributed Scheduler & IPC    | - Multimodal State |
+----------------------------------------------------------------------------------+
```

---

## 2. Three Architecture Tracks

### Track 1: Native Rust/V8 Browser Engine
- **V8 / `deno_core` Runtime**: Executes unmodified modern ECMAScript within a controlled isolate, supporting ES modules, WebAssembly, WebCrypto, and IndexedDB.
- **Custom DOM Tree**: Zero-copy, high-performance HTML5 tree with CSS selectors, custom element reactions, and declarative shadow DOM support.
- **Paint & Layout Engine**: Pure-Rust layout (`taffy`), text shaping (`cosmic-text` / `ab_glyph`), and CPU rasterization (`tiny-skia`) enabling pixel-accurate screenshots and PDF rendering without Chromium dependencies.
- **Stealth & Anti-Detection**: Built-in TLS fingerprint impersonation (`wreq` BoringSSL transport), tracker blocklists, and browser identity alignment.

### Track 2: Isolated & Distributed Browser Infrastructure
- **Independent Runtime Worker (`obscura-worker`)**: Dedicated, sandboxed worker processes executing isolated browser instances with deterministic IPC.
- **Supervision & Self-Healing (`obscura-supervisor`)**: Erlang/OTP-inspired supervisor trees managing worker pools with health checks, crash backoff, and memory recycling.
- **Unified Telemetry (`obscura-telemetry`)**: Native Prometheus metrics registry and OpenTelemetry distributed tracing across IPC and network hops.
- **Resource Containment (`obscura-isolation`)**: Strict execution limits, memory bounds, watchdog timers, and kernel/process sandbox enforcement.

### Track 3: Autonomous AI-Agent Browser Platform
- **Agent Action Protocol (`obscura-agent`)**: High-level semantic actions (`navigate`, `click`, `type`, `extract`, `wait_for_idle`, `screenshot`).
- **Semantic DOM Distiller**: Distills complex web pages into token-optimized Markdown/JSON structures designed for LLM context windows (reducing prompt tokens by up to 90%).
- **Interactive Element Indexing**: Generates stable, resilient selector coordinates for AI action execution.

---

## 3. Milestone Roadmap

| Milestone | Target | Focus Area | Status |
|-----------|--------|------------|--------|
| **v0.1.0** | Current | **Isolated Runtime Foundation** (Worker isolation, session lifecycle, supervisor, Prometheus, OpenTelemetry, security tests) | **ACTIVE** |
| **v0.2.0** | Next | **Distributed Cluster & Scheduling** (Multi-node orchestration, UDS/TCP IPC, distributed leasing, dynamic autoscaling) | Planned |
| **v0.3.0** | Follow-up | **Agent Autonomous Workflows** (Self-healing selectors, multi-modal page perception, session replay) | Planned |

---

## 4. Repository Layout

```text
obscura-next/
├── crates/                     # Rust Workspace Crates
│   ├── obscura/                # High-level embeddable API
│   ├── obscura-dom/            # DOM tree, parser, selector matcher
│   ├── obscura-ssrf/           # SSRF protection and IP classification
│   ├── obscura-net/            # HTTP client, cookies, stealth client
│   ├── obscura-browser/        # Page lifecycle, browser context
│   ├── obscura-cdp/            # Chrome DevTools Protocol implementation
│   ├── obscura-js/             # V8 isolate management, DOM ops, watchdog
│   ├── obscura-mcp/            # Model Context Protocol server
│   ├── obscura-render/         # CSSOM, layout, rasterization, screenshot
│   ├── obscura-cli/            # Main CLI & entry points
│   ├── obscura-isolation/     # Runtime isolation, sandbox policies, quotas
│   ├── obscura-worker/        # Dedicated runtime worker process & IPC
│   ├── obscura-supervisor/    # Worker pool supervisor & self-healing
│   ├── obscura-telemetry/     # Prometheus & OpenTelemetry instrumentation
│   └── obscura-agent/         # AI-agent action protocol & DOM distillation
├── docs/                       # Architectural & user documentation
├── engineering/                # Engineering specifications, threat models, ADRs
├── benchmarks/                 # Obstacle course & performance benchmarks
├── dashboard/                  # Observability & cluster control plane UI
├── sdk/                        # Client SDKs (Python, TypeScript)
├── deploy/                     # Docker, Kubernetes, and Compose manifests
├── scripts/                    # Bootstrap, validation, and operational scripts
└── .github/                    # CI/CD workflows and security automation
```
