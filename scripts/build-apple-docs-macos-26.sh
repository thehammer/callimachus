#!/usr/bin/env bash
set -euo pipefail

# Resolve repo root and key paths
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SDK="/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.sdk"
OUTDIR="$ROOT/data/apple-docs-macos-26-src"
PINAKES="$ROOT/data/apple-docs-macos-26.pinakes"
CORPUS_ID="apple-docs-macos-26"

if [ ! -d "$SDK" ]; then
  echo "error: SDK not found at $SDK — install Xcode 26 or update the path" >&2
  exit 1
fi

# Ensure calli is on PATH (caller's responsibility, but check)
command -v calli >/dev/null || {
  echo "error: calli not on PATH; run cargo build --release && export PATH=\$PWD/target/release:\$PATH" >&2
  exit 1
}

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

echo "done: $PINAKES"
