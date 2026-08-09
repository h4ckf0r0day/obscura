#!/usr/bin/env bash
# Audit the boundary between Obscura's portable Rust core and its native-only
# V8/network runtime. Known native-only failures are reported but only become a
# failing gate with --require-full.

set -euo pipefail

TARGET="wasm32-unknown-unknown"
REQUIRE_FULL=0
PROBE_V8=1

usage() {
    sed -n '2,5p' "$0"
    echo "Usage: $0 [--target <rust-target>] [--require-full] [--skip-v8-probe]"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            [[ $# -ge 2 ]] || { echo "error: --target requires a value" >&2; exit 2; }
            TARGET="$2"
            shift 2
            ;;
        --require-full)
            REQUIRE_FULL=1
            shift
            ;;
        --skip-v8-probe)
            PROBE_V8=0
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUN_DIR="$(mktemp -d "${TMPDIR:-/tmp}/obscura-wasm-audit.XXXXXX")"
trap 'rm -rf "$RUN_DIR"' EXIT

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_TARGET_DIR="${OBSCURA_WASM_AUDIT_TARGET_DIR:-$REPO_ROOT/target/wasm-audit}"

failures=0

print_diagnostics() {
    local log="$1"
    if ! grep -m 24 -E \
        '(^error:|compile_error|unsupported by mio|wasm.*not supported|static lib URL:|HTTP Error|could not compile|failed to run custom build)' \
        "$log"; then
        tail -n 24 "$log"
    fi
}

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command '$1' was not found" >&2
        failures=$((failures + 1))
    fi
}

run_portable_probe() {
    local label="$1"
    shift
    local log="$RUN_DIR/${label//[^a-zA-Z0-9]/_}.log"

    echo "[portable] $label"
    if "$@" >"$log" 2>&1; then
        echo "  PASS"
    else
        echo "  FAIL: a crate in the intended portable core no longer compiles for $TARGET"
        print_diagnostics "$log" | sed 's/^/    /'
        failures=$((failures + 1))
    fi
}

classify_engine_failure() {
    local log="$1"

    if grep -q 'This wasm target is unsupported by mio' "$log"; then
        echo "  BLOCKED: workspace Tokio enables native networking through mio."
        echo "  Action: move sockets/reqwest behind a HostTransport and use target-specific Tokio features."
        return 0
    fi
    if grep -Eq '(librusty_v8_|static lib URL:).*wasm32' "$log" \
        && grep -Eq 'HTTP Error 404|404.*Not Found|not found.*librusty_v8_' "$log"; then
        echo "  BLOCKED: rusty_v8 has no archive for $TARGET."
        echo "  Action: keep JavaScript execution in host Node/Deno V8 and expose only the portable core through WASM."
        return 0
    fi
    if grep -q 'wasm.*unknown-unknown targets are not supported by default' "$log"; then
        echo "  BLOCKED: a dependency needs a JavaScript-backed random source."
        echo "  Action: enable its wasm/js backend only on wasm32, then repeat the audit to reveal the next blocker."
        return 0
    fi

    echo "  UNCLASSIFIED FAILURE: the known blocker moved or a regression appeared."
    print_diagnostics "$log" | sed 's/^/    /'
    failures=$((failures + 1))
    return 1
}

run_native_layer_probe() {
    local label="$1"
    shift
    local log="$RUN_DIR/${label//[^a-zA-Z0-9]/_}.log"

    echo "[native layer] $label"
    if "$@" >"$log" 2>&1; then
        echo "  PASS: the previous compile blocker has been removed"
    else
        if classify_engine_failure "$log"; then
            if [[ "$REQUIRE_FULL" -eq 1 ]]; then
                print_diagnostics "$log" | sed 's/^/    /'
                failures=$((failures + 1))
            fi
        elif [[ "$REQUIRE_FULL" -eq 1 ]]; then
            # An unclassified failure was already counted by the classifier.
            true
        fi
    fi
}

probe_rusty_v8_directly() {
    local metadata_log="$RUN_DIR/metadata.log"
    local manifest
    local log="$RUN_DIR/rusty_v8.log"

    echo "[V8] direct rusty_v8 target probe"
    if ! cargo metadata --locked --format-version 1 --filter-platform "$TARGET" >"$RUN_DIR/metadata.json" 2>"$metadata_log"; then
        echo "  FAIL: cargo metadata could not resolve the target graph"
        print_diagnostics "$metadata_log" | sed 's/^/    /'
        failures=$((failures + 1))
        return
    fi

    manifest="$(jq -r 'first(.packages[] | select(.name == "v8") | .manifest_path) // empty' "$RUN_DIR/metadata.json")"
    if [[ -z "$manifest" || "$manifest" == "null" || ! -f "$manifest" ]]; then
        echo "  FAIL: the resolved deno_core graph did not expose a v8 manifest"
        failures=$((failures + 1))
        return
    fi

    echo "  Resolved: $(jq -r 'first(.packages[] | select(.name == "v8") | "v8 " + .version) // "unknown"' "$RUN_DIR/metadata.json")"
    if cargo check --locked --manifest-path "$manifest" --target "$TARGET" >"$log" 2>&1; then
        echo "  PASS: rusty_v8 now supplies a working $TARGET build; reassess the host-V8 architecture"
        return
    fi

    if grep -Eq '(librusty_v8_|static lib URL:).*wasm32' "$log" \
        && grep -Eq 'HTTP Error 404|404.*Not Found|not found.*librusty_v8_' "$log"; then
        grep -m 4 -E 'static lib URL:|HTTP Error 404|Not Found' "$log" | sed 's/^/    /'
        echo "  BLOCKED: the current rusty_v8 release does not publish a WASM static archive."
        echo "  Its build.rs also has no wasm32 cross-compilation branch; V8_FROM_SOURCE is not a supported fallback."
        if [[ "$REQUIRE_FULL" -eq 1 ]]; then
            failures=$((failures + 1))
        fi
    else
        echo "  UNCLASSIFIED FAILURE: rusty_v8 failed before/after the expected archive check"
        print_diagnostics "$log" | sed 's/^/    /'
        failures=$((failures + 1))
    fi
}

cd "$REPO_ROOT"

require_command cargo
require_command rustup
if [[ "$PROBE_V8" -eq 1 ]]; then
    require_command jq
fi
if [[ "$failures" -ne 0 ]]; then
    exit 1
fi

if ! rustup target list --installed | grep -Fxq "$TARGET"; then
    echo "error: Rust target '$TARGET' is not installed" >&2
    echo "Install it with: rustup target add $TARGET" >&2
    exit 1
fi

echo "Obscura WASM target audit"
echo "  target: $TARGET"
echo "  rustc:  $(rustc --version)"
echo

echo "[dependency path]"
if cargo tree --locked --target "$TARGET" -p obscura-js -i v8 >"$RUN_DIR/v8-tree.log" 2>&1; then
    sed 's/^/  /' "$RUN_DIR/v8-tree.log"
else
    echo "  FAIL: could not resolve obscura-js -> deno_core -> v8"
    print_diagnostics "$RUN_DIR/v8-tree.log" | sed 's/^/    /'
    failures=$((failures + 1))
fi
echo

run_portable_probe "obscura-dom" cargo check --locked --target "$TARGET" -p obscura-dom
run_portable_probe "obscura-render-core" cargo check --locked --target "$TARGET" -p obscura-render
run_portable_probe "obscura-wasm" cargo check --locked --target "$TARGET" -p obscura-wasm
echo

run_native_layer_probe "obscura-net" cargo check --locked --target "$TARGET" -p obscura-net
run_native_layer_probe "obscura-render paint" cargo check --locked --target "$TARGET" -p obscura-render --features paint
run_native_layer_probe "obscura-js (deno_core + networking + V8)" cargo check --locked --target "$TARGET" -p obscura-js
echo

if [[ "$PROBE_V8" -eq 1 ]]; then
    probe_rusty_v8_directly
else
    echo "[V8] direct probe skipped by --skip-v8-probe"
fi
echo

if [[ "$failures" -ne 0 ]]; then
    echo "WASM audit failed with $failures failing gate(s)." >&2
    exit 1
fi

if [[ "$REQUIRE_FULL" -eq 1 ]]; then
    echo "All audited WASM layers passed."
else
    echo "Portable-core gate passed. Full-engine blockers above are expected and documented."
    echo "Use --require-full to make native-only engine blockers fail the command."
fi
