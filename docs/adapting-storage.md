# Adapting the Storage Backend

Callimachus uses a `StorageBackend` trait to decouple business logic from the
underlying database. The default implementation (`SqliteBackend`) wraps SQLite via
`rusqlite`. This document explains how to implement an alternative backend
(e.g. PostgreSQL, Redis, a test double) and how to wire it in.

## The Trait

The full contract lives in `crates/callimachus-core/src/storage/backend.rs`.
Every public operation — corpus CRUD, chunk storage, entity/edge graphs,
summaries, run-log entries, corrections, FTS search, and embeddings — is a
synchronous method on `StorageBackend`.

```rust
pub trait StorageBackend: Send + Sync {
    // corpus, chunk, entity, edge, summary, run_log, correction, fts, embedding, library …
}
```

All callers hold the backend as `Arc<dyn StorageBackend>`.

## Implementing a New Backend

1. **Create a module** (e.g. `crates/callimachus-core/src/storage/postgres.rs`).

2. **Define a struct** that owns your connection pool:

   ```rust
   #[derive(Debug)]
   pub struct PostgresBackend {
       pool: sqlx::PgPool,
   }
   ```

3. **Implement `StorageBackend`** for the struct. The compile-time check is
   enforced: if a method is missing the crate will not build.

4. **Handle concurrency** in your struct, not the trait. SQLite uses
   `Arc<Mutex<Connection>>`; Postgres should use a connection pool (`sqlx::PgPool`,
   `deadpool-postgres`, etc.) whose connections are `Send + Sync`.

5. **Run migrations** independently. The `Database::open` path runs SQLite-specific
   migrations via `rusqlite_migration`. For Postgres, use your own migration tool
   (Flyway, `sqlx migrate`, Alembic, etc.).

6. **Re-export** from `crates/callimachus-core/src/storage/mod.rs`:

   ```rust
   pub use postgres::PostgresBackend;
   ```

## Wiring the Backend

In `crates/callimachus-cli/src/main.rs`, the `open_db` helper returns
`Arc<dyn StorageBackend>`:

```rust
fn open_db(path: &Path) -> Result<Arc<dyn StorageBackend>> {
    let backend = SqliteBackend::open(path)?;
    Ok(Arc::new(backend))
}
```

To use Postgres instead, swap this function to construct a `PostgresBackend` from
a connection string (e.g. from an environment variable or config file).

## The `PostgresBackend` Stub

`crates/callimachus-core/src/storage/postgres.rs` ships as a **compile-only
stub**. Every method returns:

```rust
Err(CalError::Other("PostgresBackend is not yet implemented …"))
```

Its purpose is to:

- Prove `StorageBackend` is implementable without SQLite.
- Catch any SQLite-specific types that leaked into the trait signature.
- Give contributors a clear starting point — replace each `Err(unimplemented())`
  with a real query.

## Trait Stability Guarantees

The `StorageBackend` trait is **not** `#[async_trait]`. All methods are
synchronous. Async callers use `tokio::task::spawn_blocking` if they need to
call the backend from an async context without blocking the executor (the
current indexing passes do this internally).

The trait is currently `pub` but **not semver-stable**. New methods may be
added in minor releases. If you maintain a custom backend outside this repo,
pin your dependency to a specific version.

## Testing Custom Backends

Use `SqliteBackend::open_in_memory()` for unit tests — it's cheap and fast.
For integration tests that need a real Postgres connection, gate the test with
`#[cfg(feature = "postgres-integration")]` and supply the database URL via the
`DATABASE_URL` environment variable.

## Method Reference

All methods are documented in `backend.rs`. The groups are:

| Group       | Representative methods |
|-------------|------------------------|
| Corpus      | `corpus_insert`, `corpus_list`, `corpus_require`, `corpus_delete` |
| Chunk       | `chunk_upsert`, `chunk_list`, `chunk_get_by_uri`, `chunk_count` |
| Entity      | `entity_upsert`, `entity_list`, `entity_merge` |
| Edge        | `edge_upsert`, `edge_list`, `edge_get_for_entity` |
| Summary     | `summary_upsert`, `summary_list`, `summary_get` |
| Run log     | `run_start`, `run_finish`, `run_latest` |
| Corrections | `correction_insert`, `correction_list`, `correction_list_all` |
| FTS         | `fts_search`, `fts_rebuild` |
| Embeddings  | `embedding_upsert`, `embedding_list_for_corpus`, `embedding_count` |
| Library     | `library_insert`, `library_list`, `library_get` |
