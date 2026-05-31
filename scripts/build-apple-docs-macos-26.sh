#!/usr/bin/env bash
set -euo pipefail

# Resolve repo root and key paths
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SDK="/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.sdk"

if [ ! -d "$SDK" ]; then
  echo "error: SDK not found at $SDK — install Xcode 26 or update the path" >&2
  exit 1
fi

# Ensure calli is on PATH (caller's responsibility, but check)
command -v calli >/dev/null || {
  echo "error: calli not on PATH; run cargo build --release && export PATH=\$PWD/target/release:\$PATH" >&2
  exit 1
}

# ── v1 (wiki adapter, top-level types only) ──────────────────────────────────
OUTDIR="$ROOT/data/apple-docs-macos-26-src"
PINAKES="$ROOT/data/apple-docs-macos-26.pinakes"
CORPUS_ID="apple-docs-macos-26"

mkdir -p "$OUTDIR"

# 1. Fetch markdown for each framework
python3 "$ROOT/scripts/fetch-apple-docs.py" \
  --framework AppKit \
  --framework Combine \
  --framework Foundation \
  --sdk "$SDK" \
  --target arm64-apple-macos26 \
  --output-dir "$OUTDIR"

# 2. Register the corpus (idempotent — corpus add is safe to re-run;
#    if it errors on existing, fall through and just re-index).
calli --pinakes "$PINAKES" corpus add wiki "$CORPUS_ID" "$OUTDIR" || true

# 3. Run the full index pipeline.
calli --pinakes "$PINAKES" index "$CORPUS_ID" --pass all

echo "done v1: $PINAKES"

# ── v2 (docs adapter, top-level + child symbols) ─────────────────────────────
OUTDIR_V2="$ROOT/data/apple-docs-macos-26-v2-src"
PINAKES_V2="$ROOT/data/apple-docs-macos-26-v2.pinakes"
CORPUS_V2="apple-docs-macos-26-v2"

mkdir -p "$OUTDIR_V2"

# 1. Fetch DocC JSON for each framework, including child symbol pages.
#    Estimated time: ~35 minutes for AppKit at 0.15s/req for depth-2 symbols.
python3 "$ROOT/scripts/fetch-apple-docs.py" \
  --framework AppKit \
  --framework Combine \
  --framework Foundation \
  --sdk "$SDK" \
  --target arm64-apple-macos26 \
  --output-dir "$OUTDIR_V2" \
  --format json \
  --depth 2

# 2. Register the v2 corpus using the docs adapter.
calli --pinakes "$PINAKES_V2" corpus add docs "$CORPUS_V2" "$OUTDIR_V2" || true

# 3. Run the full index pipeline.
calli --pinakes "$PINAKES_V2" index "$CORPUS_V2" --pass all

echo "done v2: $PINAKES_V2"
