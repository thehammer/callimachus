# Apple Developer Docs Corpus — macOS 26

A Callimachus pinakes built from Apple's public developer documentation for
AppKit, Combine, and Foundation targeting macOS 26. Intended for use alongside
a codebase pinakes (e.g. Nostromo) so a Claude session can answer AppKit
behavioural questions without leaving context.

## What this corpus is

- **Frameworks:** AppKit, Combine, Foundation
- **OS target:** macOS 26 (arm64-apple-macos26)
- **Coverage:** top-level types only (`class`, `struct`, `enum`, `protocol`
  with a single `pathComponent`). Methods and properties surface as abstract
  snippets in each type's **Topics** section.
- **Source:** Apple's public DocC JSON API at
  `developer.apple.com/tutorials/data/documentation/<framework>/<symbol>.json`
  — no Xcode docset download required.
- **Adapter:** `callimachus-adapter-wiki` (unchanged from v1). The DocC JSON
  is rendered to Markdown before ingestion; Callimachus treats it identically
  to a wiki page corpus.
- **Artifact:** `data/apple-docs-macos-26.pinakes` (SQLite + WAL sidecars).

## How to rebuild

**Requirements:**

- macOS with Xcode 26 installed (provides `swift-symbolgraph-extract` via `xcrun`)
- macOS 26 SDK at the default path (or update the `SDK=` line in the script)
- Python 3 (stdlib only)
- `calli` on `PATH` — build with `cargo build --release` then
  `export PATH=$PWD/target/release:$PATH`
- Network access to `developer.apple.com`

**Run:**

```bash
./scripts/build-apple-docs-macos-26.sh
```

This fetches ~500–600 Markdown files into `data/apple-docs-macos-26-src/`,
registers the corpus, and runs a full index pass. The first run takes roughly
10–20 minutes (network-bound at 0.15 s/request). Subsequent runs with no
`--force` flag skip already-fetched files and are fast.

To re-fetch all pages (e.g. after an Apple doc update):

```bash
python3 scripts/fetch-apple-docs.py \
  --framework AppKit --framework Combine --framework Foundation \
  --sdk /Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.sdk \
  --output-dir data/apple-docs-macos-26-src \
  --force
calli --pinakes data/apple-docs-macos-26.pinakes index apple-docs-macos-26 --pass all
```

## Nostromo pattern — two MCP servers in one session

`calli mcp` serves exactly one pinakes per process. Register two MCP server
entries in the consuming project's `.mcp.json` to query both the codebase and
the Apple docs from the same Claude session:

```json
{
  "mcpServers": {
    "callimachus-nostromo": {
      "command": "calli",
      "args": ["--pinakes", "${workspaceFolder}/data/nostromo.pinakes", "mcp"],
      "description": "Nostromo codebase index"
    },
    "callimachus-apple-docs": {
      "command": "calli",
      "args": ["--pinakes", "/absolute/path/to/apple-docs-macos-26.pinakes", "mcp"],
      "description": "Apple developer docs (AppKit, Combine, Foundation) for macOS 26"
    }
  }
}
```

The docs pinakes is referenced by absolute path so multiple projects can share
a single built artifact. Build it once, point every macOS project at it.

## Known limitations

1. **FTS5 AND semantics.** Full-text search uses AND across query terms, so
   multi-word natural-language queries (e.g. "why doesn't width propagate")
   fail when the terms don't co-occur within a single indexed chunk. Symbol-name
   queries (`NSStackView`, `readablePasteboardTypes`) work reliably.

2. **No per-method pages (v1).** Only top-level types are fetched as individual
   pages. Method and property docs appear as Topics-section snippets on their
   parent type page, not as standalone entities. v2's dedicated `docs` adapter
   will recover per-symbol pages.

3. **No structured cross-references.** `seeAlso`, inheritance edges, and
   conformance edges from the DocC JSON are not preserved — they live in v2's
   adapter as first-class graph edges.

4. **No availability metadata as queryable structure.** Availability prose
   (introduced-in, deprecated-in) appears in the rendered Markdown where Apple
   includes it, but is not indexed as a structured field.

## Refresh policy

Manual. Re-run the build script when:

- Apple publishes a significant doc update for the current OS release.
- A new macOS major releases — produce a new corpus with a new slug
  (e.g. `apple-docs-macos-27`) and update the `.mcp.json` entry.

The `.gitignore` patterns use `apple-docs-macos-*` wildcards so future
versions slot in without additional ignore entries.
