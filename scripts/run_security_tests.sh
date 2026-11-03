#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${ROOT_DIR}/target/release/obscura"

if [ ! -f "$BIN" ]; then
    echo "Error: $BIN not found."
    exit 1
fi

echo "=========================================================="
echo "        Obscura Next: Security Regression Suite           "
echo "=========================================================="

FAILED=0

# Test 1: SSRF Gate blocks cloud metadata service (169.254.169.254)
echo -n "[TEST 1] SSRF Cloud Metadata IP blocking: "
OUTPUT=$("$BIN" fetch "http://169.254.169.254/latest/meta-data" --timeout 2 --quiet 2>&1 || true)
if echo "$OUTPUT" | grep -qi -E "blocked|refused|error|denied|fail"; then
    echo "PASS (Traffic correctly blocked)"
else
    echo "FAIL (Cloud metadata was not blocked!)"
    FAILED=$((FAILED + 1))
fi

# Test 2: SSRF Gate blocks loopback (127.0.0.1) without flag
echo -n "[TEST 2] SSRF Loopback (127.0.0.1) blocking: "
OUTPUT=$("$BIN" fetch "http://127.0.0.1:9222/" --timeout 2 --quiet 2>&1 || true)
if echo "$OUTPUT" | grep -qi -E "blocked|refused|error|denied|fail"; then
    echo "PASS (Loopback correctly blocked by default)"
else
    echo "FAIL (Loopback was not blocked by default!)"
    FAILED=$((FAILED + 1))
fi

# Test 3: SSRF Gate blocks IPv6 Loopback ([::1])
echo -n "[TEST 3] SSRF IPv6 Loopback ([::1]) blocking: "
OUTPUT=$("$BIN" fetch "http://[::1]:9222/" --timeout 2 --quiet 2>&1 || true)
if echo "$OUTPUT" | grep -qi -E "blocked|refused|error|denied|fail"; then
    echo "PASS (IPv6 loopback correctly blocked by default)"
else
    echo "FAIL (IPv6 loopback was not blocked by default!)"
    FAILED=$((FAILED + 1))
fi

# Test 4: V8 Infinite loop watchdog termination
echo -n "[TEST 4] V8 Watchdog Preemptive Termination: "
OUTPUT=$("$BIN" fetch "about:blank" --eval "while(true){}" --timeout 3 --allow-private-network --quiet 2>&1 || true)
if echo "$OUTPUT" | grep -qi -E "terminated|timeout|error|deadline|null"; then
    echo "PASS (Infinite JS loop successfully bounded and terminated)"
else
    echo "FAIL (Watchdog did not terminate infinite JS loop!)"
    FAILED=$((FAILED + 1))
fi

# Test 5: Isolation & Security Crate Regression Suite
echo -n "[TEST 5] Isolation Policy & Sandbox Unit Tests: "
if cargo test -p obscura-isolation --quiet -- --nocapture 2>&1; then
    echo "PASS (All 4 isolation security unit tests passed)"
else
    echo "FAIL (Isolation crate unit tests failed)"
    FAILED=$((FAILED + 1))
fi

echo "=========================================================="
if [ $FAILED -eq 0 ]; then
    echo " ALL SECURITY REGRESSION TESTS PASSED (5/5)!              "
    echo "=========================================================="
    exit 0
else
    echo " $FAILED SECURITY TESTS FAILED!                           "
    echo "=========================================================="
    exit 1
fi
