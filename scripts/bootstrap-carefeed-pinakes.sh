#!/usr/bin/env bash
# scripts/bootstrap-carefeed-pinakes.sh
#
# Idempotent driver for building the Carefeed production pinakes.
#
# Usage:
#   ./scripts/bootstrap-carefeed-pinakes.sh [OPTIONS]
#
# Options:
#   --pinakes <path>    Target pinakes path (default: /tmp/carefeed-production.pinakes)
#   --manifest <path>   Corpus manifest JSON (default: scripts/carefeed-corpora.manifest.json)
#   --baseline <path>   Source pinakes to copy if target does not exist
#                       (default: ~/Library/Application Support/callimachus/index.pinakes)
#   --concurrency <n>   LLM concurrency for index passes (default: 8)
#   --dry-run           Show what would happen; pass --dry-run to calli index calls
#   --skip-baseline-copy  Don't copy the baseline; start from an existing or empty pinakes
#
# Idempotency:
#   Re-running is safe. Each step checks existing state:
#     - Corpus add: skipped if already registered.
#     - Index: skipped if corpus is marked "baseline" in the manifest, or already ready.
#     - Collection add: always called (INSERT OR IGNORE — safe to repeat).
#
# LLM provider:
#   The 'index' command uses the provider from ~/Library/Application Support/callimachus/config.toml.
#   Ensure [llm] provider and api_key are set before running the index steps.
#   To override: set CALLIMACHUS_PROVIDER in your shell (not a calli flag — configure config.toml).
#
# Requires: calli, jq

set -euo pipefail

CALLI_BIN="${CALLI_BIN:-calli}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Defaults ─────────────────────────────────────────────────────────────────

PINAKES_TARGET="/tmp/carefeed-production.pinakes"
MANIFEST="${SCRIPT_DIR}/carefeed-corpora.manifest.json"
BASELINE_SOURCE="${HOME}/Library/Application Support/callimachus/index.pinakes"
CONCURRENCY=8
DRY_RUN=false
SKIP_BASELINE_COPY=false

# ── Arg parsing ───────────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pinakes)
      PINAKES_TARGET="$2"
      shift 2
      ;;
    --manifest)
      MANIFEST="$2"
      shift 2
      ;;
    --baseline)
      BASELINE_SOURCE="$2"
      shift 2
      ;;
    --concurrency)
      CONCURRENCY="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --skip-baseline-copy)
      SKIP_BASELINE_COPY=true
      shift
      ;;
    -h|--help)
      sed -n '3,25p' "$0"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

# ── Prereq checks ─────────────────────────────────────────────────────────────

if ! command -v "$CALLI_BIN" &>/dev/null; then
  echo "Error: calli binary not found (tried '$CALLI_BIN'). Set CALLI_BIN env var or install via: cargo install --path crates/callimachus-cli" >&2
  exit 1
fi

if ! command -v jq &>/dev/null; then
  echo "Error: jq is required but not found on PATH." >&2
  exit 1
fi

if [[ ! -f "$MANIFEST" ]]; then
  echo "Error: manifest not found: $MANIFEST" >&2
  exit 1
fi

export CALLIMACHUS_PINAKES="$PINAKES_TARGET"

DRY_RUN_FLAG=""
if $DRY_RUN; then
  DRY_RUN_FLAG="--dry-run"
fi

# ── Banner ────────────────────────────────────────────────────────────────────

echo ""
echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║         Carefeed Production Pinakes Bootstrap                    ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""
echo "  Pinakes:    $PINAKES_TARGET"
echo "  Manifest:   $MANIFEST"
echo "  Concurrency: $CONCURRENCY"
if $DRY_RUN; then
  echo "  Mode:       DRY-RUN (no writes)"
fi
echo ""

# ── Step 1: Baseline copy ─────────────────────────────────────────────────────

if ! $SKIP_BASELINE_COPY; then
  if [[ ! -f "$PINAKES_TARGET" ]]; then
    echo "── Step 1: Copying baseline pinakes ──────────────────────────────"
    if [[ ! -f "$BASELINE_SOURCE" ]]; then
      echo "Error: baseline pinakes not found: $BASELINE_SOURCE" >&2
      echo "  If starting from scratch, use --skip-baseline-copy." >&2
      exit 1
    fi
    if $DRY_RUN; then
      echo "  [dry-run] would copy: $BASELINE_SOURCE → $PINAKES_TARGET"
    else
      echo "  Copying (this may take a moment for a large file)…"
      cp "$BASELINE_SOURCE" "$PINAKES_TARGET"
      echo "  ✓ Baseline copied ($(du -sh "$PINAKES_TARGET" | cut -f1))"
    fi
  else
    echo "── Step 1: Baseline copy ─────────────────────────────────────────"
    echo "  ✓ Pinakes already exists — skipping copy"
  fi
else
  echo "── Step 1: Baseline copy ─────────────────────────────────────────"
  echo "  Skipped (--skip-baseline-copy)"
fi
echo ""

# ── Step 2: Register + index corpora ──────────────────────────────────────────

COLLECTION_ID=$(jq -r '.collection_id' "$MANIFEST")
COLLECTION_NAME=$(jq -r '.collection_name' "$MANIFEST")
CORPUS_COUNT=$(jq '.corpora | length' "$MANIFEST")

echo "── Step 2: Registering and indexing $CORPUS_COUNT corpora ────────────────"

corpus_is_registered() {
  local id="$1"
  "$CALLI_BIN" corpus status "$id" &>/dev/null
}

corpus_is_ready() {
  local id="$1"
  # Use 'corpus status' which has a labelled "Status" line — safe against multi-word names.
  "$CALLI_BIN" corpus status "$id" 2>/dev/null | grep -m1 "^Status" | grep -q "ready"
}

for i in $(seq 0 $(( CORPUS_COUNT - 1 ))); do
  CORPUS_ID=$(jq -r ".corpora[$i].id" "$MANIFEST")
  CORPUS_NAME=$(jq -r ".corpora[$i].name" "$MANIFEST")
  CORPUS_KIND=$(jq -r ".corpora[$i].kind" "$MANIFEST")
  CORPUS_SOURCE=$(jq -r ".corpora[$i].source" "$MANIFEST")
  CORPUS_BASELINE=$(jq -r ".corpora[$i].baseline" "$MANIFEST")

  echo ""
  echo "  [$((i+1))/$CORPUS_COUNT] $CORPUS_NAME ($CORPUS_ID)"

  # --- 2a: Registration ---
  if corpus_is_registered "$CORPUS_ID"; then
    echo "       register: ✓ already registered — skipping"
  else
    if [[ ! -d "$CORPUS_SOURCE" ]]; then
      echo "       register: ✗ source directory not found: $CORPUS_SOURCE" >&2
      echo "                  Resolve path, then re-run. Skipping this corpus." >&2
      continue
    fi
    if $DRY_RUN; then
      echo "       register: [dry-run] would run: calli corpus add $CORPUS_KIND \"$CORPUS_NAME\" \"$CORPUS_SOURCE\" --id $CORPUS_ID"
    else
      "$CALLI_BIN" corpus add "$CORPUS_KIND" "$CORPUS_NAME" "$CORPUS_SOURCE" --id "$CORPUS_ID"
      echo "       register: ✓ registered"
    fi
  fi

  # --- 2b: Indexing ---
  if [[ "$CORPUS_BASELINE" == "true" ]]; then
    echo "       index:    ✓ baseline corpus — skipping (already indexed in baseline)"
  elif corpus_is_ready "$CORPUS_ID"; then
    echo "       index:    ✓ already ready — skipping"
  else
    if $DRY_RUN; then
      echo "       index:    [dry-run] would run: calli index $CORPUS_ID --concurrency $CONCURRENCY --dry-run"
    else
      echo "       index:    running (concurrency=$CONCURRENCY)…"
      "$CALLI_BIN" index "$CORPUS_ID" --concurrency "$CONCURRENCY"
      echo "       index:    ✓ complete"
    fi
  fi
done

echo ""

# ── Step 3: Create / update collection ────────────────────────────────────────

echo "── Step 3: Collection setup ($COLLECTION_NAME) ───────────────────────────"

collection_exists() {
  local id="$1"
  "$CALLI_BIN" collection status "$id" &>/dev/null
}

if collection_exists "$COLLECTION_ID"; then
  echo "  ✓ Collection '$COLLECTION_ID' already exists"
else
  if $DRY_RUN; then
    echo "  [dry-run] would run: calli collection add \"$COLLECTION_NAME\""
  else
    "$CALLI_BIN" collection add "$COLLECTION_NAME"
    echo "  ✓ Collection '$COLLECTION_ID' created"
  fi
fi

# Add each corpus as a member (INSERT OR IGNORE — idempotent).
echo ""
for i in $(seq 0 $(( CORPUS_COUNT - 1 ))); do
  CORPUS_ID=$(jq -r ".corpora[$i].id" "$MANIFEST")
  if $DRY_RUN; then
    echo "  [dry-run] would run: calli collection add-member $COLLECTION_ID $CORPUS_ID"
  else
    if corpus_is_registered "$CORPUS_ID"; then
      "$CALLI_BIN" collection add-member "$COLLECTION_ID" "$CORPUS_ID"
    else
      echo "  ⚠ Skipping member add for '$CORPUS_ID' — not registered (registration failed earlier?)"
    fi
  fi
done

echo ""

# ── Step 4: Verification report ───────────────────────────────────────────────

echo "── Step 4: Verification report ──────────────────────────────────────"
echo ""
echo "  Corpora:"
"$CALLI_BIN" corpus list 2>/dev/null | awk 'NR==1 || NR==2 { print "  " $0; next } { found=0 }'

# Print rows for launch corpora only.
for i in $(seq 0 $(( CORPUS_COUNT - 1 ))); do
  CORPUS_ID=$(jq -r ".corpora[$i].id" "$MANIFEST")
  "$CALLI_BIN" corpus list 2>/dev/null | awk -v id="$CORPUS_ID" '$1 == id { print "  " $0 }'
done

echo ""
echo "  Collection members:"
if collection_exists "$COLLECTION_ID" && ! $DRY_RUN; then
  "$CALLI_BIN" collection status "$COLLECTION_ID" 2>/dev/null | sed 's/^/  /'
else
  echo "  (collection not created yet — dry-run mode)"
fi
echo ""

if $DRY_RUN; then
  echo "  [dry-run complete — no changes were made]"
else
  echo "  Bootstrap complete."
  echo "  Run scripts/verify-pinakes.sh --pinakes '$PINAKES_TARGET' to validate."
fi
echo ""
