#!/usr/bin/env bash
# Forward full walk: index callimachus at each first-parent commit from root to HEAD,
# writing into data/callimachus-B.pinakes. Each iteration triggers an incremental
# re-index against the worktree's current tree.
#
# Usage:
#   scripts/forward-walk.sh
#
# Assumes a git worktree exists at ../callimachus-walk, currently checked out at the
# root commit, and that the B pinakes is fresh (ingest will happen at the root).

set -euo pipefail

WORKTREE="$(cd "$(dirname "$0")/../../callimachus-walk" && pwd)"
PINAKES="$(cd "$(dirname "$0")/.." && pwd)/data/callimachus-B.pinakes"
ORIGIN="$(cd "$(dirname "$0")/.." && pwd)"
LOG="/tmp/calli-B-forward-walk.log"

cd "$ORIGIN"

# First-parent SHAs from root → HEAD (oldest first)
SHAS=()
while IFS= read -r sha; do
    SHAS+=("$sha")
done < <(git rev-list --reverse --first-parent HEAD)
TOTAL=${#SHAS[@]}
echo "[forward-walk] $TOTAL commits, worktree=$WORKTREE pinakes=$PINAKES" | tee -a "$LOG"

# Initial ingest at the root commit (worktree is already there).
if ! calli --pinakes "$PINAKES" corpus list 2>/dev/null | grep -q '^callimachus '; then
    echo "[forward-walk] ingest at ${SHAS[0]:0:8}" | tee -a "$LOG"
    calli --pinakes "$PINAKES" corpus add code "Callimachus" "$WORKTREE" 2>&1 | tee -a "$LOG"
fi

i=0
for sha in "${SHAS[@]}"; do
    i=$((i+1))
    echo "[forward-walk] $i/$TOTAL → ${sha:0:8}" | tee -a "$LOG"
    (cd "$WORKTREE" && git checkout --quiet "$sha")
    calli --pinakes "$PINAKES" index callimachus --pass all 2>&1 | tail -15 | tee -a "$LOG"
done

echo "[forward-walk] done." | tee -a "$LOG"
