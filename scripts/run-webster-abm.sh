#!/usr/bin/env bash
# Webster A/B/M experiment.
#
# Generates three pinakes against the webster corpus (44 first-parent commits,
# ~8k LOC, TypeScript browser extension):
#
#   webster-A.pinakes  — fresh ingest at HEAD → backward backfill from root
#   webster-B.pinakes  — forward walk: index at each of 44 commits chronologically
#   webster-M.pinakes  — middle-out: index at midpoint, forward to HEAD, backfill to root
#
# Wall-clock target: 1-3 hours with the new diff-based walker + Layer-2 cache.

set -euo pipefail

CALLI="${CALLI:-/Users/hammer/Code/callimachus/target/release/calli}"
WEBSTER="/Users/hammer/Code/webster"
WALK="/Users/hammer/Code/webster-walk"
DATA="/Users/hammer/Code/callimachus/data"

cd "$WEBSTER"
C_ROOT=$(git rev-list --max-parents=0 HEAD)
C_HEAD=$(git rev-parse HEAD)
# Middle commit (median by index)
ALL_COMMITS=( $(git rev-list --reverse --first-parent HEAD) )
N=${#ALL_COMMITS[@]}
MID_IDX=$((N / 2))
C_MID="${ALL_COMMITS[$MID_IDX]}"

# Make sure A/B/M pinakes don't exist from prior runs
rm -f "$DATA"/webster-A.pinakes* "$DATA"/webster-B.pinakes* "$DATA"/webster-M.pinakes*

echo "──────────────────────────────────────────────────────────────"
echo "Webster A/B/M experiment"
echo "  repo:     $WEBSTER"
echo "  commits:  $N first-parent"
echo "  root:     ${C_ROOT:0:8}"
echo "  mid:      ${C_MID:0:8}  (index $MID_IDX of $((N-1)))"
echo "  HEAD:     ${C_HEAD:0:8}"
echo "  binary:   $CALLI"
echo "──────────────────────────────────────────────────────────────"

# Make sure we're at HEAD before kicking off A and M
git -C "$WEBSTER" checkout main --quiet 2>/dev/null || git -C "$WEBSTER" checkout master --quiet 2>/dev/null || true
git -C "$WEBSTER" reset --hard --quiet "$C_HEAD"

# ── A: backward backfill from HEAD to root ─────────────────────────────────
echo
echo "═══ A: ingest at HEAD → history backfill --from <root> ═══"
A_START=$(date +%s)
"$CALLI" --pinakes "$DATA/webster-A.pinakes" corpus add code "webster" "$WEBSTER" > /dev/null
"$CALLI" --pinakes "$DATA/webster-A.pinakes" index webster --pass all 2>&1 | tail -12
echo "  --- A head index done at $(date -u), $(($(date +%s) - A_START))s ---"
"$CALLI" --pinakes "$DATA/webster-A.pinakes" history backfill webster --from "$C_ROOT" --passes default,theme 2>&1 | tail -3
echo "  --- A COMPLETE at $(date -u), $(($(date +%s) - A_START))s total ---"

# ── B: forward walk in a worktree ──────────────────────────────────────────
echo
echo "═══ B: forward walk over $N commits in $WALK ═══"
if [ -d "$WALK" ]; then
    git -C "$WEBSTER" worktree remove "$WALK" --force 2>/dev/null || true
fi
git -C "$WEBSTER" worktree add "$WALK" "$C_ROOT" 2>&1 | tail -2
B_START=$(date +%s)
"$CALLI" --pinakes "$DATA/webster-B.pinakes" corpus add code "webster" "$WALK" > /dev/null
i=0
for sha in "${ALL_COMMITS[@]}"; do
    i=$((i+1))
    echo "  [B $i/$N] → ${sha:0:8}"
    git -C "$WALK" reset --hard --quiet "$sha"
    "$CALLI" --pinakes "$DATA/webster-B.pinakes" index webster --pass all 2>&1 | grep -E "^Done|✓ pass=" | tail -2 || true
done
echo "  --- B COMPLETE at $(date -u), $(($(date +%s) - B_START))s total ---"

# ── M: middle-out (ingest at C_MID, forward to HEAD, backfill to root) ─────
echo
echo "═══ M: ingest at midpoint ${C_MID:0:8} → forward to HEAD → backfill to root ═══"
git -C "$WEBSTER" reset --hard --quiet "$C_MID"
M_START=$(date +%s)
"$CALLI" --pinakes "$DATA/webster-M.pinakes" corpus add code "webster" "$WEBSTER" > /dev/null
"$CALLI" --pinakes "$DATA/webster-M.pinakes" index webster --pass all 2>&1 | tail -12
echo "  --- M midpoint index done at $(date -u), $(($(date +%s) - M_START))s ---"

# Forward walk from MID+1 to HEAD
for ((i=MID_IDX+1; i<N; i++)); do
    sha="${ALL_COMMITS[$i]}"
    echo "  [M-fwd $i/$((N-1))] → ${sha:0:8}"
    git -C "$WEBSTER" reset --hard --quiet "$sha"
    "$CALLI" --pinakes "$DATA/webster-M.pinakes" index webster --pass all 2>&1 | grep -E "^Done|✓ pass=" | tail -2 || true
done
echo "  --- M forward to HEAD done at $(date -u), $(($(date +%s) - M_START))s ---"

# Then backfill back to root
"$CALLI" --pinakes "$DATA/webster-M.pinakes" history backfill webster --from "$C_ROOT" --passes default,theme 2>&1 | tail -3
echo "  --- M COMPLETE at $(date -u), $(($(date +%s) - M_START))s total ---"

# Restore HEAD for cleanliness
git -C "$WEBSTER" reset --hard --quiet "$C_HEAD"

echo
echo "──────────────────────────────────────────────────────────────"
echo "All three webster pinakes built at $DATA/"
echo "Inspect with scripts/compare-pinakes.py"
echo "──────────────────────────────────────────────────────────────"
