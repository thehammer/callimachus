# Apple Developer Documentation Corpus — macOS 26 (v1)

## Blockers: none

This plan is self-contained. Cody should be able to execute end-to-end without
re-reading the PRD or asking clarifying questions. All design decisions are
locked by the PRD at
`/Users/hammer/Code/callimachus/.claude/features/backlog/prd-documentation-corpus-apple-appkit.md`.

## Context

Nostromo (a macOS GUI app under active development against AppKit / Combine /
Foundation) currently loses iteration cycles to AppKit behavioural questions
that *are* answered in Apple's reference docs but require either prior
knowledge of where to look or a semantic index to find. Callimachus already
indexes code corpora with semantic search, entity graph, summaries, themes,
and FTS — the same machinery works directly on documentation prose.

A 20-symbol spike on 2026-05-29 validated the steel thread end-to-end:

- Apple serves full narrative documentation (Discussion sections, asides, code
  examples) as JSON at
  `https://developer.apple.com/tutorials/data/documentation/<framework>/<symbol>.json`.
  No Xcode docset download and no `docc convert` CLI is required.
- `xcrun swift-symbolgraph-extract` enumerates the full symbol list for a
  framework against a chosen SDK; `pathComponents` from the symbol graph map
  directly to URL slugs.
- The DocC JSON renders cleanly to markdown preserving Discussion prose,
  `> Note:` / `> Important:` asides, code examples, and Topic listings.
- The existing `callimachus-adapter-wiki` adapter (no changes required)
  ingested 20 generated pages into 371 chunks / 20 entities; MCP `search`,
  `read`, `entity`, and `corpus_overview` all returned correct results
  end-to-end (verified `readablePasteboardTypes` → `NSTextView` and
  `autosaveName` → `NSSplitView` with the UserDefaults side-effect snippet).

v1 ships a fetch/render script, a reproducible build script that produces a
pinakes named `apple-docs-macos-26.pinakes`, a `.mcp.json` pattern for
registering two Callimachus MCP servers side-by-side, and `.gitignore`
hygiene for the generated artifacts. Nothing in the Rust source tree
changes.

## Target

- **Repo:** callimachus
- **Branch:** `feature/apple-docs-corpus-v1`
- **Base:** `origin/main`

## Files to create

- `scripts/fetch-apple-docs.py` — Python 3 fetcher (no third-party deps;
  uses only `urllib`, `json`, `subprocess`, `argparse`, `pathlib`, `time`).
- `scripts/build-apple-docs-macos-26.sh` — reproducible end-to-end build:
  fetch markdown for AppKit + Combine + Foundation against the macOS 26 SDK,
  then `corpus add` and `index --pass all` into
  `data/apple-docs-macos-26.pinakes`.
- `docs/apple-docs-corpus.md` — short operator note documenting how to
  rebuild the corpus, how to register both pinakes in a consuming repo's
  `.mcp.json`, and the known FTS5 AND-semantics limitation. Cody should
  treat this as developer-facing documentation, not a marketing doc — keep
  it ~80 lines.

## Files to modify

- `.gitignore` — add three lines (see step 7 below).

## Files NOT to modify

Cody must touch zero Rust source. The wiki adapter, MCP server, query layer,
storage layer, and CLI are all out of scope. If any of these need to change
for v1 to work, that is a *finding* to surface — not a workstream to expand
into. See "Out of scope" at the bottom.

## Approach

### 1. Write `scripts/fetch-apple-docs.py`

A small, single-file Python 3 script. No async, no threads, no third-party
packages. Behaviour:

**CLI surface:**

```
fetch-apple-docs.py
  --framework <name>          (repeatable; e.g. --framework AppKit --framework Combine --framework Foundation)
  --sdk <path>                (path to .sdk; e.g. /Applications/Xcode.app/.../MacOSX26.sdk)
  --target <triple>           (default: arm64-apple-macos26)
  --output-dir <path>         (markdown files written here, one per top-level type)
  --rate-limit <seconds>      (default: 0.15)
  --force                     (re-fetch even if <ClassName>.md already exists)
```

**Per framework, do:**

1. Create a temporary directory.
2. Run:
   ```
   xcrun swift-symbolgraph-extract \
     -module-name <Framework> \
     -target <target> \
     -sdk <sdk> \
     -output-dir <tmpdir>
   ```
   If the command fails, print the captured stderr and exit non-zero. Do not
   continue silently.
3. Load `<tmpdir>/<Framework>.symbols.json` (UTF-8 JSON).
4. Iterate `symbols[]`. Keep entries where:
   - `kind.identifier` is in
     `{"swift.class", "swift.struct", "swift.enum", "swift.protocol"}`,
     **and**
   - `pathComponents` has length exactly 1 (top-level types only — methods
     and properties are reached via each class page's Topics section in v1).
5. For each kept symbol, derive the URL:
   ```
   slug = "/".join(pc.lower() for pc in pathComponents)
   url  = f"https://developer.apple.com/tutorials/data/documentation/{framework.lower()}/{slug}.json"
   ```
6. Output path: `<output-dir>/<PathComponents[0]>.md` (preserve original
   case, e.g. `NSStackView.md`).
7. If the file exists and `--force` was not passed, skip and increment
   `skipped`.
8. Otherwise:
   - GET the URL with a `User-Agent: callimachus-apple-docs-fetcher/1.0`
     header. Treat HTTP 404 as a graceful skip (some symbols enumerated
     by the symbol graph have no published reference page — this is
     normal). Treat any other non-2xx as a failure (log + increment
     `failed`, do not abort).
   - On success, render markdown (see "Markdown render" below) and write
     to disk.
   - Sleep `--rate-limit` seconds before the next request.
9. After all frameworks: print a summary line —
   `fetched=<n> skipped=<n> failed=<n> per_framework=<map>`.

**Markdown render** — produce one `.md` file per symbol with this structure
(elide sections that aren't present in the JSON):

```markdown
# <metadata.title>

**Kind:** <metadata.symbolKind>   <!-- e.g. "Class", "Structure", "Protocol" -->
**Framework:** <Framework>
**Source URL:** <the .json URL above>

<abstract paragraph from `abstract[]`, joined `text` nodes>

## Declaration

```swift
<declarations from `primaryContentSections[].declarations[].tokens[]`, concatenated>
```

## Discussion

<rendered from primaryContentSections[] where kind=="content">
```

For Discussion rendering: walk `content[]` nodes and emit:

| DocC node `type`                    | Markdown                                      |
|-------------------------------------|-----------------------------------------------|
| `heading`                           | `## <text>` (use `level` for `#` count, min 2)|
| `paragraph`                         | inline-rendered paragraph                     |
| `aside` (`style: "note"/"important"/"warning"/"tip"`) | `> **Note:** ...` blockquote |
| `codeListing`                       | fenced ` ```swift ` block from `code[]` lines |
| `unorderedList` / `orderedList`     | `-` / `1.` lines, recursively render items    |
| `links`                             | inline as plain text (no link resolution v1)  |
| other                               | best-effort plain text fallback               |

Inline rendering (inside `paragraph` / `heading`): walk `inlineContent[]` and
concatenate — `text` → raw text; `codeVoice` → `` `text` ``; `emphasis` →
`*text*`; `strong` → `**text**`; `reference` → use the `identifier`'s
plain title (resolve from `references[]` if present, else fall back to the
identifier tail); `image` → `![alt](src)` only if a URL is available, else
drop.

After the Discussion section, append a Topics section if `topicSections[]`
is non-empty:

```markdown
## Topics

### <topicSections[i].title>

- **<identifier title>** — <abstract from references[identifier]>
- ...
```

The Topics listing is how v1 surfaces method/property docs — they appear as
abstract snippets attached to their parent type page, which is what FTS
searches against.

**Reuse the spike's working render code.** The plan does not include
pseudocode for inline rendering line-by-line; the spike already produced a
working renderer and Cody can adapt the same shape directly into this
script. The above table is the contract the renderer must satisfy.

**Idempotency:** by default, skip files that already exist on disk
(`<output-dir>/<ClassName>.md`). `--force` overrides.

**Error handling philosophy:** print a one-line warning per failed symbol
to stderr (`WARN <framework>/<symbol>: <reason>`) and keep going. Exit 0
if the symbol graph extraction succeeded for every framework, even if
some individual JSON fetches failed — the corpus is useful incomplete.
Exit non-zero only if `swift-symbolgraph-extract` failed or no symbols
were fetched at all.

### 2. Write `scripts/build-apple-docs-macos-26.sh`

Bash script. POSIX-compatible-ish (bash 3.2 is fine — macOS default).
`set -euo pipefail` at the top. Behaviour:

```bash
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
command -v calli >/dev/null || { echo "error: calli not on PATH; run cargo build --release && export PATH=\$PWD/target/release:\$PATH" >&2; exit 1; }

mkdir -p "$OUTDIR"

# 1. Fetch markdown for each framework
python3 "$ROOT/scripts/fetch-apple-docs.py" \
  --framework AppKit \
  --framework Combine \
  --framework Foundation \
  --sdk "$SDK" \
  --target arm64-apple-macos26 \
  --output-dir "$OUTDIR"

# 2. Register the corpus (idempotent — `corpus add` should be safe to re-run;
#    if it errors on existing, fall through and just re-index).
calli --pinakes "$PINAKES" corpus add wiki "$CORPUS_ID" "$OUTDIR" || true

# 3. Run the full index pipeline.
calli --pinakes "$PINAKES" index "$CORPUS_ID" --pass all

echo "done: $PINAKES"
```

`chmod +x scripts/build-apple-docs-macos-26.sh` so it runs directly.

**Cody must not actually execute this script** as part of plan execution
(it requires Xcode 26 installed, an SDK on disk, and live network access
to developer.apple.com; that's the operator's job). Cody's job is to
write the script, write the fetcher it calls, dry-run the fetcher's
`--help` to confirm argparse works, and stop there.

### 3. Verify `calli mcp` single-pinakes ergonomics (no code changes)

Read `crates/callimachus-cli/src/commands/mcp.rs` and confirm what is
already established by Archie's research: `calli mcp` accepts exactly one
`--pinakes` path (resolved via `resolve_pinakes_path` in `src/config.rs`)
and serves that single pinakes. There is no multi-pinakes mode.

**v1 decision (locked):** consuming repos register two MCP server entries
in their `.mcp.json`, one per pinakes. No Rust changes. Document this
pattern in `docs/apple-docs-corpus.md` (step 4).

### 4. Write `docs/apple-docs-corpus.md`

A short operator-facing note covering:

- **What this corpus is** — three frameworks (AppKit, Combine, Foundation)
  for macOS 26, fetched from Apple's public JSON API, rendered to markdown,
  indexed via the wiki adapter. Top-level types only; methods and
  properties surface via each type's Topics section.
- **How to rebuild** — `./scripts/build-apple-docs-macos-26.sh`.
  Requirements: Xcode 26 with the macOS 26 SDK installed, Python 3, `calli`
  on PATH, network access to `developer.apple.com`.
- **How a consuming project uses it (the Nostromo pattern).** Show a
  worked `.mcp.json` snippet registering two Callimachus MCP server
  entries side by side:

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

  Note that the docs pinakes is referenced by absolute path so multiple
  consuming repos can share a single built artifact. The Nostromo team
  builds the docs pinakes once and points each macOS project at it.
- **Known limitations** — (a) FTS5 uses AND semantics across query
  terms, so multi-word natural-language queries fail when terms don't
  co-occur in a single chunk; symbol-name queries work perfectly.
  (b) Structured cross-references (`seeAlso`, inheritance, conformance)
  from DocC JSON are not preserved in v1 — they live in v2's dedicated
  `docs` adapter. (c) Per-method pages are not generated; method/property
  docs are present as Topics-section snippets on their parent type page.
- **Refresh policy** — manual; re-run the build script when Apple
  publishes a doc update or when a new macOS major releases (which will
  produce a new corpus slug, e.g. `apple-docs-macos-27`).

Keep this file pragmatic and short. No marketing prose.

### 5. Update `.gitignore`

Append three patterns to the existing `.gitignore`:

```
# Apple docs corpus — generated artifacts (rebuild via scripts/build-apple-docs-macos-26.sh)
data/apple-docs-macos-*.pinakes
data/apple-docs-macos-*.pinakes-shm
data/apple-docs-macos-*.pinakes-wal
data/apple-docs-macos-*-src/
```

The wildcard `macos-*` is intentional and forward-compatible with v2's
multi-version expansion. Markdown sources are gitignored because they are
deterministically reproducible from a Python script plus a pinned SDK
version — no value in committing 524+ generated files.

### 6. Verify the script's CLI surface (lightweight)

Cody runs (these are cheap and don't require Xcode):

```
python3 scripts/fetch-apple-docs.py --help
bash -n scripts/build-apple-docs-macos-26.sh   # syntax check only, no execution
```

Both must succeed with exit code 0. This is the only execution-time
verification Cody is responsible for. End-to-end verification against
the macOS 26 SDK is the operator's job after merge.

### 7. Commit

Single commit, message body roughly:

```
feat(apple-docs): v1 corpus builder for macOS 26

Adds scripts/fetch-apple-docs.py and scripts/build-apple-docs-macos-26.sh
plus an operator note in docs/apple-docs-corpus.md. The fetcher enumerates
top-level AppKit / Combine / Foundation types via swift-symbolgraph-extract,
pulls Apple's public DocC JSON, and renders to markdown that the existing
wiki adapter ingests unchanged. No Rust source changes — the steel thread
is "wiki adapter is enough for v1." See the PRD at
.claude/features/backlog/prd-documentation-corpus-apple-appkit.md for
the design decisions and the v2 destination.
```

Open a PR against `main`.

## Acceptance criteria

Cody-verifiable (these are the gates Cody must check before opening the PR):

- [ ] `scripts/fetch-apple-docs.py` exists, is executable, and
      `python3 scripts/fetch-apple-docs.py --help` exits 0 and lists all
      documented flags (`--framework`, `--sdk`, `--target`, `--output-dir`,
      `--rate-limit`, `--force`).
- [ ] `scripts/build-apple-docs-macos-26.sh` exists, is executable, and
      `bash -n scripts/build-apple-docs-macos-26.sh` exits 0.
- [ ] `docs/apple-docs-corpus.md` exists and includes the two-server
      `.mcp.json` snippet.
- [ ] `.gitignore` contains the four new patterns from step 5.
- [ ] No files under `crates/` have been modified.
- [ ] `cargo check --workspace` still passes (regression guard, no Rust
      should have changed but verify).
- [ ] PR description links the PRD path and the parent FR.

Operator-verifiable (post-merge, not part of Cody's gate — listed so the
operator knows the success bar from the PRD; these are the queries that
must work after the operator runs the build script on a machine with
Xcode 26):

- [ ] `./scripts/build-apple-docs-macos-26.sh` completes and produces
      `data/apple-docs-macos-26.pinakes` plus WAL/SHM sidecars.
- [ ] Symbol-name search via MCP: `search` for `readablePasteboardTypes`
      returns an `NSTextView` chunk with the pasteboard discussion in
      the snippet.
- [ ] Symbol-name search: `search` for `autosaveName` returns an
      `NSSplitView` chunk that mentions the `UserDefaults` side-effect.
- [ ] Symbol-name search: `search` for `NSStackView.alignment` returns
      the `NSStackView` entity in the top result.
- [ ] `entity` lookup for `NSView` returns the property/method abstracts
      in the Topics-section render, including `tag`.
- [ ] `corpus_overview` for `apple-docs-macos-26` returns a sensible
      high-level summary.
- [ ] `corpus_themes` runs and either returns useful thematic groupings
      *or* a documented "themes were not useful at this corpus scale"
      finding (acceptable v1 outcome per PRD open question #4).
- [ ] A consuming repo with both `.mcp.json` entries (Nostromo pattern)
      can call MCP tools against both pinakes in the same Claude session.

## Out of scope

Explicitly forbidden for this plan. If any of these appear in the diff,
the reviewer should reject the PR:

- **No changes to the wiki adapter.** The whole point of v1 is that the
  wiki adapter is sufficient as-is. If Cody discovers it isn't,
  surface that as a finding — don't extend the adapter.
- **No changes to the MCP server, MCP tool surface, or any dispatch
  code.** Multi-pinakes mode is a possible v2/v3 ergonomics feature; two
  `.mcp.json` entries is the v1 answer.
- **No structured cross-reference extraction.** `seeAlso`, inheritance
  edges, conformance edges, parameter-type edges — all present in the
  DocC JSON, all dropped in v1's markdown render. v2's dedicated `docs`
  adapter recovers them.
- **No per-method or per-property pages.** Top-level types only
  (`swift.class`, `swift.struct`, `swift.enum`, `swift.protocol` with
  `pathComponents` length 1). Method/property docs appear as Topics
  snippets on their parent type page.
- **No automated refresh, watcher, scheduler, or change detection.**
  Manual `./scripts/build-apple-docs-macos-26.sh` only.
- **No scholia tooling.** Schema supports it; v1 doesn't drive it.
- **No cross-corpus query primitives.** No `entity_meet` across
  Nostromo and Apple docs pinakes. The session bridges them.
- **No async / parallel fetching, no retry/backoff logic** in the
  fetcher beyond a simple try/except-skip on individual symbol errors.
  Apple's docs site is fast and the rate-limited serial loop completes
  in well under an hour for three frameworks. Concurrency is premature.
- **No additional frameworks.** AppKit, Combine, Foundation only.
  CoreGraphics, CoreText, SwiftUI, Network — all v2.
- **No other macOS versions.** macOS 26 only. The wildcard in
  `.gitignore` is forward-compatibility plumbing, not an invitation to
  build a multi-version pipeline.

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: medium
    rationale: "Scripts + docs + gitignore; no Rust changes. Reuses spike's render logic. Medium effort gives room to handle DocC JSON edge cases cleanly."
  redd:
    skip: true
    rationale: "No Rust code changes; nothing in the workspace test suite needs new coverage. Operator-verifiable criteria require Xcode 26 and are out of CI scope."
  marty:
    skip: true
    rationale: "Single-shot scripts; no existing duplication to consolidate and no refactor surface area."
  perri:
    model: sonnet
    effort: medium
    rationale: "Reviewer should confirm zero Rust changes, gitignore patterns are correct, the .mcp.json snippet is valid, and the out-of-scope list held."
```
