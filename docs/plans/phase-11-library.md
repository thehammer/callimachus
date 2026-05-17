# Phase 11 — Library

## Context

Phase 10 introduced the `StorageBackend` trait and the `Library` type (with stub storage
methods). A library is a named collection of corpora that can be searched together and whose
entities can be explicitly linked across corpus boundaries.

The motivating use case: an operator indexes three books in a series — *Xenos*, *Malleus*,
*Hereticus* — as separate corpora. Each has its own entity graph. Eisenhorn appears in all
three under different aliases. A library called "Eisenhorn Trilogy" groups them. The agent can
then ask: "Where do Eisenhorn and Glaw interact across the whole series?" without needing to
issue three separate `entity_meet` calls and manually merge the results.

This phase implements:
1. **Library storage** — the `libraries` and `library_corpora` tables (schema already in
   migration `004_libraries.sql` from Phase 10).
2. **Library CLI** — `calli library add|list|add-corpus|remove-corpus|status`.
3. **Library MCP tools** — 4 new tools added to the MCP server.
4. **Cross-corpus entity linking** — a new correction kind `entity_link` that declares
   two entities in different corpora to be the same person/place/thing.
5. **Library-scoped query methods** — fan-out search and entity resolution across all
   corpora in a library.

Reference: `docs/plans/callimachus-standalone.md §10.14`, Phase 10 storage abstraction.

## Target

- **Repo:** `callimachus`
- **Branch:** `feat/phase-11-library`
- **Base:** `main`

## Files to change

---

### `crates/callimachus-core/src/storage/library_store.rs` (new)

Implement the library storage methods from `StorageBackend` in the SQLite backend.

```rust
pub fn insert(db: &Database, library: &Library) -> Result<()>
pub fn list(db: &Database) -> Result<Vec<Library>>
pub fn get(db: &Database, id: &str) -> Result<Option<Library>>
pub fn require(db: &Database, id: &str) -> Result<Library>  // CalError::NotFound if missing
pub fn add_corpus(db: &Database, library_id: &str, corpus_id: &str) -> Result<()>
pub fn remove_corpus(db: &Database, library_id: &str, corpus_id: &str) -> Result<()>
pub fn delete(db: &Database, id: &str) -> Result<bool>
pub fn corpus_ids(db: &Database, library_id: &str) -> Result<Vec<String>>
```

`list` JOIN with `library_corpora` to populate `Library.corpus_ids`.

Wire these into `SqliteBackend` (the delegation wrapper from Phase 10).

---

### `crates/callimachus-core/src/corrections/types.rs`

Add a new `CorrectionKind` variant:

```rust
EntityLink {
    /// Entity in corpus A.
    corpus_a_id: String,
    entity_a_id: String,
    /// Entity in corpus B (may be in a different corpus).
    corpus_b_id: String,
    entity_b_id: String,
    /// Human-readable reason.
    note: Option<String>,
},
```

`EntityLink` corrections are stored in the `corrections` table like any other correction, but
scoped to the library rather than a single corpus. The `corpus_id` field on the correction row
stores the **library** id (prefixed `lib:`) to distinguish from corpus corrections. This avoids
a schema change.

`CorrectionsEngine::load_for_library(backend, library_id)` loads all `EntityLink` corrections
whose `corpus_id` starts with `lib:<library_id>`.

---

### `crates/callimachus-core/src/query/library_service.rs` (new)

`LibraryService` is the cross-corpus query layer. It wraps the existing `QueryService` and
fans out calls across all corpora in a library.

```rust
pub struct LibraryService {
    backend: Arc<dyn StorageBackend>,
    /// One QueryService per corpus in the library.
    corpus_services: HashMap<String, QueryService>,
    /// Cross-corpus entity links loaded from corrections.
    links: EntityLinkIndex,
}

impl LibraryService {
    pub fn load(backend: Arc<dyn StorageBackend>, library_id: &str) -> Result<Self> { ... }

    /// Fan-out search across all corpora in the library. Merge and re-rank results.
    pub fn library_search(&self, input: LibrarySearchInput) -> ToolResult<LibrarySearchOutput>

    /// Return a corpus-agnostic overview of the library.
    pub fn library_overview(&self, input: LibraryOverviewInput) -> ToolResult<LibraryOverviewOutput>

    /// Resolve an entity name across all corpora. Returns all matches with corpus labels.
    pub fn library_entity_resolve(&self, input: LibraryEntityResolveInput) -> ToolResult<LibraryEntityResolveOutput>

    /// Find all co-occurrences of two entities across all corpora in the library.
    pub fn library_entity_meet(&self, input: LibraryEntityMeetInput) -> ToolResult<LibraryEntityMeetOutput>
}
```

#### `EntityLinkIndex`

An in-memory structure built from `EntityLink` corrections. Provides:

```rust
/// Given (corpus_id, entity_id), return all linked (corpus_id, entity_id) pairs.
pub fn linked_entities(&self, corpus_id: &str, entity_id: &str) -> Vec<(String, String)>

/// Given an entity name, return all (corpus_id, entity_id) pairs across the library
/// that this name might refer to (direct matches + link-resolved matches).
pub fn resolve_name(&self, name: &str, all_entities: &[(String, Entity)]) -> Vec<(String, Entity)>
```

---

### Query types (`src/query/types.rs`)

Add library-scoped input/output types:

```rust
pub struct LibrarySearchInput {
    pub library_id: String,
    pub query: String,
    pub mode: SearchMode,
    pub limit: Option<u32>,
}

pub struct LibrarySearchOutput {
    pub results: Vec<LibrarySearchResult>,
}

pub struct LibrarySearchResult {
    pub corpus_id: String,
    pub corpus_name: String,
    pub location: Location,
    pub snippet: String,
    pub relevance: f32,
}

pub struct LibraryOverviewInput { pub library_id: String }
pub struct LibraryOverviewOutput {
    pub library: Library,
    pub corpora: Vec<CorpusOverviewOutput>,
    pub total_chunks: u64,
    pub total_entities: u64,
    pub cross_corpus_links: u64,
}

pub struct LibraryEntityResolveInput {
    pub library_id: String,
    pub name: String,
}
pub struct LibraryEntityResolveOutput {
    pub matches: Vec<LibraryEntityMatch>,
}
pub struct LibraryEntityMatch {
    pub corpus_id: String,
    pub corpus_name: String,
    pub entity: Entity,
    pub linked_to: Vec<(String, String)>,  // (corpus_id, entity_id) of linked entities
}

pub struct LibraryEntityMeetInput {
    pub library_id: String,
    pub entity_a: String,   // name or id (resolved across all corpora)
    pub entity_b: String,
}
pub struct LibraryEntityMeetOutput {
    /// First co-occurrence across the entire library (may be in different corpora
    /// for linked entities). Ordered by location_uri lexicographically per corpus,
    /// then by corpus order within the library.
    pub first_co_occurrence: Option<LibraryLocation>,
    pub all: Vec<LibraryLocation>,
    pub count: u64,
}
pub struct LibraryLocation {
    pub corpus_id: String,
    pub corpus_name: String,
    pub location: Location,
}
```

---

### `crates/callimachus-mcp/src/tools.rs`

Add 4 new tool entries to the tool registry:

| Tool | Description |
|------|-------------|
| `library_list` | List all libraries with corpus counts |
| `library_overview` | Overview of a library: member corpora, entity counts, cross-corpus links |
| `library_search` | Keyword/semantic search across all corpora in a library |
| `library_entity_resolve` | Resolve an entity name across all corpora in a library |
| `library_entity_meet` | Find all co-occurrences of two entities across the library |

Total MCP tools: 12 (existing) + 5 (library) = **17 tools**.

---

### `crates/callimachus-mcp/src/dispatch.rs`

Add dispatch cases for the 5 new library tools. Each deserializes into the appropriate
`Library*Input` type and calls `LibraryService`.

`McpServer` now holds both a `QueryService` (per-corpus) and a `LibraryService` (cross-corpus).
Both are constructed at startup from the same `StorageBackend`.

---

### `crates/callimachus-cli` — `calli library`

#### `src/commands/library.rs` (new)

```rust
pub enum LibrarySubcommand {
    Add { name: String },
    List,
    Status { library_id: String },
    AddCorpus { library_id: String, corpus_id: String },
    RemoveCorpus { library_id: String, corpus_id: String },
    Remove { library_id: String },
}

pub fn run(sub: LibrarySubcommand, backend: &dyn StorageBackend) -> anyhow::Result<()>
```

**`library add <name>`**: Generate a slug id from the name (`slugify(name)`). Insert via
`library_store::insert`. Print: `✓ Library 'eisenhorn-trilogy' created (id: eisenhorn-trilogy)`.

**`library list`**: Print table:
```
ID                   NAME                  CORPORA  CHUNKS    ENTITIES
eisenhorn-trilogy    Eisenhorn Trilogy     3        1247      892
```

**`library status <id>`**: Print each member corpus with its `calli corpus status` summary,
plus cross-corpus entity link count.

**`library add-corpus <lib_id> <corpus_id>`**: Validate both exist, call
`library_store::add_corpus`. Print confirmation.

**`library remove-corpus <lib_id> <corpus_id>`**: Remove without deleting the corpus itself.

**`library remove <lib_id>`**: Remove the library record (not the member corpora).

Wire through `src/main.rs` → `Command::Library`. Remove `not_yet_plain("library")` if present.

---

### `calli correct` — `entity-link` subcommand

Add to `src/commands/correct.rs`:

```rust
CorrectSubcommand::EntityLink {
    library_id: String,
    corpus_a: String,
    entity_a: String,
    corpus_b: String,
    entity_b: String,
    note: Option<String>,
}
```

Usage:
```
calli correct --library eisenhorn-trilogy entity-link \
  --corpus-a xenos --entity-a eisenhorn \
  --corpus-b malleus --entity-b eisenhorn
```

Validates that both entities exist in their respective corpora. Builds a `Correction` with
`kind = EntityLink`, `corpus_id = "lib:eisenhorn-trilogy"`. Prints confirmation.

---

### `calli inspect` — `library-links` subcommand

Add to `src/commands/inspect.rs`:

```rust
InspectSubcommand::LibraryLinks { library_id: String }
```

Loads all `EntityLink` corrections for the library. Prints:
```
ENTITY A                         ENTITY B                         NOTE
xenos / Eisenhorn (def456)  ↔   malleus / Eisenhorn (ghi789)
xenos / Bequin (abc123)     ↔   malleus / Beta (jkl012)         same person, different alias
```

---

## Approach

1. Implement `library_store.rs` and wire into `SqliteBackend`. Run `cargo test` — migration
   `004_libraries.sql` already present; storage tests pass.
2. Add `Library` CLI (`calli library add|list|status|add-corpus|remove-corpus|remove`). All
   commands work against empty library.
3. Add `EntityLink` correction kind to `corrections/types.rs`. Update `CorrectionsEngine` to
   load library corrections. Add `calli correct entity-link` subcommand.
4. Implement `EntityLinkIndex`. Unit-test link resolution with fixture data.
5. Implement `LibraryService` with all 5 query methods (fan-out search, entity resolve, entity
   meet, overview). Integrate into `McpServer`.
6. Add 5 library tools to MCP tool registry and dispatch table.
7. Add `calli inspect library-links` subcommand.
8. Write tests (see below).
9. `cargo test --all` passes. Manual smoke test (see acceptance criteria).

## Tests

### `callimachus-core/library_store`

- Insert library, list → returns it with correct corpus_ids.
- `add_corpus` / `remove_corpus` → corpus_ids update correctly.
- `require` on missing id → `CalError::NotFound`.
- `delete` → cascade deletes `library_corpora` rows.

### `callimachus-core/EntityLinkIndex`

- Seed two entities (different corpora) with an `EntityLink` correction.
- `linked_entities("xenos", "abc")` returns `[("malleus", "def")]`.
- `resolve_name("Eisenhorn", all_entities)` returns both corpus matches.
- No link → `linked_entities` returns empty vec.

### `callimachus-core/LibraryService`

- Seed library with 2 corpora, 3 chunks each, FTS content.
- `library_search`: query matching chunks in both corpora → returns results from both,
  ordered by relevance.
- `library_entity_resolve`: entity present in both corpora → returns 2 matches.
- `library_entity_meet`: entity in corpus A meets entity in corpus B via a link correction →
  all co-occurrences returned.
- `library_overview`: correct total_chunks (sum), total_entities (sum), cross_corpus_links.

### `callimachus-mcp/library tools`

- `tools/list` returns 17 tools (12 + 5 new).
- `library_list` on empty DB returns `[]`.
- `library_search` with unknown library_id returns not-found error.
- `library_entity_resolve` with no matches returns empty matches list.

### `callimachus-cli/library`

- `library add` creates a library and prints the id.
- `library add-corpus` with invalid corpus_id returns clear error.
- `library remove` deletes the library but not the member corpora.

## Acceptance criteria

- `calli library add "Eisenhorn Trilogy"` creates a library
- `calli library add-corpus eisenhorn-trilogy xenos` (and malleus, hereticus) adds corpora
- `calli mcp` + `library_list` returns the library with corpus count
- `calli mcp` + `library_search(library_id="eisenhorn-trilogy", query="Bequin")` returns
  results from all three corpora
- `calli correct --library eisenhorn-trilogy entity-link ...` records a cross-corpus link
- `calli mcp` + `library_entity_meet` respects cross-corpus entity links
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes

## Out of scope

- No library-level summarization (no "summarize the whole trilogy" LLM call)
- No library-level corrections other than `entity_link`
- No library export format
- No HTTP endpoints for library tools (Phase 8 HTTP server is already done; library HTTP
  routes are a follow-on)
- No UI for managing libraries
- Cross-corpus entity linking does not retroactively merge entity graphs in storage — links
  are resolved at query time only (same as all corrections)

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "Fan-out query service, EntityLinkIndex, 5 new MCP tools, new CLI command group, new correction kind — significant new surface area but well-specified."
  redd:
    model: sonnet
    effort: high
    rationale: "Cross-corpus correctness is subtle — entity link resolution, fan-out merge ordering, and empty-library edge cases all need explicit coverage."
  marty:
    model: sonnet
    effort: medium
    rationale: "Fan-out pattern in LibraryService parallels QueryService — shared result-merging helpers worth extracting."
  perri:
    model: sonnet
    effort: high
    rationale: "Library tools are the highest-level API surface; entity link semantics must be precisely reviewed — wrong resolution silently corrupts cross-corpus analysis."
no_pr: true
```
