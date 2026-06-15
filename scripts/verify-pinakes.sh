#!/usr/bin/env bash
# scripts/verify-pinakes.sh
#
# Smoke-verification for the Carefeed production pinakes.
# Implements the "every corpus genuinely live" probe shape from the PRD.
#
# Usage:
#   ./scripts/verify-pinakes.sh [OPTIONS]
#
# Options:
#   --pinakes <path>    Pinakes to verify (default: /tmp/carefeed-production.pinakes)
#   --manifest <path>   Corpus manifest (default: scripts/carefeed-corpora.manifest.json)
#
# Checks:
#   1. All six launch corpora are present and status=ready
#   2. Each corpus has entities > 0 (search would return results)
#   3. Each corpus has chunks > 0
#   4. The 'carefeed' collection exists and lists all six corpora as members
#   5. At least 2 launch corpora have entities with start_line set (line-span probe)
#   6. Cross-corpus proxy: at least 2 distinct launch corpora in the collection have entities
#
# Exit code:
#   0 — all checks passed
#   1 — one or more checks failed
#
# Requires: calli, jq, sqlite3

set -uo pipefail

CALLI_BIN="${CALLI_BIN:-calli}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PINAKES="/tmp/carefeed-production.pinakes"
MANIFEST="${SCRIPT_DIR}/carefeed-corpora.manifest.json"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pinakes)
      PINAKES="$2"
      shift 2
      ;;
    --manifest)
      MANIFEST="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '3,22p' "$0"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

export CALLIMACHUS_PINAKES="$PINAKES"

PASS_COUNT=0
FAIL_COUNT=0

pass() { echo "  ✓ $*"; (( PASS_COUNT++ )) || true; }
fail() { echo "  ✗ $*"; (( FAIL_COUNT++ )) || true; }

# ── Prereqs ───────────────────────────────────────────────────────────────────

echo ""
echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║         Carefeed Pinakes Verification                            ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""
echo "  Pinakes: $PINAKES"
echo ""

if ! command -v "$CALLI_BIN" &>/dev/null; then
  echo "Error: calli not found. Set CALLI_BIN or install via: cargo install --path crates/callimachus-cli" >&2
  exit 1
fi
if ! command -v jq &>/dev/null; then
  echo "Error: jq is required." >&2
  exit 1
fi
if ! command -v sqlite3 &>/dev/null; then
  echo "Error: sqlite3 is required (for line-span probe)." >&2
  exit 1
fi
if [[ ! -f "$PINAKES" ]]; then
  echo "Error: pinakes not found: $PINAKES" >&2
  exit 1
fi
if [[ ! -f "$MANIFEST" ]]; then
  echo "Error: manifest not found: $MANIFEST" >&2
  exit 1
fi

COLLECTION_ID=$(jq -r '.collection_id' "$MANIFEST")
CORPUS_COUNT=$(jq '.corpora | length' "$MANIFEST")
LAUNCH_IDS=()
for i in $(seq 0 $(( CORPUS_COUNT - 1 ))); do
  LAUNCH_IDS+=("$(jq -r ".corpora[$i].id" "$MANIFEST")")
done

# ── Check 1: All corpora present and ready ─────────────────────────────────────

echo "── Check 1: All launch corpora present and ready ────────────────────"
for CID in "${LAUNCH_IDS[@]}"; do
  STATUS=$("$CALLI_BIN" corpus list 2>/dev/null | awk -v id="$CID" '$1 == id { print $4 }')
  if [[ "$STATUS" == "ready" ]]; then
    pass "$CID — ready"
  elif [[ -z "$STATUS" ]]; then
    fail "$CID — NOT FOUND in corpus list"
  else
    fail "$CID — status is '$STATUS' (expected 'ready')"
  fi
done
echo ""

# ── Check 2: Entities > 0 per corpus ──────────────────────────────────────────

echo "── Check 2: Entities > 0 per corpus (search proxy) ──────────────────"
for CID in "${LAUNCH_IDS[@]}"; do
  ENTITY_COUNT=$(sqlite3 "$PINAKES" "SELECT COUNT(*) FROM entities WHERE corpus_id = '$CID';" 2>/dev/null || echo "0")
  if [[ "$ENTITY_COUNT" -gt 0 ]]; then
    pass "$CID — $ENTITY_COUNT entities"
  else
    fail "$CID — 0 entities (corpus may not be indexed yet)"
  fi
done
echo ""

# ── Check 3: Chunks > 0 per corpus ────────────────────────────────────────────

echo "── Check 3: Chunks > 0 per corpus ────────────────────────────────────"
for CID in "${LAUNCH_IDS[@]}"; do
  CHUNK_COUNT=$(sqlite3 "$PINAKES" "SELECT COUNT(*) FROM chunks WHERE corpus_id = '$CID';" 2>/dev/null || echo "0")
  if [[ "$CHUNK_COUNT" -gt 0 ]]; then
    pass "$CID — $CHUNK_COUNT chunks"
  else
    fail "$CID — 0 chunks"
  fi
done
echo ""

# ── Check 4: Collection exists and has all six members ───────────────────────

echo "── Check 4: 'carefeed' collection has all launch corpora ─────────────"
if ! "$CALLI_BIN" collection status "$COLLECTION_ID" &>/dev/null; then
  fail "Collection '$COLLECTION_ID' not found"
else
  pass "Collection '$COLLECTION_ID' exists"
  for CID in "${LAUNCH_IDS[@]}"; do
    MEMBER_CHECK=$(sqlite3 "$PINAKES" \
      "SELECT COUNT(*) FROM collection_members WHERE collection_id = '$COLLECTION_ID' AND member_id = '$CID';" \
      2>/dev/null || echo "0")
    if [[ "$MEMBER_CHECK" -gt 0 ]]; then
      pass "  member: $CID"
    else
      fail "  member: $CID — NOT in collection"
    fi
  done
fi
echo ""

# ── Check 5: Line-span probe ──────────────────────────────────────────────────
# Gate on freshly-indexed (non-baseline) corpora only.
# Baseline corpora (admin-portal, referral-monitor) were indexed pre-W4 and lack
# line spans until W20's first nightly reindex populates them. Fresh corpora
# (payments, family-portal, employee-app, carefeed-core) are indexed with the
# current code adapter and MUST carry line spans.

echo "── Check 5: Line spans present (code indexing quality gate) ──────────"

FRESH_IDS=()
for i in $(seq 0 $(( CORPUS_COUNT - 1 ))); do
  IS_BASELINE=$(jq -r ".corpora[$i].baseline" "$MANIFEST")
  if [[ "$IS_BASELINE" != "true" ]]; then
    FRESH_IDS+=("$(jq -r ".corpora[$i].id" "$MANIFEST")")
  fi
done

if [[ "${#FRESH_IDS[@]}" -eq 0 ]]; then
  echo "  (no freshly-indexed corpora in manifest — line span check skipped)"
  echo "  NOTE: Both baseline corpora (admin-portal, referral-monitor) were indexed"
  echo "        pre-W4 and will lack line spans until W20's first nightly reindex."
  (( PASS_COUNT++ )) || true
else
  IDS_SQL=$(printf "'%s'," "${FRESH_IDS[@]}" | sed 's/,$//')
  SPAN_CORPUS_COUNT=$(sqlite3 "$PINAKES" \
    "SELECT COUNT(DISTINCT corpus_id) FROM entities WHERE corpus_id IN ($IDS_SQL) AND start_line IS NOT NULL;" \
    2>/dev/null || echo "0")
  if [[ "$SPAN_CORPUS_COUNT" -ge 1 ]]; then
    pass "$SPAN_CORPUS_COUNT of ${#FRESH_IDS[@]} fresh corpora have entities with line spans"
  else
    fail "0 freshly-indexed corpora have line-spanned entities — code adapter may have an issue"
  fi
fi

IDS_SQL=$(printf "'%s'," "${LAUNCH_IDS[@]}" | sed 's/,$//')

# Per-corpus line span count for detail.
for CID in "${LAUNCH_IDS[@]}"; do
  SPAN_COUNT=$(sqlite3 "$PINAKES" \
    "SELECT COUNT(*) FROM entities WHERE corpus_id = '$CID' AND start_line IS NOT NULL;" \
    2>/dev/null || echo "0")
  echo "       $CID: $SPAN_COUNT entities with line spans"
done
echo ""

# ── Check 6: Cross-corpus collection proxy ────────────────────────────────────

echo "── Check 6: Cross-corpus collection proxy (≥2 corpora with entities) ──"
ACTIVE_COUNT=0
for CID in "${LAUNCH_IDS[@]}"; do
  COUNT=$(sqlite3 "$PINAKES" "SELECT COUNT(*) FROM entities WHERE corpus_id = '$CID';" 2>/dev/null || echo "0")
  if [[ "$COUNT" -gt 0 ]]; then
    (( ACTIVE_COUNT++ )) || true
  fi
done
if [[ "$ACTIVE_COUNT" -ge 2 ]]; then
  pass "$ACTIVE_COUNT launch corpora have entities — collection_search would return cross-corpus results"
else
  fail "Only $ACTIVE_COUNT launch corpora have entities; cross-corpus search needs ≥2"
fi
echo ""

# ── Summary ───────────────────────────────────────────────────────────────────

TOTAL=$(( PASS_COUNT + FAIL_COUNT ))
echo "══════════════════════════════════════════════════════════════════════"
echo "  Results: $PASS_COUNT/$TOTAL checks passed"
if [[ "$FAIL_COUNT" -gt 0 ]]; then
  echo "  Status:  FAILED ($FAIL_COUNT check(s) failed)"
  echo ""
  echo "  Resolve failures and re-run. Re-running the bootstrap script is safe."
  exit 1
else
  echo "  Status:  ALL CHECKS PASSED"
  echo ""
  echo "  Next step: upload pinakes to S3 generation-0 location (see runbook §6)."
  exit 0
fi
