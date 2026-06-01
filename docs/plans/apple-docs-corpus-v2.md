# Apple Developer Documentation Corpus — macOS 26 (v2)

## Blockers: none — clean to queue

This plan is self-contained. v2 sits on top of v1 (PR #35, branch
`feature/apple-docs-corpus-v1`), so the fetch script, build script,
operator note, and `.gitignore` patterns already exist in the base
branch. v2 adds a new Rust adapter crate plus extensions to the fetch
script; no v1 file is rewritten, only extended.

**Risk flagged but contained: SourceAdapter trait stability.**
Honest-provenance PR 2 (per `docs/plans/honest-provenance-implementation.md`)
explicitly states in its "Out of scope" section that *"adapter trait
changes (the source adapters stay provenance-agnostic)"*. The trait
surface this plan implements against (`discover`, `chunk`,
`extract_structure`, `extract_with_llm`, `summarize`, `resolve_aliases`,
`format_location`, `parse_location`, plus the optional Phase-12
methods) is therefore expected to be stable across the
honest-provenance work. The new adapter writes
`derived_at_version` strings exactly the way `BookAdapter` and
`WikiAdapter` do today — honest-provenance PR 1 ships a backward-compat
facade for that field. **Conclusion:** v2 can land independently of
honest-provenance; rebasing onto a post-PR-2 main may require updating
the adapter to stamp `Provenance::Concrete(sha)` instead of a bare
version string, but that is a one-call-site change and is not a
correctness risk.

## Context

v1 ships a script-based pipeline: `scripts/fetch-apple-docs.py` pulls
Apple's DocC JSON for AppKit / Combine / Foundation top-level types,
renders to markdown, and indexes via the existing `WikiAdapter`. v1 is
useful and shipped, but it discards every piece of structured
information in the DocC JSON that the wiki adapter can't see in plain
markdown:

- `relationships[]` (`inheritsFrom`, `conformsTo`) — entity edges.
- `primaryContentSections[].declarations[].tokens[]` with
  `kind: "typeIdentifier"` — parameter / return / property type
  references.
- `topicSections[].identifiers[]` — the parent-child membership graph
  that lets us treat methods and properties as first-class entities
  with their own pages.
- `availability[]` — `introducedAt`, `deprecatedAt` per platform.
- The fine-grained entity taxonomy (`class`, `struct`, `enum`,
  `protocol`, `method`, `property`, `init`, `case`, `notification`,
  `typealias`, `constant`) instead of v1's flat "topic / page" shape.

v2's job is to keep the v1 pipeline runnable (for comparison and
fallback) and add a parallel, structured pipeline driven by a new
`docs` adapter that reads the DocC JSON directly. The fetch script
gains a `--depth 2` mode that also retrieves child symbol pages
(methods, properties, enum cases) so each gets its own page with full
Discussion prose rather than a one-line Topics snippet.

The v2 outcome is a richer pinakes (`apple-docs-macos-26-v2.pinakes`)
with structured edges and per-symbol pages, alongside the existing v1
pinakes that consumers can keep using until they switch.

## Target

- **Repo:** callimachus (this workspace, `/Users/hammer/Code/callimachus`)
- **Branch:** `feature/apple-docs-corpus-v2`
- **Base:** `feature/apple-docs-corpus-v1` (PR #35 — branch off this,
  not `main`, so the v1 scripts and docs are already present)
- **Isolation:** worktree (independent of any honest-provenance work)

## Files to create

- `crates/adapters/callimachus-adapter-docs/Cargo.toml` — new crate
  manifest, mirroring `callimachus-adapter-wiki/Cargo.toml`. Deps:
  `callimachus-core` (path), `callimachus-llm` (path), `anyhow`,
  `serde`, `serde_json`, `tokio`, `tracing`, `async-trait`, `walkdir`,
  `chrono`, `uuid`, `sha2`, `regex`.
- `crates/adapters/callimachus-adapter-docs/src/lib.rs` — re-exports
  `DocsAdapter` and a `create()` constructor (matches wiki adapter).
- `crates/adapters/callimachus-adapter-docs/src/adapter.rs` — the
  `SourceAdapter` impl. Top-level file.
- `crates/adapters/callimachus-adapter-docs/src/docc.rs` — DocC JSON
  parser: typed structs (`DoccPage`, `DoccMetadata`, `DoccRelationship`,
  `DoccDeclaration`, `DoccTopicSection`, `DoccAvailability`,
  `DoccReference`) using `serde(rename_all = "camelCase")` + permissive
  `#[serde(default)]` everywhere. **Use `serde_json::Value` for
  `primaryContentSections[].content`** — that tree is deeply variant
  and the v1 Python render code already walks it heuristically; the
  Rust side does not need to model every node type, only enough to
  extract Discussion text for `description` fields.
- `crates/adapters/callimachus-adapter-docs/src/chunker.rs` — produces
  one page-grain chunk per DocC JSON file. The chunk's `content` is
  the same markdown render the v1 fetcher produces (re-rendered in
  Rust so the adapter is self-contained — see Approach §3). One
  section-grain chunk per `primaryContentSections[]` whose `kind ==
  "content"` (i.e. one chunk for the Discussion block).
- `crates/adapters/callimachus-adapter-docs/src/extractor.rs` — the
  load-bearing structured-edge extractor (see Approach §4).
- `crates/adapters/callimachus-adapter-docs/src/summarizer.rs` —
  near-identical to `wiki/summarizer.rs`. The docs corpus needs `page`
  and `section` summaries (DocC pages have natural prose Discussion);
  corpus-level summarization comes from the standard pass.
- `crates/adapters/callimachus-adapter-docs/src/render.rs` — Rust port
  of the v1 Python markdown renderer. Keeps the adapter self-contained
  so the pinakes carries the same `chunk.content` text v1 generated.
- `crates/adapters/callimachus-adapter-docs/tests/adapter_smoke.rs` —
  integration test: load a small fixture DocC JSON tree from
  `crates/adapters/callimachus-adapter-docs/tests/fixtures/` (committed
  alongside the test, two or three handwritten symbols sufficient),
  call `discover` → `chunk` → `extract_structure` on each, assert
  entity kinds, edge kinds, and parent/child membership.
- `docs/plans/apple-docs-corpus-v2.md` — this plan.

## Files to modify

- `Cargo.toml` (workspace root) — add
  `"crates/adapters/callimachus-adapter-docs"` to `workspace.members`.
- `crates/callimachus-cli/Cargo.toml` — add
  `callimachus-adapter-docs = { path = "../adapters/callimachus-adapter-docs" }`.
- `crates/callimachus-cli/src/commands/corpus.rs` — extend the help text
  for `CorpusCommand::Add::kind` from "book, code, wiki" to
  "book, code, wiki, docs" (line 13). No other change needed; `kind`
  is a free-form string at registration time.
- `crates/callimachus-cli/src/commands/index.rs` —
  - `use callimachus_adapter_docs::DocsAdapter;` (alongside the other
    three adapter imports near line 4–6).
  - Add `"docs" => Ok(Arc::new(DocsAdapter::new())),` to the
    `build_adapter` match arms near line 102.
- `scripts/fetch-apple-docs.py` — extend with `--format` and
  `--depth` flags (see Approach §2). Backward-compatible: defaults
  preserve the v1 behaviour exactly.
- `scripts/build-apple-docs-macos-26.sh` — add a second invocation
  block that builds the v2 pinakes via the `docs` adapter, alongside
  the existing v1 block (see Approach §6). The v1 block stays
  byte-identical to what's on `feature/apple-docs-corpus-v1` so
  consumers who haven't migrated keep working.
- `docs/apple-docs-corpus.md` — append a short "v2 corpus" section at
  the bottom describing the structured-edges difference, the
  `--depth 2` extra-cost note (~35 minutes added for AppKit at
  0.15s/req), and the new `apple-docs-macos-26-v2.pinakes` artifact.
- `.gitignore` — add patterns for the v2 artifacts (see Approach §7).

## Files NOT to modify

- `callimachus-adapter-wiki` — untouched. v1 keeps using it.
- The MCP server, storage layer, query layer, and migration tree — no
  schema changes. v2 fits inside the existing storage shape because
  the structured edges are written via the same `Edge` type the code
  adapter uses (`from_entity_id`, `to_entity_id`, `kind`).
- Honest-provenance work — this plan does not depend on it and must
  not pre-empt it. If a rebase post-PR-2 needs the adapter to stamp
  `Provenance::Concrete(...)` instead of a version string, that is a
  trivial follow-up.

## Approach

### 1. Scaffold the new crate

Create `crates/adapters/callimachus-adapter-docs/` with the file
layout listed above. Mirror `callimachus-adapter-wiki/`'s `Cargo.toml`
and `lib.rs` structure exactly:

```rust
// lib.rs
pub mod adapter;
pub mod chunker;
pub mod docc;
pub mod extractor;
pub mod render;
pub mod summarizer;

pub use adapter::DocsAdapter;

pub fn create() -> DocsAdapter {
    DocsAdapter::new()
}
```

`DocsAdapter::kind()` returns `"docs"`. `DocsAdapter::version()`
returns `env!("CARGO_PKG_VERSION")`. `summary_levels()` returns
`vec!["section", "page"]` (same as wiki).

Wire the crate into the workspace (`Cargo.toml` root) and into the CLI
(`crates/callimachus-cli/Cargo.toml` + `index.rs` `build_adapter`
match arm + `corpus.rs` help text). After this step,
`cargo build --workspace` must still succeed and
`calli corpus add docs ...` must work end-to-end against an empty
directory (it'll find zero sources, which is fine).

### 2. Extend `scripts/fetch-apple-docs.py` with `--format` and `--depth`

Add two new CLI flags. **Defaults preserve current behaviour.**

```
--format {markdown,json,both}   default: markdown
    markdown — render to .md (v1 behaviour, untouched)
    json     — write raw DocC JSON files (one .json per symbol)
    both     — write both .md and .json side-by-side

--depth {1,2}                   default: 1
    1 — top-level types only (v1 behaviour)
    2 — also fetch each top-level type's child symbol pages
        (methods, properties, enum cases) via topicSections[].identifiers[]
```

Implementation notes:

- When `--format` includes `json`, write each fetched DocC payload
  unmodified to `<output-dir>/<Framework>/<symbol-slug>.json` (mirror
  the URL path under the output directory). The directory layout
  matters for the adapter's `discover` walk — the adapter uses the
  path to recover the framework name and `pathComponents`.
- When `--format == markdown` (default), the existing markdown writer
  runs unchanged. When `--format == both`, both writers run.
- When `--depth == 2`, after fetching each top-level type's JSON,
  walk its `topicSections[].identifiers[]`. Each identifier looks like
  `"doc://com.apple.documentation/documentation/appkit/nsstackview/alignment"`.
  Resolve to a URL by replacing the `doc://com.apple.documentation/`
  prefix with `https://developer.apple.com/tutorials/data/`. Fetch
  with the same rate-limit and error-tolerance policy as the
  top-level fetch (404 → skip; other non-2xx → log + continue).
- Estimate / warning: print a one-line warning before depth-2 starts
  noting "~14000 additional symbols at 0.15s/req ≈ 35 min" so the
  operator knows what they're about to do. Suppress with `--quiet`.
- The script's summary line gains a `depth=` field and the
  per-framework counts split top-level vs. child:
  `fetched_top=<n> fetched_children=<n> skipped=<n> failed=<n>`.
- Idempotency: as in v1, files already on disk are skipped unless
  `--force` is passed.

The JSON-emission path is straightforward: the script already has the
parsed JSON in memory before rendering — just `json.dump` it to the
mirrored path before (or instead of) calling the markdown renderer.

### 3. Port the markdown render to Rust (`render.rs`)

The v1 Python renderer (in `scripts/fetch-apple-docs.py` on
`feature/apple-docs-corpus-v1`) is already specified by a clear node-
type table — see the v1 plan at `docs/plans/apple-docs-corpus-v1.md`
section "Markdown render". Port that contract to Rust:

- Input: a parsed DocC JSON `serde_json::Value` (or the typed shape
  from `docc.rs` where helpful — `references` map and primary content
  tree).
- Output: a `String` of markdown that the chunk's `.content` field
  carries.

The output should be **byte-equivalent or near-byte-equivalent to the
v1 Python output for the same input**, so that side-by-side queries
against v1 and v2 pinakes return comparable snippets. Cody should
spot-check this by running the v1 script against a single AppKit
symbol with `--format both`, then running the Rust renderer against
the same JSON and diffing the outputs. Small whitespace differences
are acceptable; substantive content drift is not.

This is the same renderer the chunker will call to populate
`chunk.content` for both page and section chunks. Keep it as a
standalone function so the test fixture can exercise it directly.

### 4. Structured extraction (`extractor.rs`) — the load-bearing piece

For each parsed DocC page, produce:

- **One primary entity** per page, with the kind mapped from
  `metadata.symbolKind` per the table below.
- **Edges** derived from JSON structure (not from prose).
- **Aliases / availability** stored on the entity's description /
  metadata fields.

#### Entity-kind mapping

| DocC `metadata.symbolKind` (or `roleHeading`) | Callimachus entity `kind` |
|---|---|
| `class`                          | `class`         |
| `struct` / `structure`           | `struct`        |
| `enum` / `enumeration`           | `enum`          |
| `protocol`                       | `protocol`      |
| `instm` / `instance method` / `method` / `func` | `method`        |
| `clm` / `type method`            | `method` (with description noting "type method") |
| `instp` / `instance property` / `property` / `var` | `property`      |
| `init` / `intfcm` / `initializer` | `initializer`  |
| `case` / `enum case` / `enumelt` | `enum_case`     |
| `notification` / `data`-with-NotificationName ref | `notification` |
| `typealias` / `tdef`             | `typealias`     |
| top-level `let` / `var` / `constant` | `constant`  |

The fallback for an unrecognised `symbolKind` is `"docs_topic"` (so
the entity is still indexed; the absence of a kind row in this table
becomes an observable finding rather than a silent drop).

The canonical name for an entity is its full path: for a top-level
type, `NSStackView`; for a child, `NSStackView.alignment` or
`NSStackView.init(views:)`. Use `metadata.title` joined onto the
parent's `pathComponents[0]` for child pages — DocC's
`pathComponents` array gives the dotted chain.

The entity's `description` field carries the rendered Discussion
markdown (use `render.rs` to extract the prose for any
`primaryContentSections[]` whose `kind == "content"`). The
`first_location_uri` / `last_location_uri` carry the same
calli-format URI (`calli://<corpus>/docs/<framework>/<slug>`) the
chunker uses (see Approach §5).

#### Edges

For each DocC page parsed:

1. **`inherits_from` edges.** For each entry in `relationships[]`
   where `type == "inheritsFrom"`, emit:
   `Edge { from: this_entity_id, to: resolve(target_identifier), kind: "inherits_from" }`.
2. **`conforms_to` edges.** For each entry in `relationships[]`
   where `type == "conformsTo"`, emit a `conforms_to` edge similarly.
3. **`references_type` edges.** For each
   `primaryContentSections[].declarations[].tokens[]` with
   `kind == "typeIdentifier"`, resolve the `identifier` to a target
   entity id (via the `references[]` map's `title`) and emit a
   `references_type` edge. De-duplicate per-page so a function with
   `(NSView, NSView) -> NSView` produces one outbound edge to
   `NSView`, not three.
4. **`member_of` edges.** For each top-level page, walk
   `topicSections[].identifiers[]` and emit one `member_of` edge
   *from* each child *to* this page. (Direction: child → parent.
   Matches how the code adapter's "defined_in" edges read.)

**Target resolution.** Edge targets are DocC identifiers like
`doc://com.apple.documentation/documentation/appkit/nsview`. Convert
to a Callimachus entity ID using the same canonical-name rule above:
take the path tail (after `documentation/<framework>/`), uppercase the
first letter to match Swift convention, and consult the local
`references[]` map for the human title where available. The entity
ID itself is the deterministic hash the storage layer already
produces from `(corpus_id, canonical_name, kind)` — the adapter
doesn't compute that hash; it sets `from_entity_id` / `to_entity_id`
to the canonical names and lets the storage upsert resolve them.

**Cross-corpus targets** (e.g. an AppKit method that references a
Foundation type): emit the edge anyway. If the Foundation page lives
in the same corpus (which is the case here — we index AppKit,
Combine, and Foundation into one pinakes), the edge resolves
naturally. If a referenced symbol turns out to be missing (404 in the
fetch), the edge points at a non-existent entity — store-layer
behaviour is to drop or keep depending on whether
`enforce_entity_fk` is on; for our use this is fine either way
(missing-target edges are observable as "to_entity_id not in
entities").

#### Availability metadata

Each DocC page has `metadata.platforms[]` and a top-level
`availability[]` (the schema varies — handle both). For each entry
with name `"macOS"`:
- `introducedAt: "14.0"` → store as `introduced_in_macos = "14.0"`.
- `deprecatedAt: "15.0"` → `deprecated_in_macos = "15.0"`.

In v2 these are recorded as a JSON blob in the entity's `description`
prefix (a one-line header before the prose, e.g.
`**Availability:** macOS 14.0+`), **not** as a separate column. The
storage schema doesn't have first-class availability columns and
adding them is out of scope. Surfacing the data in the description
text means FTS picks it up, which is what consumers actually need.

### 5. Chunker (`chunker.rs`)

For each DocC JSON file discovered:

- One **page-grain** chunk: `content = render::page(&docc)` (full
  rendered markdown including the header, declaration block,
  discussion, and topics).
- One **section-grain** chunk per `primaryContentSections[].kind ==
  "content"` element: `content = render::section(&section)` — just
  the Discussion prose for that section.

Both chunks share the same canonical location URI:
`calli://<corpus>/docs/<framework>/<slug>` for the page chunk;
section chunks append `#<section-anchor>` where `section-anchor` is
the section's `title` lowercased + hyphenated.

The `Chunk.kind` field uses `"page"` and `"section"` to match the
two-level `summary_levels()` the adapter declares.

### 6. Build script — extend `scripts/build-apple-docs-macos-26.sh`

Append a v2 block after the existing v1 block. The whole script
becomes (note: v1 block unchanged from the base branch; v2 block is
new):

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SDK="/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.sdk"

# ── v1 (wiki adapter, top-level types only) — unchanged ─────────────────────
OUTDIR_V1="$ROOT/data/apple-docs-macos-26-src"
PINAKES_V1="$ROOT/data/apple-docs-macos-26.pinakes"
CORPUS_V1="apple-docs-macos-26"

# … existing v1 fetch + corpus add + index commands …

# ── v2 (docs adapter, top-level + child symbols) ────────────────────────────
OUTDIR_V2="$ROOT/data/apple-docs-macos-26-v2-src"
PINAKES_V2="$ROOT/data/apple-docs-macos-26-v2.pinakes"
CORPUS_V2="apple-docs-macos-26-v2"

mkdir -p "$OUTDIR_V2"

python3 "$ROOT/scripts/fetch-apple-docs.py" \
  --framework AppKit \
  --framework Combine \
  --framework Foundation \
  --sdk "$SDK" \
  --target arm64-apple-macos26 \
  --output-dir "$OUTDIR_V2" \
  --format json \
  --depth 2

calli --pinakes "$PINAKES_V2" corpus add docs "$CORPUS_V2" "$OUTDIR_V2" || true
calli --pinakes "$PINAKES_V2" index "$CORPUS_V2" --pass all

echo "done v2: $PINAKES_V2"
```

`chmod +x` stays set (it already is on the base branch).

**Cody must not actually execute either block** — Xcode 26 + 35-minute
network fetch is operator work. Cody runs `bash -n` to syntax-check
the script and stops.

### 7. `.gitignore` additions

The existing v1 patterns already cover `data/apple-docs-macos-*` via
wildcards, but be explicit for v2 source directories:

```
data/apple-docs-macos-*-v2-src/
data/apple-docs-macos-*-v2.pinakes
data/apple-docs-macos-*-v2.pinakes-shm
data/apple-docs-macos-*-v2.pinakes-wal
```

If the existing v1 wildcard `data/apple-docs-macos-*.pinakes` already
matches `apple-docs-macos-26-v2.pinakes` (it does), the new patterns
are belt-and-braces. Keep them so a grep for `v2` in `.gitignore`
finds the right place.

### 8. Smoke test (`tests/adapter_smoke.rs`)

Bundle two or three handwritten DocC JSON fixtures under
`crates/adapters/callimachus-adapter-docs/tests/fixtures/`:

- `nsview.json` — top-level class with two `inheritsFrom` /
  `conformsTo` entries, a `topicSections[]` listing two child
  identifiers, and a `primaryContentSections[]` Discussion block.
- `nsview-tag.json` — child property page (read-only var) for
  `NSView.tag`. Includes an `availability[]` for macOS.
- `nsstackview.json` — top-level class inheriting `NSView`,
  conforming to two protocols, with `references[]` resolving back to
  `NSView`.

The test:

1. Calls `DocsAdapter::discover` on the fixtures directory; asserts
   exactly three sources.
2. For each, calls `chunk` and asserts the chunk count (page +
   N section chunks).
3. Calls `extract_structure` on the page chunk and asserts:
   - The primary entity's `kind` matches the table in §4.
   - `inherits_from`, `conforms_to`, `member_of` edges are present
     with the right `from` / `to`.
   - `references_type` edges are de-duplicated.
   - Availability text appears in the entity description for
     `nsview-tag.json`.

The smoke test runs under `cargo test --workspace` with no LLM. It
does **not** validate end-to-end indexing — that's operator work post-merge.

### 9. Commit and PR

Single commit (or two — one for the adapter crate, one for the script
+ build + docs changes; Cody's call):

```
feat(apple-docs): v2 corpus — dedicated docs adapter with structured edges

Adds crates/adapters/callimachus-adapter-docs reading DocC JSON
directly: rich entity taxonomy (class, struct, method, property,
notification, …), structured edges (inherits_from, conforms_to,
references_type, member_of), per-method pages via --depth 2 fetch.

Extends scripts/fetch-apple-docs.py with --format and --depth flags
(defaults preserve v1 behaviour). Extends the build script with a
parallel v2 block producing apple-docs-macos-26-v2.pinakes alongside
the existing v1 pinakes.

Does not touch the wiki adapter, the MCP surface, or storage schema.
v1 pinakes remain consumable; switch via the .mcp.json corpus name.
```

PR base: `feature/apple-docs-corpus-v1` (PR #35). PR title:
`feat(apple-docs): v2 corpus — dedicated docs adapter`. PR body
links the PRD at
`.claude/features/backlog/prd-documentation-corpus-apple-appkit.md`,
this plan, and PR #35.

## Acceptance criteria

### Cody-verifiable (gate before opening the PR)

- [ ] `cargo build --workspace` succeeds.
- [ ] `cargo test --workspace` passes, including the new
      `adapter_smoke` test.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
      (no new clippy regressions).
- [ ] `calli corpus add docs <name> <empty-dir>` succeeds against a
      throwaway pinakes (proves the adapter is wired into the CLI).
- [ ] `python3 scripts/fetch-apple-docs.py --help` exits 0 and lists
      `--format` and `--depth` alongside the v1 flags.
- [ ] `bash -n scripts/build-apple-docs-macos-26.sh` exits 0.
- [ ] `grep -c "v1\|v2" scripts/build-apple-docs-macos-26.sh` shows
      both blocks present; the v1 block matches what's on
      `feature/apple-docs-corpus-v1` byte-for-byte.
- [ ] `docs/apple-docs-corpus.md` has a new v2 section.
- [ ] `.gitignore` carries the four new v2 patterns.
- [ ] No files under `crates/callimachus-core`, `callimachus-llm`,
      `callimachus-mcp`, `callimachus-http`, or
      `callimachus-adapter-wiki` are modified (apart from the workspace
      `Cargo.toml` membership line).
- [ ] PR description links the PRD, this plan, and PR #35.

### Operator-verifiable (post-merge; requires Xcode 26 and network)

- [ ] `./scripts/build-apple-docs-macos-26.sh` completes and produces
      both `data/apple-docs-macos-26.pinakes` and
      `data/apple-docs-macos-26-v2.pinakes`. Both with WAL/SHM
      sidecars.
- [ ] In the v2 pinakes: `calli inspect entities apple-docs-macos-26-v2`
      shows entities with kinds drawn from the §4 table (at minimum:
      `class`, `protocol`, `method`, `property`).
- [ ] In the v2 pinakes: edge counts for `inherits_from`,
      `conforms_to`, `references_type`, `member_of` are all non-zero
      and the orders of magnitude make sense (e.g. `member_of` should
      vastly outnumber `inherits_from`).
- [ ] MCP `entity` lookup for `NSStackView.alignment` against the v2
      pinakes returns a `property` entity with its own Discussion
      prose (not just a Topics snippet on `NSStackView`).
- [ ] MCP `related` against `NSStackView` returns `NSView` via the
      `inherits_from` edge and child symbols via `member_of`.
- [ ] MCP `search` for `readablePasteboardTypes` against the v2
      pinakes returns the `method` entity (not the parent `NSTextView`
      page).
- [ ] `corpus_overview` and `corpus_themes` work against the v2
      pinakes (themes may or may not be useful; either result is
      acceptable per PRD open question #4 and the v1 finding).
- [ ] An MCP client can register both v1 and v2 pinakes side-by-side
      via the `.mcp.json` pattern from `docs/apple-docs-corpus.md` and
      query both in the same session.

## Out of scope

Explicitly forbidden for this plan. If any of these appear in the
diff, reject the PR:

- **No storage-schema changes.** v2 fits inside the existing entity /
  edge / chunk schema. If Cody finds it doesn't, surface as a finding,
  do not extend the schema.
- **No MCP tool-surface changes.** All v2 queries go through existing
  tools (`search`, `entity`, `related`, `read`, `summarize`,
  `corpus_overview`, `corpus_themes`).
- **No changes to the wiki adapter.** v1 keeps working unchanged.
- **No honest-provenance work.** Use the existing
  `derived_at_version` path the other adapters use. If the rebase
  post-honest-provenance needs an update, that is a follow-up.
- **No cross-corpus query primitives.** `entity_meet` across the
  Nostromo and Apple-docs pinakes is a separate design pass.
- **No scholia tooling.** Schema supports it; v2 doesn't drive it.
- **No additional frameworks beyond AppKit, Combine, Foundation.**
  CoreGraphics, CoreText, SwiftUI, Network — out of scope.
- **No additional macOS versions.** macOS 26 only.
- **No multi-version corpus.** One pinakes per macOS major release
  remains the rule (PRD D2).
- **No theme-quality investigation.** If themes are useless on doc
  prose at v2 scale, file a finding — do not pivot mid-plan.
- **No async / parallel fetching in the Python script** beyond the
  existing serial rate-limited loop. Depth-2 takes ~35 minutes;
  operators are warned, that's enough.
- **No retry/backoff logic** in the fetch script beyond the existing
  log-and-continue policy. Apple's docs site is stable.
- **No automated refresh, watcher, or change-detection.** Manual
  `./scripts/build-apple-docs-macos-26.sh` only.

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "New adapter crate touching SourceAdapter trait, DocC JSON parsing, structured edge extraction, plus script extension and CLI wiring. Multiple files, correctness on edge shape matters."
  redd:
    model: sonnet
    effort: medium
    rationale: "Smoke test with three fixtures is the main test surface; standard adapter-test shape. Medium effort sufficient — no protocol-level invariants like the honest-provenance work."
  marty:
    model: sonnet
    effort: low
    rationale: "downgrade: new crate, low duplication surface against existing adapters; cleanup pass mostly checks imports + module boundaries. Low effort is appropriate for the small refactor surface."
  perri:
    model: sonnet
    effort: medium
    rationale: "Reviewer must confirm zero changes to wiki adapter / MCP / storage, confirm CLI wiring, confirm v1 build block is byte-identical, and sanity-check the edge-kind extraction logic."
```
