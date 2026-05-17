# Phase 11 — Collection

## Context

Callimachus is a Rust-based, local-first tool that indexes corpora (books, codebases, wikis) into SQLite and exposes them as LLM tools via MCP. Phases 1–9 built the indexing pipeline, query service, MCP server, corrections overlay, book/code adapters, CLI, and HTTP server. Phase 10 introduces a `StorageBackend` trait and a stub `Library` type with migration `004_libraries.sql` to make storage swappable.

This phase introduces **Collection** — a named, *recursive* group of corpora and/or other collections that can be searched together and whose entities can be explicitly linked across corpus boundaries.

Naming note: Phase 10's draft plan called this concept `Library`. That implies books. The actual concept is broader — a Collection can contain code corpora, wikis, book series, and other collections. **This phase renames `Library` → `Collection` everywhere Phase 10 stubbed it, and replaces Phase 10's flat `library_corpora` table with a recursive `collection_members` table.** Phase 10 has not shipped to `main` at the time Phase 11 runs — its migrations land first, then this phase rebases on top of them.

Motivating use cases:

- **Series**: `xenos`, `malleus`, `hereticus` corpora → Collection `eisenhorn-trilogy` (kind=`series`).
- **Catalog**: `eisenhorn-trilogy` + `ravenor` + standalone novels → Collection `black-library-40k` (kind=`catalog`).
- **Domain**: `black-library-40k` + other publishers → Collection `fiction` (kind=`domain`).
- **Cross-type**: `gof-design-patterns` book corpus + `my-app` code corpus → Collection `design-patterns-in-practice`. An agent can declare "Gang-of-Four Observer Pattern" `Implements` `EventEmitter` via a typed entity link.

This phase implements:
1. **Collection storage** — `collections` and `collection_members` tables; `member_type` of `"corpus"` or `"collection"`; recursive membership.
2. **Collection CLI** — `calli collection add|list|status|add-member|remove-member|remove`.
3. **Collection MCP tools** — 5 new tools.
4. **Cross-corpus entity linking** — a new `CorrectionKind::EntityLink` with a typed `kind` field (`SameAs`, `Implements`, `Exemplifies`, `References`, `Contrasts`).
5. **Collection-scoped corrections** — a new nullable `collection_id` column on `corrections` so collection-scoped corrections live cleanly alongside corpus-scoped ones.
6. **Collection-scoped query methods** — fan-out search and entity resolution across all corpora reachable from a collection (transitively expanding sub-collections).

Reference: `docs/plans/callimachus-standalone.md §10.14`, Phase 10 storage abstraction.

## Target

- **Repo:** `callimachus` (at `/Users/hammer/Software Development/Open Source/callimachus`)
- **Branch:** `feat/phase-11-collection`
- **Base:** `main`
- **Isolation:** main-dir (no remote yet)
- **PR:** no_pr (set in suggested_config below)

## Codebase layout (as of Phase 10 landing)

```
crates/
  callimachus-core/
    migrations/
      001_initial.sql
      002_fts.sql
      003_*.sql         # whatever Phase 9 added (if any)
      004_libraries.sql # Phase 10 — Library stub
    src/
      corrections/
        engine.rs
        mod.rs
        types.rs        # CorrectionKind enum lives here
      query/
        mod.rs
        search.rs
        service.rs      # QueryService
        types.rs
      storage/
        chunk_store.rs
        corpus_store.rs
        correction_store.rs
        db.rs
        edge_store.rs
        entity_store.rs
        fts.rs
        mod.rs
        run_log.rs
        summary_store.rs
        # Phase 10 will add: backend.rs (StorageBackend trait), sqlite_backend.rs,
        # library_store.rs (stub), and a Library type. Phase 11 renames these.
      types/...
  callimachus-mcp/
    src/
      dispatch.rs
      lib.rs
      server.rs
      tools.rs
  callimachus-cli/
    src/
      commands/
        corpus.rs corrupt.rs export.rs index.rs inspect.rs mcp.rs
        mod.rs reindex.rs serve.rs watch.rs correct.rs
```

If Phase 10 produces slightly different filenames (e.g. `library_store.rs` vs. `libraries.rs`), rename to the `collection_*` equivalents as part of this phase.

## Files to change

### Migration: `crates/callimachus-core/migrations/005_collections.sql` (new)

This migration:
1. Drops Phase 10's `library_corpora` table and the `libraries` table (Phase 10 only stubs storage; no production data exists yet).
2. Creates `collections` and `collection_members`.
3. Adds a nullable `collection_id` column to `corrections`.

```sql
-- Replace Phase 10's library scaffolding with the recursive Collection model.

DROP TABLE IF EXISTS library_corpora;
DROP TABLE IF EXISTS libraries;

CREATE TABLE collections (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL,        -- 'series' | 'catalog' | 'domain' | 'workspace' | free-form
    created_at   TEXT NOT NULL          -- ISO 8601
);

CREATE TABLE collection_members (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    member_id     TEXT NOT NULL,
    member_type   TEXT NOT NULL CHECK (member_type IN ('corpus', 'collection')),
    added_at      TEXT NOT NULL,
    PRIMARY KEY (collection_id, member_id, member_type)
);

CREATE INDEX collection_members_member_idx
    ON collection_members (member_id, member_type);

-- Corrections gain an optional collection scope. corpus_id remains required for
-- corpus-scoped corrections; collection_id is set instead for collection-scoped
-- corrections (currently: EntityLink).
ALTER TABLE corrections ADD COLUMN collection_id TEXT
    REFERENCES collections(id) ON DELETE CASCADE;

CREATE INDEX corrections_collection_idx ON corrections (collection_id);
```

Note: SQLite does not allow `ALTER TABLE ... DROP CONSTRAINT`. The existing `corrections.corpus_id NOT NULL` constraint must be relaxed to support collection-scoped rows. If the current schema declares `corpus_id NOT NULL`, this migration must rebuild the table (`CREATE TABLE corrections_new ... INSERT INTO corrections_new SELECT ... DROP TABLE corrections; ALTER TABLE corrections_new RENAME TO corrections;`) with `corpus_id TEXT NULL` and a `CHECK ((corpus_id IS NOT NULL) OR (collection_id IS NOT NULL))` constraint. Inspect `migrations/001_initial.sql` first to confirm the corrections schema and write the appropriate rebuild block.

---

### Domain type: `crates/callimachus-core/src/types/collection.rs` (new, or rename from Phase 10's `library.rs`)

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Series,
    Catalog,
    Domain,
    Workspace,
    Other(String),
}

impl CollectionKind {
    pub fn as_str(&self) -> &str {
        match self {
            CollectionKind::Series => "series",
            CollectionKind::Catalog => "catalog",
            CollectionKind::Domain => "domain",
            CollectionKind::Workspace => "workspace",
            CollectionKind::Other(s) => s.as_str(),
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "series" => Self::Series,
            "catalog" => Self::Catalog,
            "domain" => Self::Domain,
            "workspace" => Self::Workspace,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberType { Corpus, Collection }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionMember {
    pub member_id: String,
    pub member_type: MemberType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub kind: CollectionKind,
    pub created_at: String,
    pub members: Vec<CollectionMember>,
}
```

Export from `types/mod.rs`. Remove or alias the old `Library` type from Phase 10 so nothing references it.

---

### Storage: `crates/callimachus-core/src/storage/collection_store.rs` (new, replaces Phase 10's `library_store.rs`)

```rust
pub fn insert(db: &Database, collection: &Collection) -> Result<()>;
pub fn list(db: &Database) -> Result<Vec<Collection>>;
pub fn get(db: &Database, id: &str) -> Result<Option<Collection>>;
pub fn require(db: &Database, id: &str) -> Result<Collection>; // CalError::NotFound if missing
pub fn add_member(db: &Database, collection_id: &str, member_id: &str, member_type: MemberType) -> Result<()>;
pub fn remove_member(db: &Database, collection_id: &str, member_id: &str, member_type: MemberType) -> Result<()>;
pub fn delete(db: &Database, id: &str) -> Result<bool>;
pub fn direct_members(db: &Database, collection_id: &str) -> Result<Vec<CollectionMember>>;

/// Recursively resolve to the flat set of corpus ids reachable from `collection_id`,
/// expanding nested collections. Detects and breaks cycles (logs warn, drops the back-edge).
/// Returns deterministic ordering: DFS, alphabetical by member_id at each level.
pub fn resolve_corpus_ids(db: &Database, collection_id: &str) -> Result<Vec<String>>;
```

Update `StorageBackend` trait (added in Phase 10) so its `library_*` methods become `collection_*` methods with these signatures. Wire into the SQLite backend struct.

---

### Corrections: `crates/callimachus-core/src/corrections/types.rs`

Add the new variant and supporting enum. The `corpus_id` field on `Correction` becomes `Option<String>`; add `collection_id: Option<String>`. Exactly one must be `Some`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub id: String,
    pub corpus_id: Option<String>,       // Some for corpus-scoped corrections
    pub collection_id: Option<String>,   // Some for collection-scoped corrections (EntityLink)
    pub kind: CorrectionKind,
    pub applied_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CorrectionKind {
    Merge { /* unchanged */ },
    Unmerge { /* unchanged */ },
    Rename { /* unchanged */ },
    Alias { /* unchanged */ },
    EditSummary { /* unchanged */ },
    EntityLink {
        /// Corpus containing entity A.
        corpus_a_id: String,
        entity_a_id: String,
        /// Corpus containing entity B (may differ from corpus_a_id).
        corpus_b_id: String,
        entity_b_id: String,
        /// Semantic relationship between A and B.
        kind: EntityLinkKind,
        /// Free-text human note (optional).
        note: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntityLinkKind {
    /// Same person/place/thing across corpora (e.g. Eisenhorn in Xenos = Eisenhorn in Malleus).
    SameAs,
    /// Entity B implements the pattern/concept of entity A (e.g. EventEmitter implements Observer).
    Implements,
    /// Entity B is a concrete example of the abstract concept entity A.
    Exemplifies,
    /// Entity B explicitly references entity A.
    References,
    /// Entity B contrasts with entity A.
    Contrasts,
}

impl CorrectionKind {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Merge { .. } => "merge",
            Self::Unmerge { .. } => "unmerge",
            Self::Rename { .. } => "rename",
            Self::Alias { .. } => "alias",
            Self::EditSummary { .. } => "edit_summary",
            Self::EntityLink { .. } => "entity_link",
        }
    }
}
```

Update `correction_store.rs`:
- Persist `collection_id` (nullable column from migration 005).
- `insert` accepts the updated `Correction` struct; enforces XOR (`corpus_id.is_some() ^ collection_id.is_some()`).
- Add `pub fn list_for_collection(db: &Database, collection_id: &str) -> Result<Vec<Correction>>`.
- Keep existing `list_for_corpus`.

Update `corrections/engine.rs`:
- `CorrectionsEngine::load_for_corpus` remains; ignores `EntityLink` records (they never have a corpus_id).
- Add `CorrectionsEngine::load_for_collection(backend, collection_id) -> Result<Vec<Correction>>` returning the raw correction list (the EntityLinkIndex below consumes it; the engine itself does not apply EntityLink to corpus-level views).

**Correction construction sites to update (corpus_id: String → Option<String>):**

1. `crates/callimachus-core/src/storage/correction_store.rs` — the `row_to_correction` mapper function: change `corpus_id: row.get(1)?` to `corpus_id: row.get::<_, Option<String>>(1)?` and add `collection_id: row.get::<_, Option<String>>(N)?` where N is the new column index.

2. `crates/callimachus-cli/src/commands/correct.rs` — every subcommand that constructs a `Correction { ... }`: add `corpus_id: Some(corpus_id.to_string()), collection_id: None` for all existing (corpus-scoped) kinds. The new EntityLink path uses `corpus_id: None, collection_id: Some(collection_id.to_string())`.

3. `crates/callimachus-core/src/corrections/engine.rs` — any `#[cfg(test)]` Correction fixtures: add `corpus_id: Some("test-corpus".to_string()), collection_id: None` to all existing test Correction structs.

---

### Query: `crates/callimachus-core/src/query/collection_service.rs` (new)

```rust
pub struct CollectionService {
    backend: Arc<dyn StorageBackend>,
    collection: Collection,
    /// One QueryService per resolved leaf corpus.
    corpus_services: HashMap<String, QueryService>,
    /// Cross-corpus entity links loaded from collection-scoped corrections.
    links: EntityLinkIndex,
}

impl CollectionService {
    pub fn load(backend: Arc<dyn StorageBackend>, collection_id: &str) -> Result<Self>;

    pub fn collection_search(&self, input: CollectionSearchInput) -> ToolResult<CollectionSearchOutput>;
    pub fn collection_overview(&self, input: CollectionOverviewInput) -> ToolResult<CollectionOverviewOutput>;
    pub fn collection_entity_resolve(&self, input: CollectionEntityResolveInput) -> ToolResult<CollectionEntityResolveOutput>;
    pub fn collection_entity_meet(&self, input: CollectionEntityMeetInput) -> ToolResult<CollectionEntityMeetOutput>;
}
```

Resolution: `CollectionService::load` calls `collection_store::resolve_corpus_ids` to flatten nested collections to a deterministic list of leaf corpora, then opens one `QueryService` per corpus.

#### `EntityLinkIndex`

```rust
/// In-memory index built from EntityLink corrections for one collection.
pub struct EntityLinkIndex { /* ... */ }

impl EntityLinkIndex {
    pub fn from_corrections(corrections: &[Correction]) -> Self;

    /// All (corpus_id, entity_id) directly linked to the given pair, filtered by kind.
    /// If `kinds` is empty, returns all kinds.
    pub fn linked(&self, corpus_id: &str, entity_id: &str, kinds: &[EntityLinkKind])
        -> Vec<(String, String, EntityLinkKind)>;

    /// Transitive closure over `SameAs` links only — for identity resolution.
    /// Returns the equivalence class containing the given entity.
    pub fn same_as_class(&self, corpus_id: &str, entity_id: &str)
        -> HashSet<(String, String)>;

    /// Resolve a name across the collection: direct entity-name matches in any corpus,
    /// plus everything reachable from those via `SameAs` links.
    pub fn resolve_name(&self, name: &str, all_entities: &[(String, Entity)])
        -> Vec<(String, Entity)>;
}
```

Identity rule: only `SameAs` folds entities together for `entity_resolve` / `entity_meet`. Other kinds (`Implements`, `Exemplifies`, `References`, `Contrasts`) surface as **related** results, not equivalence — they appear in `entity_resolve` output as a separate `related` list, never as direct matches.

---

### Query types: `crates/callimachus-core/src/query/types.rs`

Add (replacing the Phase 10 stub `Library*` types if any):

```rust
pub struct CollectionListInput {}

pub struct CollectionListOutput {
    pub collections: Vec<CollectionListEntry>,
}

pub struct CollectionListEntry {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub member_count: u64,
    pub corpus_count: u64,   // resolved leaf corpora, including nested
}

pub struct CollectionSearchInput {
    pub collection_id: String,
    pub query: String,
    pub mode: SearchMode,
    pub limit: Option<u32>,
}
pub struct CollectionSearchOutput { pub results: Vec<CollectionSearchResult> }
pub struct CollectionSearchResult {
    pub corpus_id: String,
    pub corpus_name: String,
    pub location: Location,
    pub snippet: String,
    pub relevance: f32,
}

pub struct CollectionOverviewInput { pub collection_id: String }
pub struct CollectionOverviewOutput {
    pub collection: Collection,
    pub corpora: Vec<CorpusOverviewOutput>,
    pub nested_collections: Vec<Collection>, // direct children of type=collection
    pub total_chunks: u64,
    pub total_entities: u64,
    pub cross_corpus_links_by_kind: BTreeMap<String, u64>, // "same_as" → 12, "implements" → 3, …
}

pub struct CollectionEntityResolveInput { pub collection_id: String, pub name: String }
pub struct CollectionEntityResolveOutput {
    pub matches: Vec<CollectionEntityMatch>,
    pub related: Vec<CollectionEntityRelation>,
}
pub struct CollectionEntityMatch {
    pub corpus_id: String,
    pub corpus_name: String,
    pub entity: Entity,
    pub same_as: Vec<(String, String)>, // (corpus_id, entity_id) in SameAs class
}
pub struct CollectionEntityRelation {
    pub from: (String, String), // (corpus_id, entity_id)
    pub to:   (String, String),
    pub kind: EntityLinkKind,
    pub note: Option<String>,
}

pub struct CollectionEntityMeetInput {
    pub collection_id: String,
    pub entity_a: String, // name or id, resolved via SameAs across the collection
    pub entity_b: String,
}
pub struct CollectionEntityMeetOutput {
    pub first_co_occurrence: Option<CollectionLocation>,
    pub all: Vec<CollectionLocation>,
    pub count: u64,
}
pub struct CollectionLocation {
    pub corpus_id: String,
    pub corpus_name: String,
    pub location: Location,
}
```

Merge & ordering for `collection_search`: rerank by `(relevance DESC, corpus_id ASC, location_uri ASC)` for determinism. `collection_entity_meet` orders by `(corpus_order_in_collection, location_uri)` where `corpus_order_in_collection` is the index returned by `resolve_corpus_ids`.

---

### MCP: `crates/callimachus-mcp/src/tools.rs` + `dispatch.rs`

Add 5 tools to the registry:

| Tool | Description |
|------|-------------|
| `collection_list` | List all collections with member counts and kinds |
| `collection_overview` | Member corpora/collections, entity counts, link counts by kind |
| `collection_search` | Keyword/semantic search across all corpora reachable from the collection |
| `collection_entity_resolve` | Resolve a name across the collection (matches + related via typed links) |
| `collection_entity_meet` | Find all co-occurrences of two entities across the collection (SameAs-aware) |

`McpServer` constructs `CollectionService` lazily per request (one collection at a time) since the resolved corpus set may change between calls. Cache by `collection_id` with invalidation on any insert into `collections` / `collection_members` / collection-scoped corrections (cheap: a u64 generation counter on the backend bumped on writes).

Existing per-corpus tool count: 12. New: 5. **Total MCP tools after Phase 11: 17.**

---

### CLI: `crates/callimachus-cli/src/commands/collection.rs` (new)

```rust
pub enum CollectionSubcommand {
    Add { name: String, kind: Option<String> },         // default kind: "series"
    List,
    Status { collection_id: String },
    AddMember { collection_id: String, member: String, as_collection: bool },
    RemoveMember { collection_id: String, member: String, as_collection: bool },
    Remove { collection_id: String },
}

pub fn run(sub: CollectionSubcommand, backend: &dyn StorageBackend) -> anyhow::Result<()>;
```

- `collection add <name> [--kind <kind>]`: slugify name → id; insert. Print `✓ Collection 'eisenhorn-trilogy' (kind: series) created`.
- `collection list`: tabular `ID | NAME | KIND | MEMBERS | CHUNKS | ENTITIES`. `CHUNKS`/`ENTITIES` sum across resolved leaf corpora.
- `collection status <id>`: print the resolved corpus tree (indented for nested collections), each corpus's `calli corpus status` summary, plus link counts grouped by `EntityLinkKind`.
- `collection add-member <coll_id> <member_id> [--collection]`: defaults to `member_type=corpus`; `--collection` flips to nested collection. Validates target exists.
- `collection remove-member <coll_id> <member_id> [--collection]`: removes the membership row, leaves the member itself intact.
- `collection remove <coll_id>`: deletes the collection (members and entity_link corrections scoped to it cascade via FK).

Wire into `commands/mod.rs` and `main.rs` (`Command::Collection`). Drop any `not_yet_plain("library" | "collection")` placeholder.

---

### CLI: `calli correct entity-link` subcommand

Add to `crates/callimachus-cli/src/commands/correct.rs`:

```rust
CorrectSubcommand::EntityLink {
    collection_id: String,
    corpus_a: String,
    entity_a: String,
    corpus_b: String,
    entity_b: String,
    kind: String,            // parsed into EntityLinkKind
    note: Option<String>,
}
```

Usage:
```
calli correct entity-link \
  --collection design-patterns-in-practice \
  --corpus-a gof-design-patterns --entity-a observer-pattern \
  --corpus-b my-app --entity-b event-emitter \
  --kind implements \
  --note "EventEmitter is a concrete Observer implementation"
```

Validates:
1. Collection exists.
2. Both corpora are reachable from the collection via `resolve_corpus_ids`.
3. Both entities exist in their respective corpora.
4. `--kind` parses into a known `EntityLinkKind` (`same_as | implements | exemplifies | references | contrasts`).

Persists with `collection_id = Some(...)`, `corpus_id = None`. Prints `✓ EntityLink (implements) recorded: gof-design-patterns/observer-pattern → my-app/event-emitter`.

The existing `correct` runner signature takes a `corpus_id` first arg; refactor `correct::run` so corpus-scoped subcommands take a `corpus_id` and `EntityLink` takes a `collection_id`. Use a top-level enum split (e.g. parse a `--collection` flag at the `calli correct` level that dispatches to the new path).

---

### CLI: `calli inspect collection-links` subcommand

Add to `crates/callimachus-cli/src/commands/inspect.rs`:

```rust
InspectSubcommand::CollectionLinks { collection_id: String, kind: Option<String> }
```

Loads `EntityLink` corrections for the collection (optionally filtered by `--kind`). Prints:
```
KIND         FROM                                       TO                                          NOTE
same_as      xenos / Eisenhorn (def456)               ↔ malleus / Eisenhorn (ghi789)
implements   gof-design-patterns / Observer (abc)     → my-app / EventEmitter (xyz)               EventEmitter is a concrete Observer impl
```

---

## Approach

1. **Migration & types**: Write `005_collections.sql`. Inspect `001_initial.sql` first to confirm whether `corrections.corpus_id` is `NOT NULL` (rebuild the table if so). Add `Collection`, `CollectionKind`, `CollectionMember`, `MemberType` types. Delete or rename Phase 10's `Library` type and its storage stub.
2. **Storage**: Implement `collection_store` (insert/list/get/require/add_member/remove_member/delete/direct_members/resolve_corpus_ids). Update `correction_store` for the new `collection_id` column and the XOR invariant. Update `StorageBackend` trait — rename `library_*` → `collection_*`. Run `cargo test -p callimachus-core` until storage tests pass.
3. **Corrections**: Add `EntityLink` variant + `EntityLinkKind` enum to `corrections/types.rs`. Update `Correction` struct's `corpus_id` → `Option<String>` and add `collection_id: Option<String>`. Fix all compile sites. Add `CorrectionsEngine::load_for_collection`.
4. **EntityLinkIndex**: Implement in `query/collection_service.rs` (or a sibling module). Unit-test `linked`, `same_as_class` (including transitivity), and `resolve_name`.
5. **CollectionService**: Implement `load` (resolve corpora, open one QueryService each, build EntityLinkIndex). Implement `collection_search`, `collection_overview`, `collection_entity_resolve`, `collection_entity_meet`. Use deterministic ordering as specified.
6. **MCP**: Add 5 tools to `tools.rs` registry with input/output JSON schemas derived from the types. Add dispatch arms in `dispatch.rs`. Confirm `tools/list` returns 17 entries. The `collection_list` dispatch arm deserializes `CollectionListInput` (empty struct, default-deserializes from any JSON object or null).
7. **CLI — collection**: Add `commands/collection.rs` with all 6 subcommands. Wire into `commands/mod.rs` and `main.rs`.
8. **CLI — correct entity-link**: Extend `commands/correct.rs` with the entity-link path and `--collection` flag handling.
9. **CLI — inspect collection-links**: Add subcommand to `commands/inspect.rs`.
10. **Tests** (see below) and a manual smoke run.
11. `cargo test --all` + `cargo clippy --all -- -D warnings` clean.

## Tests

### `callimachus-core::storage::collection_store`
- Insert collection → list returns it with empty members.
- `add_member` corpus + `add_member` nested collection → `direct_members` returns both with correct `member_type`.
- `resolve_corpus_ids` flattens nested collections deterministically.
- `resolve_corpus_ids` on cyclic membership: logs warning, does not loop, returns the reachable set.
- `delete` cascades `collection_members` and any `corrections` rows whose `collection_id` matches.
- `require` on unknown id → `CalError::NotFound`.

### `callimachus-core::storage::correction_store`
- Insert correction with `corpus_id = Some, collection_id = None` (existing kinds) → round-trips.
- Insert correction with `corpus_id = None, collection_id = Some` (EntityLink) → round-trips.
- Insert with both `None` or both `Some` → error.
- `list_for_corpus` ignores collection-scoped corrections; `list_for_collection` ignores corpus-scoped.

### `callimachus-core::EntityLinkIndex`
- Two corpora; one `SameAs` link → `same_as_class` returns the 2-element set; `resolve_name` returns both corpora's entities.
- Transitive `SameAs` chain across three corpora → equivalence class size 3.
- `Implements` link between book entity and code entity → `linked` returns it; `same_as_class` does not include it; `resolve_name` does not fold them but they appear in `related` output.
- No links → all methods return empty.

### `callimachus-core::CollectionService`
- Seed collection with 2 corpora, 3 chunks each, FTS content. `collection_search` matches in both corpora, ordered by relevance with deterministic tiebreak.
- Nested collection (collection A contains collection B contains corpus C) → search reaches corpus C's chunks.
- `collection_entity_resolve` with a `SameAs` link → returns 2 matches; with an `Implements` link → returns 1 match + 1 related.
- `collection_entity_meet` with entities linked by `SameAs` across corpora → returns co-occurrences from both corpora; `first_co_occurrence` deterministic.
- `collection_overview`: `cross_corpus_links_by_kind` counts correct per kind.

### `callimachus-mcp` tools
- `tools/list` returns 17 tools (12 + 5).
- `collection_list` on empty DB returns `{ collections: [] }`.
- `collection_search` with unknown `collection_id` returns a typed not-found error.
- `collection_entity_resolve` with no matches returns `{ matches: [], related: [] }`.

### `callimachus-cli` collection commands
- `collection add "Eisenhorn Trilogy"` creates collection with id `eisenhorn-trilogy`, kind `series`.
- `collection add-member` with unknown corpus id returns clear error.
- `collection add-member <coll> <child> --collection` adds a nested collection membership.
- `collection remove` deletes the collection but leaves member corpora untouched.
- `correct entity-link --kind implements ...` records the link with kind `Implements`.
- `inspect collection-links --kind same_as` filters output to SameAs only.

## Acceptance criteria

- `calli collection add "Eisenhorn Trilogy" --kind series` creates a collection.
- `calli collection add-member eisenhorn-trilogy xenos` (and `malleus`, `hereticus`) attaches corpora.
- `calli collection add "Black Library 40K" --kind catalog` + `calli collection add-member black-library-40k eisenhorn-trilogy --collection` produces a nested collection.
- `calli mcp` + `collection_list` returns both collections with correct member counts and kinds.
- `calli mcp` + `collection_search(collection_id="black-library-40k", query="Bequin")` returns results from all three nested book corpora.
- `calli correct entity-link --collection eisenhorn-trilogy --corpus-a xenos --entity-a eisenhorn --corpus-b malleus --entity-b eisenhorn --kind same_as` records a cross-corpus link.
- `calli mcp` + `collection_entity_meet` honours `SameAs` links (entity in xenos can meet "the same" entity in malleus).
- `calli correct entity-link --collection design-patterns-in-practice --corpus-a gof-design-patterns --entity-a observer-pattern --corpus-b my-app --entity-b event-emitter --kind implements` records a typed cross-type link.
- `calli inspect collection-links eisenhorn-trilogy` prints all links with kinds.
- `cargo test --all` passes.
- `cargo clippy --all -- -D warnings` passes.
- All references to `Library` / `library` from Phase 10 are removed (no dead code, no commented-out stubs).
- No PR opened (this phase is `no_pr: true`).

## Out of scope

- No collection-level summarization (no "summarize the whole catalog" LLM call).
- No collection-level corrections other than `EntityLink` (no collection-wide rename/merge).
- No collection export format.
- No HTTP endpoints for collection tools — Phase 8's HTTP server is shipped, but collection HTTP routes are a follow-on phase.
- No UI for managing collections.
- Cross-corpus entity linking does not retroactively merge entity graphs in storage — links resolve at query time only (same rule as all corrections).
- No backfill/migration of pre-existing data — Phase 10's `Library` storage is a stub with no real data, so the drop-and-recreate approach in migration 005 is safe.
- `EntityLinkKind::References` / `Contrasts` are persisted and surfaced via `collection_entity_resolve.related`, but no specialized query method beyond that ships this phase.

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "Recursive collection storage, typed EntityLink kind, 5 MCP tools, 2 new CLI command groups, schema migration with corrections-table rebuild, Phase 10 Library rename."
  redd:
    model: sonnet
    effort: high
    rationale: "SameAs transitivity, typed link semantics, fan-out merge determinism, recursive cycle handling, XOR invariant on corrections — gaps here silently corrupt cross-corpus analysis."
  marty:
    model: sonnet
    effort: medium
    rationale: "Fan-out result-merging in CollectionService parallels QueryService; worth extracting shared helpers. Collection store and corpus store share insert/list/require patterns."
  perri:
    model: sonnet
    effort: high
    rationale: "Highest-level API surface in the project; typed entity-link semantics and SameAs identity rules must be reviewed precisely — wrong resolution silently produces bad cross-corpus answers."
no_pr: true
```
