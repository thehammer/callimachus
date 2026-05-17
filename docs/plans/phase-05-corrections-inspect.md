# Phase 5 — Corrections + inspect CLI

## Context

Phase 4 delivered the full MCP tool surface and `calli mcp`. The index is queryable but read-only from the operator's perspective — there is no way to correct mistakes the LLM made during extraction (wrong merges, wrong names, hallucinated relationships) without re-indexing from scratch.

This phase implements the corrections overlay engine and the full suite of operator-facing CLI tools: `calli correct`, `calli inspect`, and `calli export`. Corrections survive re-indexing because they are stored as an immutable log and applied at query time, not at write time.

Reference: `docs/plans/callimachus-standalone.md §7`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 4 commit

## Files to change

---

### `crates/callimachus-core` — corrections overlay engine

New module: `src/corrections/`. Add `pub mod corrections;` to `src/lib.rs`. Re-export `CorrectionsEngine`.

#### `src/corrections/mod.rs`

```rust
pub mod engine;
pub mod types;
pub use engine::CorrectionsEngine;
pub use types::{Correction, CorrectionKind};
```

#### `src/corrections/types.rs`

The `Correction` type mirrors the `corrections` table row. `CorrectionKind` is the tagged union of all supported operations.

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Correction {
    pub id: String,
    pub corpus_id: String,
    pub kind: CorrectionKind,
    pub applied_at: String,   // ISO 8601
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CorrectionKind {
    Merge {
        entity_a_id: String,
        entity_b_id: String,
        canonical_id: String,   // which one to keep (a or b)
    },
    Unmerge {
        entity_id: String,
        split_by: SplitGranularity,
    },
    Rename {
        entity_id: String,
        new_name: String,
    },
    Alias {
        entity_id: String,
        add: Vec<String>,
        remove: Vec<String>,
    },
    EditSummary {
        target_kind: String,   // "chunk" | "entity" | "corpus"
        target_id: String,
        text: String,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitGranularity {
    Scene,
    Chapter,
}
```

#### `src/corrections/engine.rs`

`CorrectionsEngine` loads corrections for a corpus at construction and applies them as an in-memory overlay.

```rust
pub struct CorrectionsEngine {
    corrections: Vec<Correction>,
}

impl CorrectionsEngine {
    /// Load all corrections for corpus_id from the DB.
    pub fn load(db: &Database, corpus_id: &str) -> Result<Self> { ... }

    /// Apply merge/rename/alias corrections to a mutable entity list in place.
    pub fn apply_to_entities(&self, entities: &mut Vec<Entity>) { ... }

    /// Apply edit_summary corrections: return the operator text if one exists
    /// for this target, otherwise return None (caller uses the generated text).
    pub fn override_summary(
        &self,
        target_kind: &str,
        target_id: &str,
    ) -> Option<&str> { ... }

    /// Return the canonical entity_id that a given id maps to under merge
    /// corrections. Returns the input id unchanged if no merge applies.
    pub fn resolve_entity_id<'a>(&'a self, entity_id: &'a str) -> &'a str { ... }

    /// All loaded corrections, ordered by applied_at asc.
    pub fn all(&self) -> &[Correction] { &self.corrections }
}
```

Implementation notes:

**Merge**: Build a union-find structure from all `Merge` corrections. `resolve_entity_id` walks the find path to the root. `apply_to_entities` collapses entities with the same resolved root: the canonical entity absorbs aliases and sums `appearance_count`.

**Rename**: After merge resolution, if an entity has a `Rename` correction, override `canonical_name`.

**Alias add/remove**: After rename, apply alias mutations in `applied_at` order (later corrections win on conflicts).

**Unmerge**: Record separately; the engine does not apply unmerge in `apply_to_entities` (it only undoes a merge that was stored in the `entities` table, not a correction-merge). Unmerge is a signal to the re-indexer: when `calli reindex` sees an `Unmerge` correction it re-runs the semantic pass for the affected entity's chunk range without alias resolution for those entities.

**EditSummary**: Stored per `(target_kind, target_id)`. `override_summary` scans the corrections list for the most recent matching entry.

#### Integration into `QueryService`

`QueryService::new` now accepts an optional `CorrectionsEngine`. Every method that returns entities runs results through `engine.apply_to_entities`. Every method that returns summaries calls `engine.override_summary` and substitutes if Some.

Add a constructor:

```rust
impl QueryService {
    pub fn new(db: Arc<Mutex<Database>>) -> Self { ... }

    pub fn with_corrections(db: Arc<Mutex<Database>>, corrections: CorrectionsEngine) -> Self { ... }
}
```

In `calli mcp` startup: always load corrections (`CorrectionsEngine::load`) and pass to `QueryService::with_corrections`.

---

### `crates/callimachus-cli` — `calli correct`

#### `src/commands/correct.rs` (new)

All five subcommands share a common `run(corpus_id, subcommand, db, output)` entry point.

```rust
pub enum CorrectSubcommand {
    Merge { entity_a: String, entity_b: String, keep: Option<String> },
    Unmerge { entity_id: String, split_by: SplitGranularity },
    Rename { entity_id: String, new_name: String },
    Alias { entity_id: String, add: Vec<String>, remove: Vec<String> },
    EditSummary { target: String, text: String },  // target = location URI or entity id
}

pub fn run(
    corpus_id: &str,
    sub: CorrectSubcommand,
    db: &Database,
) -> anyhow::Result<()>
```

Each subcommand:
1. Validates the corpus exists (`corpus_store::require`).
2. Validates referenced entity IDs / location URIs exist (print a helpful error with suggestions if not).
3. Builds a `Correction` (uuid v4 for id, `chrono::Utc::now()` for applied_at).
4. Calls `correction_store::insert`.
5. Prints a one-line confirmation: `✓ Correction recorded (id: <uuid>)`.

**`merge` details**: If `--keep` is omitted, keep entity_a. Warn if entity_a and entity_b are in different corpora (forbidden; print error and exit 1). Print the canonical name being kept.

**`alias`**: `--add` and `--remove` may be specified multiple times (clap `action = Append`).

**`edit-summary`**: The `target` argument accepts either a location URI (`calli://xenos/ch/3/sc/7`) or a bare entity id. If it looks like a location URI, set `target_kind = "chunk"`; if it looks like an entity id, set `target_kind = "entity"`; if it equals the corpus_id, set `target_kind = "corpus"`.

Wire all subcommands through `src/main.rs` → `Command::Correct`.

---

### `crates/callimachus-cli` — `calli inspect`

#### `src/commands/inspect.rs` (new)

Four subcommands.

```rust
pub enum InspectSubcommand {
    Entities {
        corpus_id: String,
        filter: Option<String>,
        kind: Option<String>,
        min_confidence: Option<f32>,
        limit: Option<usize>,
    },
    Chunk {
        location: String,   // URI or bare path
    },
    Runs {
        corpus_id: String,
        limit: Option<usize>,
    },
    Corrections {
        corpus_id: String,
    },
}
```

**`inspect entities`**: Query `entity_store::list` (add this function — returns all entities for corpus, ordered by `appearance_count DESC`). Apply filter/kind/min_confidence in Rust (not SQL, for simplicity). Print as a table:

```
NAME                   KIND        APPEARANCES  CONFIDENCE  ALIASES
Eisenhorn              character   47           0.95        Gregor, the Inquisitor
Bequin                 character   31           0.92        Alizebeth, Beta
Malinter               place       12           0.71
```

With `--filter=<string>`: case-insensitive substring match on canonical_name and aliases.

**`inspect chunk`**: Load chunk by location URI (`chunk_store::get_by_uri` — add this function). Print all fields in key-value format using `output::print_kv`. Also load entities whose `first_location_uri` or `last_location_uri` matches and print under "Entities present:".

**`inspect runs`**: Load runs via `run_log::latest_runs` (already exists). Print as table, most recent first:

```
PASS        STATUS     STARTED              CHUNKS     ENTITIES  COST
semantic    completed  2025-01-15 14:32:01  342/342    1247      $1.23
chunk       completed  2025-01-15 14:30:00  342/342    -         -
```

Default limit: 20. `--limit` overrides.

**`inspect corrections`**: Load all corrections via `correction_store::list`. Print each with timestamp, kind, and human-readable description:

```
2025-01-15 16:45:02  merge      Merged "the Inquisitor" (abc123) into "Eisenhorn" (def456)
2025-01-15 17:01:15  rename     Renamed entity xyz789 to "Pontius Glaw"
2025-01-15 17:10:00  edit_sum   Replaced summary for calli://xenos/ch/3
```

Wire all subcommands through `src/main.rs` → `Command::Inspect`.

---

### `crates/callimachus-cli` — `calli export`

#### `src/commands/export.rs` (new)

```rust
pub async fn run(
    corpus_id: &str,
    format: ExportFormat,
    output: Option<PathBuf>,
    db: &Database,
) -> anyhow::Result<()>

pub enum ExportFormat {
    Jsonl,
    // Scip reserved for Phase 7 (code adapter)
}
```

**JSONL format**: One JSON object per line. Each line is one of:

```json
{"kind": "chunk", "id": "...", "corpus_id": "...", "location_uri": "...", "parent_path": "...", "content": "..."}
{"kind": "entity", "id": "...", "corpus_id": "...", "canonical_name": "...", "kind": "character", "aliases": [...], "appearance_count": 47}
{"kind": "edge", "id": "...", "corpus_id": "...", "from_entity_id": "...", "to_entity_id": "...", "kind": "meets", "location_uri": "..."}
{"kind": "summary", "id": "...", "corpus_id": "...", "target_kind": "chunk", "target_id": "...", "text": "..."}
{"kind": "correction", "id": "...", "corpus_id": "...", "kind": "merge", "payload": {...}}
```

Emit chunks first, then entities (with corrections applied), then edges, then summaries (with edit_summary corrections applied), then raw corrections. Write to file if `--output` given, stdout otherwise.

Progress: emit `tracing::info!` every 1000 lines.

Wire through `src/main.rs` → `Command::Export`.

---

### Storage: new functions needed

#### `src/storage/entity_store.rs`

Add:
```rust
pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Entity>>
```

Returns all entities for corpus ordered by `appearance_count DESC`.

#### `src/storage/chunk_store.rs`

Add:
```rust
pub fn get_by_uri(db: &Database, uri: &str) -> Result<Option<Chunk>>
```

Queries `WHERE location_uri = ?1 LIMIT 1`.

#### `src/storage/correction_store.rs`

Add:
```rust
pub fn list(db: &Database, corpus_id: &str) -> Result<Vec<Correction>>
pub fn delete(db: &Database, correction_id: &str) -> Result<bool>
```

`list` orders by `applied_at ASC`. `delete` returns `true` if a row was deleted.

---

## Tests

### `callimachus-core/corrections`

`src/corrections/engine.rs` `#[cfg(test)]`:

- **Merge**: seed two entities A and B, apply merge correction keeping A → `apply_to_entities` returns one entity (A) with B's aliases absorbed and appearance_count summed. `resolve_entity_id("B-id")` returns `"A-id"`.
- **Rename**: apply rename correction → entity's `canonical_name` is overridden.
- **Alias add**: apply alias correction adding "the Inquisitor" → entity aliases include it.
- **Alias remove**: apply alias correction removing an existing alias → alias absent from result.
- **EditSummary**: `override_summary("chunk", "uri")` returns the operator text; non-matching target returns None.
- **Chained corrections**: merge A→B, then rename the canonical → both merge and rename apply in order.
- **Empty corrections**: engine with no corrections returns entities unchanged.

### `callimachus-cli/correct`

`src/commands/correct.rs` `#[cfg(test)]`:

- Test that `merge` with invalid entity_id returns clear error (not a panic).
- Test that `merge` with mismatched corpus_ids returns error.
- Test that each subcommand writes exactly one row to `corrections`.
- Test that `edit-summary` with a location URI sets `target_kind = "chunk"`.
- Test that `edit-summary` with a bare entity id sets `target_kind = "entity"`.

### `callimachus-cli/inspect`

`src/commands/inspect.rs` `#[cfg(test)]`:

- Test `inspect entities` with 3 seeded entities, filter by kind → returns correct subset.
- Test `inspect entities` with min_confidence=0.9 → filters low-confidence entities.
- Test `inspect chunk` with valid URI → prints all fields.
- Test `inspect chunk` with invalid URI → clear not-found error.
- Test `inspect runs` with 3 runs → prints all 3, most recent first.
- Test `inspect corrections` with 2 corrections → prints both with human-readable descriptions.

### `callimachus-cli/export`

`src/commands/export.rs` `#[cfg(test)]`:

- Seed corpus with 2 chunks, 3 entities, 1 edge, 1 summary, 1 correction.
- Run export to a Vec<u8>.
- Parse JSONL output, assert all 7 records present (2 chunks + 3 entities + 1 edge + 1 summary).
- Assert corrections section is last.
- Assert entity export reflects applied corrections (merged entities appear merged).

## Acceptance criteria

- `calli correct xenos merge <id-a> <id-b>` records a correction and prints confirmation
- `calli inspect entities xenos` prints a table of all entities with aliases and appearance counts
- `calli inspect corrections xenos` shows all recorded corrections in human-readable form
- After recording a merge correction, querying via MCP returns the merged entity (corrections applied)
- `calli export xenos --format=jsonl` writes valid JSONL to stdout
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes
