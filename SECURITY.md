# Security Policy

The Obscura project takes security seriously. Obscura runs real, untrusted
JavaScript from arbitrary web pages through V8, so we appreciate your efforts to
responsibly disclose what you find and will work with you to address it.

## Reporting a vulnerability

**Please do not report security issues through public GitHub issues, pull
requests, or discussions.**

Report a vulnerability privately through GitHub: go to the repository's
**Security** tab and click [**Report a vulnerability**](https://github.com/h4ckf0r0day/obscura/security/advisories/new)
to open a private advisory.

If you cannot use GitHub advisories, email **hello@obscura.sh** with "security"
in the subject line.

### What to include

To help us triage quickly, please provide as much of the following as you can:

- Type of issue (SSRF, hang or crash / denial of service, memory safety,
  cross-session data exposure, etc.).
- The obscura version or commit, plus OS and architecture.
- The location of the affected code (file path and commit, or a direct link).
- Any special configuration or flags required to reproduce.
- Step-by-step instructions: a page, `--eval` snippet, or CDP sequence that
  triggers it.
- Proof-of-concept code if you have it, and the impact you think it has.

## Our response

We will send a response indicating the next steps in handling your report,
normally within 3 business days. We will keep you informed of progress toward a
fix, and may ask for additional information. We practice coordinated disclosure:
please give us a reasonable window to ship a fix before any public writeup, and
we will credit you in the advisory unless you ask us not to.

If you do not receive an acknowledgement within 6 business days, please follow
up by email to make sure we received the report.

## Scope

Obscura is a powerful tool for browser automation, scraping, and inspection. It
is the responsibility of the calling code and the operator to use it safely. The
security boundaries Obscura is meant to hold, and which are in scope:

- **Egress / SSRF control.** Page content reaching loopback, RFC 1918, or
  link-local addresses without `--allow-private-network` being set.
- **Availability.** A page, script, or DOM structure that defeats the V8
  termination watchdog, the CLI hard deadline, or the panic guards and so hangs
  or aborts the process.
- **Memory safety** in Obscura's own `unsafe` Rust or in an op that bridges JS
  to Rust.
- **Process and data integrity.** A page that escapes the intended op surface,
  corrupts another page's state, or reads data across origins or sessions it
  should not (cookies, storage, response bodies).
- **TLS / identity correctness** issues that weaken or misrepresent a connection
  in a way the user did not request.

The following are **not** vulnerabilities. We welcome feedback on them as
feature requests, but they will not be treated as security issues:

- **Stealth / anti-fingerprinting behavior.** Presenting a normal, consistent
  browser fingerprint is the intended, privacy-first design of stealth mode.
  Requests to add detection-evasion for abusive purposes are out of scope.
- **Anything behind an explicit opt-in,** such as reaching a private address
  when `--allow-private-network` (or `OBSCURA_ALLOW_PRIVATE_NETWORK=1`) is set,
  or behavior that requires local access to the machine running Obscura.
- **Resource use from a page you chose to load** that stays within the watchdog
  and deadline limits. Slow pages are not a vulnerability.
- **Findings against the companion benchmark repo fixtures** rather than the
  engine itself.

## Third-party dependencies

Report vulnerabilities in third-party crates to their respective maintainers (or
the [RustSec advisory database](https://rustsec.org/)). The workspace is gated
by `cargo deny` via `deny.toml`. If a dependency advisory affects Obscura's own
behavior, let us know so we can pin or patch.

## Security model and operator responsibilities

Obscura executes untrusted page JavaScript **in process** through V8. Its
in-process hardening reduces blast radius but is not a substitute for operating
system isolation:

- The **V8 termination watchdog** terminates the isolate from a separate thread
  when synchronous script work overruns, because `tokio` timeouts only cancel at
  await points.
- The **CLI process-level hard deadline** is an absolute backstop for a hang
  inside a Rust op that neither `tokio` nor `terminate_execution` can interrupt.
- **Panic safety:** ops are wrapped so a panic degrades to a null result instead
  of aborting the process inside V8's FFI frame; `panic = "unwind"` is pinned in
  the release profile.
- The **default network egress policy** blocks private and loopback ranges.

These measures protect availability and limit egress. They do **not** claim to
contain a hostile page that achieves native code execution through a V8 exploit.
If you run Obscura against untrusted or adversarial input at scale, run it under
OS-level isolation (a container or VM) with a restricted network, the same way
you would run headless Chrome. Per-user container isolation is planned for the
hosted service so that one session cannot affect another.

## Supported versions

Security fixes land on `main` and ship in the next release. Please test reports
against the latest release or `main`.
