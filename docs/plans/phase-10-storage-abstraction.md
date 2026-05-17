# Phase 10 — Storage backend abstraction

## Context

Phases 1–9 implement Callimachus as a local-first tool backed exclusively by SQLite. That is
the correct choice for personal use — zero ops, single binary, FTS5 included. But the
architecture as built has SQLite baked into every store: `rusqlite::Connection` is passed
directly, SQL uses FTS5 syntax, vectors are stored as raw BLOBs, and migrations are
SQLite-specific.

This phase introduces a `StorageBackend` trait that makes the storage layer swappable.
`SqliteBackend` (the existing implementation) satisfies it without behavioural change. A
`PostgresBackend` stub — compilable but returning `unimplemented!()` — is added as proof of
concept and as a target for future hosted deployments.

The goal is not to ship Postgres support. The goal is to ensure the codebase does not
permanently close the door on it. This is an architectural prep phase: refactor, no new
features, no behaviour changes, all existing tests pass.

Reference: `docs/plans/callimachus-standalone.md §1.1`, conversation notes on AWS hosting.

## Target

- **Repo:** `callimachus`
- **Branch:** `feat/phase-10-storage-abstraction`
- **Base:** `main`

## Files to change

---

### `crates/callimachus-core/src/storage/backend.rs` (new)

Define the `StorageBackend` trait. Every method on every store becomes a method here,
grouped by domain.

```rust
/// A swappable storage backend. `SqliteBackend` is the default implementation.
/// Future: `PostgresBackend` (RDS/Aurora), `MemoryBackend` (test-only).
pub trait StorageBackend: Send + Sync {
    // --- Corpus ---
    fn corpus_insert(&self, corpus: &Corpus) -> Result<()>;
    fn corpus_list(&self) -> Result<Vec<Corpus>>;
    fn corpus_get(&self, id: &str) -> Result<Option<Corpus>>;
    fn corpus_require(&self, id: &str) -> Result<Corpus>;
    fn corpus_update_status(&self, id: &str, status: CorpusStatus) -> Result<()>;
    fn corpus_set_last_indexed(&self, id: &str, at: &str) -> Result<()>;
    fn corpus_delete(&self, id: &str) -> Result<bool>;
    fn corpus_exists(&self, id: &str) -> Result<bool>;

    // --- Chunk ---
    fn chunk_upsert(&self, chunk: &Chunk) -> Result<()>;
    fn chunk_has(&self, id: &str) -> Result<bool>;
    fn chunk_get(&self, id: &str) -> Result<Option<Chunk>>;
    fn chunk_get_by_uri(&self, uri: &str) -> Result<Option<Chunk>>;
    fn chunk_list(&self, corpus_id: &str) -> Result<Vec<Chunk>>;
    fn chunk_count(&self, corpus_id: &str) -> Result<u64>;

    // --- Entity ---
    fn entity_upsert(&self, entity: &Entity) -> Result<()>;
    fn entity_get_by_id(&self, id: &str) -> Result<Option<Entity>>;
    fn entity_find_by_name(&self, corpus_id: &str, name: &str) -> Result<Vec<Entity>>;
    fn entity_list(&self, corpus_id: &str) -> Result<Vec<Entity>>;
    fn entity_count(&self, corpus_id: &str) -> Result<u64>;
    fn entity_top(&self, corpus_id: &str, limit: usize) -> Result<Vec<Entity>>;
    fn entity_merge(&self, keep_id: &str, absorb_id: &str) -> Result<()>;

    // --- Edge ---
    fn edge_upsert(&self, edge: &Edge) -> Result<()>;
    fn edge_get_for_entity(
        &self,
        entity_id: &str,
        direction: EdgeDirection,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Edge>>;
    fn edge_count(&self, corpus_id: &str) -> Result<u64>;

    // --- Summary ---
    fn summary_upsert(&self, summary: &Summary) -> Result<()>;
    fn summary_get(
        &self,
        corpus_id: &str,
        target_kind: &SummaryTargetKind,
        target_id: &str,
    ) -> Result<Option<Summary>>;

    // --- Run log ---
    fn run_start(&self, corpus_id: &str, pass: &str, provider: Option<&str>) -> Result<String>;
    fn run_finish(&self, run_id: &str, status: RunStatus, stats: &PassStats) -> Result<()>;
    fn run_latest(&self, corpus_id: &str, limit: usize) -> Result<Vec<RunRecord>>;

    // --- Corrections ---
    fn correction_insert(&self, correction: &Correction) -> Result<()>;
    fn correction_list(&self, corpus_id: &str) -> Result<Vec<Correction>>;
    fn correction_delete(&self, id: &str) -> Result<bool>;

    // --- FTS / Search ---
    fn fts_search(
        &self,
        corpus_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FtsResult>>;
    fn fts_rebuild(&self, corpus_id: &str) -> Result<()>;

    // --- Embeddings ---
    fn embedding_upsert(&self, embedding: &StoredEmbedding) -> Result<()>;
    fn embedding_get_for_chunk(&self, chunk_id: &str) -> Result<Option<StoredEmbedding>>;
    fn embedding_list_for_corpus(&self, corpus_id: &str) -> Result<Vec<StoredEmbedding>>;
    fn embedding_count(&self, corpus_id: &str) -> Result<u64>;

    // --- Library ---
    fn library_insert(&self, library: &Library) -> Result<()>;
    fn library_list(&self) -> Result<Vec<Library>>;
    fn library_get(&self, id: &str) -> Result<Option<Library>>;
    fn library_add_corpus(&self, library_id: &str, corpus_id: &str) -> Result<()>;
    fn library_remove_corpus(&self, library_id: &str, corpus_id: &str) -> Result<()>;
    fn library_delete(&self, id: &str) -> Result<bool>;
}
```

Note: `Library` type is introduced here (even though the full library feature is Phase 11) so
the backend trait is complete. `SqliteBackend` implements the library methods using the
`libraries` and `library_corpora` tables added in migration `004_libraries.sql`.

---

### `crates/callimachus-core/src/storage/sqlite.rs` (new)

`SqliteBackend` wraps `Arc<Mutex<Database>>` and implements `StorageBackend` by delegating to
the existing store functions.

```rust
pub struct SqliteBackend {
    db: Arc<Mutex<Database>>,
}

impl SqliteBackend {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self { db: Arc::new(Mutex::new(Database::open(path)?)) })
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self { db: Arc::new(Mutex::new(Database::open_in_memory()?)) })
    }
}

impl StorageBackend for SqliteBackend {
    fn corpus_insert(&self, corpus: &Corpus) -> Result<()> {
        corpus_store::insert(&*self.db.lock().unwrap(), corpus)
    }
    // ... one line per method, delegating to the existing store modules
}
```

All existing `*_store` functions remain — they are the implementation. `SqliteBackend` is a
thin delegation wrapper. Do not inline the SQL into `SqliteBackend`; keep the store modules
as they are.

---

### `crates/callimachus-core/src/storage/postgres.rs` (new)

Stub only. Compiles. Every method returns `Err(CalError::Other("postgres backend not yet
implemented".into()))`. This exists to:

1. Prove the trait is implementable without SQLite
2. Give future contributors a clear starting point
3. Catch any `SqliteBackend`-specific assumptions that leaked into the trait signature

```rust
pub struct PostgresBackend {
    // future: tokio_postgres::Client or sqlx::PgPool
    _placeholder: (),
}

impl PostgresBackend {
    /// Not yet implemented. Returns an error at construction time.
    pub fn connect(_url: &str) -> Result<Self> {
        Err(CalError::Other(
            "PostgresBackend is not yet implemented. \
             Contributions welcome — see docs/adapting-storage.md".into()
        ))
    }
}

impl StorageBackend for PostgresBackend {
    fn corpus_insert(&self, _corpus: &Corpus) -> Result<()> {
        Err(CalError::Other("postgres backend not yet implemented".into()))
    }
    // ... repeat for every method
}
```

---

### `crates/callimachus-core/migrations/004_libraries.sql` (new)

```sql
CREATE TABLE libraries (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE library_corpora (
    library_id TEXT NOT NULL,
    corpus_id TEXT NOT NULL,
    added_at TEXT NOT NULL,
    PRIMARY KEY (library_id, corpus_id),
    FOREIGN KEY (library_id) REFERENCES libraries(id) ON DELETE CASCADE,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE
);

CREATE INDEX idx_library_corpora_library ON library_corpora(library_id);
CREATE INDEX idx_library_corpora_corpus ON library_corpora(corpus_id);
```

---

### `crates/callimachus-core/src/types/library.rs` (new)

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub corpus_ids: Vec<String>,
    pub created_at: String,
}
```

Add to `src/types/mod.rs` and re-export from `src/lib.rs`.

---

### Propagate `StorageBackend` through callers

Every struct that currently holds `Arc<Mutex<Database>>` is updated to hold
`Arc<dyn StorageBackend>`:

- `QueryService` — `db: Arc<Mutex<Database>>` → `backend: Arc<dyn StorageBackend>`
- `IndexPipeline` — same
- `CorrectionsEngine::load` — takes `&dyn StorageBackend` instead of `&Database`
- `McpServer` — no change (holds `QueryService`)
- `callimachus-cli` commands — open DB via `SqliteBackend::open`, wrap in `Arc`, pass to pipeline/query service

Update `src/lib.rs` to re-export `StorageBackend`, `SqliteBackend`, `PostgresBackend`.

The CLI entry point pattern changes from:

```rust
let db = Arc::new(Mutex::new(Database::open(&db_path)?));
let qs = QueryService::new(db);
```

to:

```rust
let backend: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open(&db_path)?);
let qs = QueryService::new(backend);
```

---

### `docs/adapting-storage.md` (new)

Document the `StorageBackend` trait contract. Sections:
1. Why the abstraction exists
2. Implementing a new backend (required methods, expected semantics)
3. FTS considerations (FTS5 is SQLite-specific; Postgres backends should use `pg_trgm` +
   `tsvector`; the `fts_search` contract is defined in terms of returned `FtsResult`, not
   implementation)
4. Embedding considerations (BLOB encoding is SQLite-specific; Postgres backends should use
   `pgvector`)
5. Migration strategy (backends manage their own schema; the `Database` migration runner is
   SQLite-specific and should not be used by non-SQLite backends)
6. Litestream as a zero-ops middle ground for hosted deployments

---

## Approach

1. Add `004_libraries.sql` migration and `Library` type. Run `cargo test` — migrations test
   passes, all existing tests pass.
2. Write `StorageBackend` trait in `backend.rs` with all method signatures. Compile only.
3. Implement `SqliteBackend` delegating to existing store modules. All tests pass.
4. Implement `PostgresBackend` stub (all methods return error). Compile only.
5. Propagate `Arc<dyn StorageBackend>` through `QueryService`, `IndexPipeline`,
   `CorrectionsEngine`. Fix all callers.
6. Update CLI commands to construct `SqliteBackend::open(...)` instead of `Database::open`.
7. Write `docs/adapting-storage.md`.
8. `cargo test --all` passes, `cargo clippy --all -- -D warnings` passes.
9. Verify: `calli corpus list`, `calli index`, `calli mcp` all work identically to before.

## Acceptance criteria

- `cargo test --all` passes with no behaviour changes
- `cargo clippy --all -- -D warnings` passes
- `SqliteBackend::open_in_memory()` works for all tests that previously used `Database::open_in_memory()`
- `PostgresBackend::connect("")` returns a clear "not yet implemented" error
- `calli corpus add book xenos /path/to/xenos.epub` and `calli mcp` work identically to before
- `docs/adapting-storage.md` exists and describes the trait contract

## Out of scope

- No Postgres implementation (stub only)
- No connection pooling
- No async storage trait (sync for now — `Mutex<Connection>` is acceptable at this scale)
- No per-backend configuration in `config.toml` (always SQLite unless code is changed)
- No data migration tooling between backends

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "Pure refactor — introduce trait, implement delegation wrapper, propagate through all callers without breaking behaviour. Breadth of changes across crates is the challenge."
  redd:
    model: sonnet
    effort: medium
    rationale: "All existing tests should pass unchanged; add trait-level tests confirming SqliteBackend and PostgresBackend both satisfy the trait."
  marty:
    model: sonnet
    effort: high
    rationale: "This IS the refactor phase — finding and eliminating all direct Database references is Marty's whole job here."
  perri:
    model: sonnet
    effort: high
    rationale: "Trait abstraction boundary must be airtight — any leaked rusqlite types in public signatures will break future backend implementations."
no_pr: true
```
