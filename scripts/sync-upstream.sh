#!/usr/bin/env bash
#
# Gated upstream sync for the asap-cool/obscura fork.
#
# Fetches upstream/main, merges it into the local integration branch, builds, and
# runs the hard gate. It NEVER pushes: on green, review and push/re-pin yourself.
# See FORK.md for the carry set and the pinning policy.
#
# Env:
#   UPSTREAM_REMOTE  upstream git remote name        (default: upstream)
#   MAIN_BRANCH      fork integration branch         (default: main)
#   OBSCURA_BENCH    path to the obscura-benchmark repo for obstacle-course
#                    (default: $HOME/Workspace/obscura-benchmark; skipped if absent)
set -euo pipefail

UPSTREAM_REMOTE="${UPSTREAM_REMOTE:-upstream}"
MAIN_BRANCH="${MAIN_BRANCH:-main}"
OBSCURA_BENCH="${OBSCURA_BENCH:-$HOME/Workspace/obscura-benchmark}"

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"
bin="$repo_root/target/release/obscura"

say()  { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
fail() { printf '\n\033[31mSYNC FAILED: %s\033[0m\n' "$*" >&2; exit 1; }

# 0. Preconditions.
git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1 \
  || fail "remote '$UPSTREAM_REMOTE' not set (git remote add $UPSTREAM_REMOTE https://github.com/h4ckf0r0day/obscura.git)"
[ -z "$(git status --porcelain)" ] || fail "working tree is dirty; commit or stash first"

# 1. Fetch upstream and show what is incoming.
say "Fetching $UPSTREAM_REMOTE"
git fetch "$UPSTREAM_REMOTE" --quiet
git checkout --quiet "$MAIN_BRANCH"

incoming="$(git log --oneline "${MAIN_BRANCH}..${UPSTREAM_REMOTE}/main")"
if [ -z "$incoming" ]; then
  say "Already up to date with ${UPSTREAM_REMOTE}/main. Nothing to sync."
  exit 0
fi
say "Incoming from ${UPSTREAM_REMOTE}/main:"
echo "$incoming"

# 2. Merge upstream. Abort cleanly on conflict so the tree is left untouched.
say "Merging ${UPSTREAM_REMOTE}/main into ${MAIN_BRANCH}"
if ! git merge --no-edit "${UPSTREAM_REMOTE}/main"; then
  git merge --abort
  fail "merge conflict with upstream. Resolve manually, then re-run the gate:
      git merge ${UPSTREAM_REMOTE}/main    # resolve conflicts (likely the carry in FORK.md)
      scripts/sync-upstream.sh             # or run the gate steps below by hand"
fi

# 3. Build.
say "Building release"
cargo build --release --bin obscura || fail "release build broke after the merge"

# 4. Hard gate: concurrency + full obscura-cdp suite (the fork's load-bearing carry).
say "Gate 1/2: cargo nextest run -p obscura-cdp"
cargo nextest run -p obscura-cdp || fail "obscura-cdp tests red after sync (concurrency regression?)"

# 5. Hard gate: obstacle-course must stay fully green.
if [ -f "$OBSCURA_BENCH/obstacle-course/run.py" ]; then
  say "Gate 2/2: obstacle-course ($OBSCURA_BENCH)"
  oc_out="$(cd "$OBSCURA_BENCH" && OBSCURA_BIN="$bin" python3 obstacle-course/run.py --runs 1 --warmup 0 2>&1)"
  echo "$oc_out" | grep -iE "correctness:|stages passed" || true
  score="$(echo "$oc_out" | grep -oE '[0-9]+/[0-9]+ stages passed' | head -1)"
  [ -n "$score" ] || fail "could not read obstacle-course score"
  passed="${score%%/*}"; total="${score#*/}"; total="${total%% *}"
  [ "$passed" = "$total" ] || fail "obstacle-course regressed: $score"
  say "obstacle-course: $score"
else
  printf '\n\033[33mWARN: obstacle-course skipped (set OBSCURA_BENCH to the obscura-benchmark repo).\033[0m\n'
fi

# 6. Green. Human does the outward steps.
head_sha="$(git rev-parse --short HEAD)"
cat <<EOF

$(printf '\033[32mSYNC OK\033[0m') — ${MAIN_BRANCH} is now at ${head_sha}, gate green.

Not pushed. To publish and re-pin:
  1. Manual check: the live Google Maps feed still paginates to its full result set.
  2. git push origin ${MAIN_BRANCH}          # triggers the GHCR image build
  3. Re-pin ASAP to the new digest: ghcr.io/asap-cool/obscura@sha256:<digest>
EOF
