#!/usr/bin/env bash
set -euo pipefail

# Check out the pinned web-platform-tests revision and install the Obscura overlay.
# The full WPT tree is large (several GB once the manifest is built). It is meant
# to live on the high-performance VPS, not a laptop.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

WPT_REPO="${WPT_REPO:-https://github.com/web-platform-tests/wpt.git}"
WPT_REF="${WPT_REF:-$(tr -d '[:space:]' <"$SCRIPT_DIR/wpt-config/WPT_COMMIT")}"
WPT_DIR="${WPT_DIR:-$SCRIPT_DIR/wpt}"

OVERLAY="$SCRIPT_DIR/wpt-overlay/resources/testharnessreport.js"
TARGET="$WPT_DIR/resources/testharnessreport.js"

echo "[setup-wpt] base dir: $SCRIPT_DIR"
echo "[setup-wpt] wpt dir:  $WPT_DIR"

if [ ! -e "$WPT_DIR" ]; then
  echo "[setup-wpt] cloning $WPT_REPO"
  git clone --depth 1 --filter=blob:none --no-checkout "$WPT_REPO" "$WPT_DIR"
elif ! git -C "$WPT_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "[setup-wpt] error: $WPT_DIR exists but is not a git checkout" >&2
  exit 1
fi

CURRENT_REF="$(git -C "$WPT_DIR" rev-parse HEAD 2>/dev/null || true)"
if [ "$CURRENT_REF" != "$WPT_REF" ]; then
  if [ -n "$(git -C "$WPT_DIR" status --porcelain --untracked-files=no)" ]; then
    echo "[setup-wpt] error: $WPT_DIR has tracked changes; clean them before changing revisions" >&2
    exit 1
  fi
  echo "[setup-wpt] fetching pinned WPT revision $WPT_REF"
  git -C "$WPT_DIR" fetch --depth 1 origin "$WPT_REF"
  git -C "$WPT_DIR" checkout --detach FETCH_HEAD
else
  echo "[setup-wpt] pinned WPT revision already checked out"
fi
if [ "$(git -C "$WPT_DIR" rev-parse HEAD)" != "$WPT_REF" ]; then
  echo "[setup-wpt] error: checked-out WPT revision does not match $WPT_REF" >&2
  exit 1
fi

if [ ! -f "$OVERLAY" ]; then
  echo "[setup-wpt] error: overlay not found at $OVERLAY" >&2
  exit 1
fi

# Back up the upstream report script once, then install our overlay.
if [ -f "$TARGET" ] && [ ! -f "$TARGET.orig" ]; then
  echo "[setup-wpt] backing up original report script to $TARGET.orig"
  cp "$TARGET" "$TARGET.orig"
fi

echo "[setup-wpt] installing overlay -> $TARGET"
cp "$OVERLAY" "$TARGET"

# Hosts file. The WPT server serves on web-platform.test and a set of subdomains.
# We do not edit /etc/hosts ourselves because that needs sudo. Tell the user what
# to run, unless it already resolves.
if grep -q "web-platform.test" /etc/hosts 2>/dev/null; then
  echo "[setup-wpt] web-platform.test already present in /etc/hosts, skipping hosts step"
else
  echo "[setup-wpt] web-platform.test is not in /etc/hosts yet."
  echo "[setup-wpt] run this once, from inside $WPT_DIR, to add the WPT hostnames:"
  echo ""
  echo "    ./wpt make-hosts-file | sudo tee -a /etc/hosts"
  echo ""
fi

echo "[setup-wpt] building the WPT manifest (this can take a while)"
( cd "$WPT_DIR" && ./wpt manifest )

echo ""
echo "[setup-wpt] done."
echo "[setup-wpt] next steps:"
echo "  1) if you have not added the hosts entries yet, run the make-hosts-file line above."
echo "  2) run a test pass with: $SCRIPT_DIR/scripts/run-wpt.sh"
