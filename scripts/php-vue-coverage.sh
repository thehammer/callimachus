#!/usr/bin/env bash
# scripts/php-vue-coverage.sh
#
# PHP/Vue extraction quality coverage report (LLM-free passes only).
# Indexes a target directory with the Chunk + Structure passes (no LLM calls)
# and emits metrics for the PHP/Vue launch gate evaluation.
#
# Usage:
#   ./scripts/php-vue-coverage.sh <target-directory> [corpus-id]
#
# Example:
#   ./scripts/php-vue-coverage.sh ~/Code/admin-portal admin-portal
#   ./scripts/php-vue-coverage.sh ~/Code/referral-monitor referral-monitor
#
# Requirements:
#   - calli binary on PATH (or set CALLI_BIN env var)
#   - jq on PATH

set -euo pipefail

CALLI_BIN="${CALLI_BIN:-calli}"
TARGET="${1:-}"
CORPUS_ID="${2:-coverage-$$}"

if [[ -z "$TARGET" ]]; then
  echo "Usage: $0 <target-directory> [corpus-id]" >&2
  exit 1
fi

if ! command -v "$CALLI_BIN" &>/dev/null; then
  echo "Error: calli binary not found (tried '$CALLI_BIN'). Set CALLI_BIN env var." >&2
  exit 1
fi

if ! command -v jq &>/dev/null; then
  echo "Error: jq is required but not found on PATH" >&2
  exit 1
fi

TARGET="$(cd "$TARGET" && pwd)"
PINAKES="$(mktemp /tmp/calli-coverage-XXXXXX.pinakes)"
EXPORT_FILE="$(mktemp /tmp/calli-export-XXXXXX.jsonl)"
trap "rm -f '$PINAKES' '$EXPORT_FILE'" EXIT

echo ""
echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║         PHP/Vue Extraction Quality Report (LLM-free)            ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""
echo "  Target:    $TARGET"
echo "  Corpus ID: $CORPUS_ID"
echo "  Date:      $(date -u '+%Y-%m-%d %H:%M UTC')"
echo ""

# ── Phase 1: File discovery metrics ─────────────────────────────────────────

echo "── 1. File Discovery ─────────────────────────────────────────────────"

PHP_TOTAL=$(find "$TARGET" -type f -name "*.php" \
  ! -path "*/vendor/*" ! -path "*/node_modules/*" \
  ! -path "*/storage/*" ! -path "*/public/build/*" \
  ! -path "*/.git/*" 2>/dev/null | wc -l | tr -d '[:space:]')

BLADE_TOTAL=$(find "$TARGET" -type f -name "*.blade.php" \
  ! -path "*/vendor/*" ! -path "*/node_modules/*" \
  ! -path "*/storage/*" ! -path "*/public/build/*" \
  ! -path "*/.git/*" 2>/dev/null | wc -l | tr -d '[:space:]')

PHP_PURE=$((PHP_TOTAL - BLADE_TOTAL))

VUE_TOTAL=$(find "$TARGET" -type f -name "*.vue" \
  ! -path "*/vendor/*" ! -path "*/node_modules/*" \
  ! -path "*/storage/*" ! -path "*/public/build/*" \
  ! -path "*/.git/*" 2>/dev/null | wc -l | tr -d '[:space:]')

printf "  PHP files (total):         %s\n" "$PHP_TOTAL"
printf "    of which .blade.php:     %s\n" "$BLADE_TOTAL"
printf "    pure .php (non-blade):   %s\n" "$PHP_PURE"
printf "  Vue SFCs:                  %s\n" "$VUE_TOTAL"
echo ""

# ── Phase 2: Index with LLM-free passes ─────────────────────────────────────

echo "── 2. Indexing (chunk + structure passes, no LLM) ────────────────────"
printf "  Running: calli ingest chunk+structure on %s\n" "$TARGET"
echo ""

"$CALLI_BIN" --pinakes "$PINAKES" ingest code "$CORPUS_ID" "$TARGET" \
  --passes chunk,structure --yes
echo ""

# ── Phase 3: Export to JSONL ─────────────────────────────────────────────────

"$CALLI_BIN" --pinakes "$PINAKES" export "$CORPUS_ID" --output "$EXPORT_FILE" 2>/dev/null

TOTAL_CHUNKS=$(jq -r 'select(.record_type == "chunk")' "$EXPORT_FILE" | grep -c '"record_type"' || true)
TOTAL_ENTITIES=$(jq -r 'select(.record_type == "entity")' "$EXPORT_FILE" | grep -c '"record_type"' || true)
TOTAL_EDGES=$(jq -r 'select(.record_type == "edge")' "$EXPORT_FILE" | grep -c '"record_type"' || true)

printf "  Total chunks indexed:  %s\n" "$TOTAL_CHUNKS"
printf "  Total entities:        %s\n" "$TOTAL_ENTITIES"
printf "  Total edges:           %s\n" "$TOTAL_EDGES"
echo ""

# ── Chunk counts by kind ─────────────────────────────────────────────────────

echo "── 3. Chunk Counts by Kind ──────────────────────────────────────────"
jq -r 'select(.record_type == "chunk") | .kind' "$EXPORT_FILE" \
  | sort | uniq -c | sort -rn \
  | awk '{printf "  %-20s %s\n", $2, $1}'
echo ""

# ── PHP chunk breakdown ───────────────────────────────────────────────────────

echo "── 4. PHP Chunk Breakdown ───────────────────────────────────────────"

PHP_FILE_CHUNKS=$(jq -r 'select(.record_type == "chunk" and .kind == "file") | .location_uri' \
  "$EXPORT_FILE" | grep -c '\.php' || true)
BLADE_FILE_CHUNKS=$(jq -r 'select(.record_type == "chunk" and .kind == "file") | .location_uri' \
  "$EXPORT_FILE" | grep -c '\.blade\.php' || true)
PURE_PHP_FILE_CHUNKS=$((PHP_FILE_CHUNKS - BLADE_FILE_CHUNKS))
PHP_CLASS_CHUNKS=$(jq -r 'select(.record_type == "chunk" and .kind == "class") | .location_uri' \
  "$EXPORT_FILE" | grep -c '\.php' || true)
BLADE_ITEM_CHUNKS=$(jq -r 'select(.record_type == "chunk" and .kind != "file") | .location_uri' \
  "$EXPORT_FILE" | grep -c '\.blade\.php' || true)

printf "  PHP file chunks:               %s / %s discovered\n" "$PHP_FILE_CHUNKS" "$PHP_TOTAL"
printf "    .blade.php file chunks:      %s / %s discovered\n" "$BLADE_FILE_CHUNKS" "$BLADE_TOTAL"
printf "    pure PHP file chunks:        %s / %s discovered\n" "$PURE_PHP_FILE_CHUNKS" "$PHP_PURE"
printf "  PHP class-body chunks:         %s\n" "$PHP_CLASS_CHUNKS"
echo ""

# ── Blade template findings ────────────────────────────────────────────────────

echo "── 5. Blade Template Findings ───────────────────────────────────────"
printf "  .blade.php discovered:         %s\n" "$BLADE_TOTAL"
printf "  Blade file chunks indexed:     %s\n" "$BLADE_FILE_CHUNKS"
printf "  Blade item chunks (non-file):  %s\n" "$BLADE_ITEM_CHUNKS"
if [[ "$BLADE_ITEM_CHUNKS" -eq 0 ]]; then
  echo "  ✓ CLEAN DEGRADATION: Blade templates produce file chunks only."
  echo "    PHP parser applied but Blade directives produce no class/function items."
else
  echo "  ✗ WARNING: Blade templates produced item chunks — investigate false positives."
fi
echo ""

# ── Vue SFC findings ──────────────────────────────────────────────────────────

echo "── 6. Vue SFC Findings ─────────────────────────────────────────────"

VUE_FILE_CHUNKS=$(jq -r 'select(.record_type == "chunk" and .kind == "file") | .location_uri' \
  "$EXPORT_FILE" | grep -c '\.vue' || true)
VUE_ITEM_CHUNKS=$(jq -r 'select(.record_type == "chunk" and .kind != "file") | .location_uri' \
  "$EXPORT_FILE" | grep -c '\.vue' || true)

printf "  Vue SFCs discovered:           %s\n" "$VUE_TOTAL"
printf "  Vue file chunks:               %s\n" "$VUE_FILE_CHUNKS"
printf "  Vue item chunks (from script): %s\n" "$VUE_ITEM_CHUNKS"

if [[ "$VUE_TOTAL" -gt 0 && "$VUE_FILE_CHUNKS" -gt 0 ]]; then
  # Sample Vue item URIs to detect script setup vs options API
  VUE_SCRIPT_SETUP=$(jq -r 'select(.record_type == "chunk" and .kind != "file") | .location_uri' \
    "$EXPORT_FILE" | grep '\.vue' | head -5)
  if [[ -n "$VUE_SCRIPT_SETUP" ]]; then
    echo "  Sample Vue item chunk URIs:"
    echo "$VUE_SCRIPT_SETUP" | sed 's/^/    /'
  fi
fi
echo ""

# ── Entity counts by kind ─────────────────────────────────────────────────────

echo "── 7. Entity Counts by Kind ─────────────────────────────────────────"
jq -r 'select(.record_type == "entity") | .kind' "$EXPORT_FILE" \
  | sort | uniq -c | sort -rn \
  | awk '{printf "  %-20s %s\n", $2, $1}'
echo ""

# ── Edge counts by kind ───────────────────────────────────────────────────────

echo "── 8. Edge Counts by Kind ───────────────────────────────────────────"
jq -r 'select(.record_type == "edge") | .kind' "$EXPORT_FILE" \
  | sort | uniq -c | sort -rn \
  | awk '{printf "  %-20s %s\n", $2, $1}'
echo ""

# ── Spot-check: sample PHP class entities ─────────────────────────────────────

echo "── 9. Spot-check: Sample PHP Entities ──────────────────────────────"
echo "  Class entities (first 10):"
jq -r 'select(.record_type == "entity" and .kind == "class") | "    \(.canonical_name)"' \
  "$EXPORT_FILE" | head -10
echo ""
echo "  Method entities (first 10):"
jq -r 'select(.record_type == "entity" and .kind == "method") | "    \(.canonical_name)"' \
  "$EXPORT_FILE" | head -10
echo ""

# ── PHP extends/implements edges spot-check ───────────────────────────────────

echo "── 10. Spot-check: Inheritance Edges (first 10) ────────────────────"
jq -r 'select(.record_type == "edge" and (.kind == "extends" or .kind == "implements")) |
  "    \(.kind): \(.from_entity_id) → \(.to_entity_id)"' \
  "$EXPORT_FILE" | head -10
echo ""

echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║                         SUMMARY                                  ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""
printf "  PHP files:      %s total, %s blade, %s pure PHP\n" "$PHP_TOTAL" "$BLADE_TOTAL" "$PHP_PURE"
printf "  Vue SFCs:       %s total\n" "$VUE_TOTAL"
printf "  Chunks indexed: %s\n" "$TOTAL_CHUNKS"
printf "  Entities:       %s\n" "$TOTAL_ENTITIES"
printf "  Edges:          %s\n" "$TOTAL_EDGES"
echo ""
if [[ "$BLADE_ITEM_CHUNKS" -eq 0 ]]; then
  echo "  Blade behavior:   CLEAN DEGRADATION ✓"
else
  echo "  Blade behavior:   ITEM CHUNKS PRODUCED — review ✗"
fi
if [[ "$VUE_TOTAL" -gt 0 ]]; then
  printf "  Vue extraction:   %s file chunks, %s item chunks from %s SFCs\n" \
    "$VUE_FILE_CHUNKS" "$VUE_ITEM_CHUNKS" "$VUE_TOTAL"
fi
echo ""
