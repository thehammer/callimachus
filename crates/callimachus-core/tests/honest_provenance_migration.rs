//! Schema-level tests covering migrations 013 and 015 (honest provenance).
//!
//! Opens a fresh in-memory database (which runs all migrations through the
//! latest) and asserts the resulting schema shape via `PRAGMA table_info(...)`,
//! `PRAGMA index_list(...)`, and `sqlite_master` queries. Also verifies the
//! history-table uniqueness constraint.
//!
//! All tests use `Database::open_in_memory()` directly so they can reach the
//! raw `rusqlite::Connection` via `db.conn()`. No `StorageBackend` methods are
//! exercised here — this file is purely schema-level.
//!
//! # Coverage
//!
//! 1. `entities_head_table_has_provenance_and_content_hash_columns` — the three
//!    new columns on `entities` exist after migrations 013+015.
//! 2. `chunks_head_table_has_new_columns` — `chunks` gained `derived_at_kind`,
//!    `derived_at_sha`, `file_shape_hash`, and `entity_id_list`.
//! 3. `embeddings_head_table_has_provenance_and_context_hash_columns` — the
//!    three new columns on `embeddings` exist.
//! 4. `all_head_tables_have_provenance_column_pair` — every head table that
//!    participated in Part 1 of migration 013 has both `derived_at_kind` and
//!    `derived_at_sha`.
//! 5. `legacy_derived_at_version_dropped` — migration 015 has dropped the
//!    `derived_at_version` and `superseded_at_version` columns from all tables.
//! 6. `new_tables_are_queryable_and_empty` — `layer2_cache`,
//!    `artifact_tombstones`, and `embeddings_history` all exist and start empty.
//! 7. `history_tables_have_provenance_and_superseded_at_sha_columns` —
//!    `entities_history`, `chunks_history`, and `entity_purposes_history` all
//!    gained the three new history columns; `entities_history` additionally has
//!    `content_hash`.
//! 8. `entities_history_uniqueness_index_exists` — the
//!    `uq_entities_history_identity` index is present on `entities_history`.
//! 9. `fresh_entities_row_defaults_concrete_kind` — a newly-inserted `entities`
//!    row has `derived_at_kind='concrete'` (the DEFAULT) before any backfill.
//! 10. `entities_history_unique_index_rejects_true_duplicate` — inserting two
//!     `entities_history` rows with identical `(corpus_id, id, derived_at_kind,
//!     derived_at_sha)` where `derived_at_sha` is non-empty fails with a
//!     constraint error on the second insert.
//! 11. `history_uniqueness_indexes_are_plain_column_only` — confirms that none
//!     of the `uq_*_history_identity` indexes contain a COALESCE expression.

use callimachus_core::Database;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Collect the column names present in `table` via `PRAGMA table_info`.
fn column_names(db: &Database, table: &str) -> Vec<String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare PRAGMA table_info");
    stmt.query_map([], |row| row.get::<_, String>(1))
        .expect("query PRAGMA table_info")
        .map(|r| r.expect("column name"))
        .collect()
}

/// Return `true` if `table` has a column named `col`.
fn has_column(db: &Database, table: &str, col: &str) -> bool {
    column_names(db, table).contains(&col.to_string())
}

/// Collect all index names on `table` via `PRAGMA index_list`.
fn index_names(db: &Database, table: &str) -> Vec<String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_list({table})"))
        .expect("prepare PRAGMA index_list");
    // index_list columns: seq, name, unique, origin, partial
    stmt.query_map([], |row| row.get::<_, String>(1))
        .expect("query PRAGMA index_list")
        .map(|r| r.expect("index name"))
        .collect()
}

/// Return `true` if `table` exists in `sqlite_master`.
fn table_exists(db: &Database, table: &str) -> bool {
    db.conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

// ── Test 1: entities head table — new columns ────────────────────────────────

/// After migration 013 `entities` must have `derived_at_kind`, `derived_at_sha`,
/// and `content_hash`.
#[test]
fn entities_head_table_has_provenance_and_content_hash_columns() {
    let db = Database::open_in_memory().unwrap();

    assert!(
        has_column(&db, "entities", "derived_at_kind"),
        "entities must have derived_at_kind after migration 013"
    );
    assert!(
        has_column(&db, "entities", "derived_at_sha"),
        "entities must have derived_at_sha after migration 013"
    );
    assert!(
        has_column(&db, "entities", "content_hash"),
        "entities must have content_hash after migration 013"
    );
}

// ── Test 2: chunks head table — new columns ──────────────────────────────────

/// After migration 013 `chunks` must have `derived_at_kind`, `derived_at_sha`,
/// `file_shape_hash`, and `entity_id_list`.
#[test]
fn chunks_head_table_has_new_columns() {
    let db = Database::open_in_memory().unwrap();

    for col in &[
        "derived_at_kind",
        "derived_at_sha",
        "file_shape_hash",
        "entity_id_list",
    ] {
        assert!(
            has_column(&db, "chunks", col),
            "chunks must have column {col} after migration 013"
        );
    }
}

// ── Test 3: embeddings head table — new columns ──────────────────────────────

/// After migration 013 `embeddings` must have `derived_at_kind`,
/// `derived_at_sha`, and `surrounding_context_hash`.
#[test]
fn embeddings_head_table_has_provenance_and_context_hash_columns() {
    let db = Database::open_in_memory().unwrap();

    for col in &[
        "derived_at_kind",
        "derived_at_sha",
        "surrounding_context_hash",
    ] {
        assert!(
            has_column(&db, "embeddings", col),
            "embeddings must have column {col} after migration 013"
        );
    }
}

// ── Test 4: every relevant head table has the provenance column pair ──────────

/// All artifact head tables that received tagged-union columns in Part 1 of
/// migration 013 must have both `derived_at_kind` and `derived_at_sha`.
#[test]
fn all_head_tables_have_provenance_column_pair() {
    let db = Database::open_in_memory().unwrap();

    let tables = [
        "entities",
        "edges",
        "entity_purposes",
        "entity_contracts",
        "entity_blocks",
        "summaries",
        "themes",
        "chunks",
    ];

    for table in &tables {
        assert!(
            has_column(&db, table, "derived_at_kind"),
            "table {table} must have derived_at_kind"
        );
        assert!(
            has_column(&db, table, "derived_at_sha"),
            "table {table} must have derived_at_sha"
        );
    }
}

// ── Test 5: legacy derived_at_version and superseded_at_version are DROPPED ────

/// Migration 015 drops the `derived_at_version` and `superseded_at_version`
/// columns from all head and history tables. After all migrations run neither
/// column should appear on any table.
#[test]
fn legacy_derived_at_version_dropped() {
    let db = Database::open_in_memory().unwrap();

    // Head tables that had derived_at_version before migration 015:
    let head_tables = [
        "entities",
        "edges",
        "entity_purposes",
        "entity_contracts",
        "entity_blocks",
        "summaries",
        "themes",
    ];
    for table in &head_tables {
        assert!(
            !has_column(&db, table, "derived_at_version"),
            "{table} must NOT have derived_at_version after migration 015"
        );
    }

    // History tables that had derived_at_version (chunks_history never had it):
    let history_tables_with_derived = [
        "entities_history",
        "edges_history",
        "entity_purposes_history",
        "entity_contracts_history",
        "entity_blocks_history",
        "summaries_history",
        "themes_history",
    ];
    for table in &history_tables_with_derived {
        assert!(
            !has_column(&db, table, "derived_at_version"),
            "{table} must NOT have derived_at_version after migration 015"
        );
    }

    // All history tables had superseded_at_version (embeddings_history is excluded
    // — it was created fresh in migration 013 without it):
    let history_tables_with_superseded = [
        "entities_history",
        "edges_history",
        "entity_purposes_history",
        "entity_contracts_history",
        "entity_blocks_history",
        "summaries_history",
        "themes_history",
        "chunks_history",
    ];
    for table in &history_tables_with_superseded {
        assert!(
            !has_column(&db, table, "superseded_at_version"),
            "{table} must NOT have superseded_at_version after migration 015"
        );
    }

    // embeddings_history was created fresh in 013 without either column — still clean.
    assert!(
        !has_column(&db, "embeddings_history", "derived_at_version"),
        "embeddings_history must NOT have derived_at_version"
    );
    assert!(
        !has_column(&db, "embeddings_history", "superseded_at_version"),
        "embeddings_history must NOT have superseded_at_version"
    );
}

// ── Test 6: new tables are queryable and empty ────────────────────────────────

/// `layer2_cache`, `artifact_tombstones`, and `embeddings_history` must all
/// exist and start with zero rows after a fresh migration.
#[test]
fn new_tables_are_queryable_and_empty() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();

    for table in &["layer2_cache", "artifact_tombstones", "embeddings_history"] {
        assert!(
            table_exists(&db, table),
            "table {table} must exist after migration 013"
        );

        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("SELECT COUNT(*) FROM {table} failed: {e}"));

        assert_eq!(count, 0, "table {table} must be empty in a fresh database");
    }
}

// ── Test 7: history tables have provenance + superseded_at_sha columns ────────

/// `entities_history`, `chunks_history`, and `entity_purposes_history` must
/// have `derived_at_kind`, `derived_at_sha`, and `superseded_at_sha` after
/// migration 013. `entities_history` additionally requires `content_hash`.
#[test]
fn history_tables_have_provenance_and_superseded_at_sha_columns() {
    let db = Database::open_in_memory().unwrap();

    let base_tables = [
        "entities_history",
        "chunks_history",
        "entity_purposes_history",
    ];
    for table in &base_tables {
        for col in &["derived_at_kind", "derived_at_sha", "superseded_at_sha"] {
            assert!(
                has_column(&db, table, col),
                "history table {table} must have column {col} after migration 013"
            );
        }
    }

    // entities_history specifically also gets content_hash
    assert!(
        has_column(&db, "entities_history", "content_hash"),
        "entities_history must have content_hash after migration 013"
    );
}

// ── Test 8: entities_history uniqueness index exists ─────────────────────────

/// Migration 013 creates `uq_entities_history_identity` on `entities_history`.
/// This index is the storage-side fix for the duplicate-row bug.
#[test]
fn entities_history_uniqueness_index_exists() {
    let db = Database::open_in_memory().unwrap();
    let indexes = index_names(&db, "entities_history");

    assert!(
        indexes.iter().any(|n| n == "uq_entities_history_identity"),
        "uq_entities_history_identity index must exist on entities_history; found: {indexes:?}"
    );
}

// ── Test 9: fresh entities row defaults to concrete kind ─────────────────────

/// A row inserted into `entities` without specifying `derived_at_kind` must
/// default to `'concrete'` (the column DEFAULT added by migration 013).
#[test]
fn fresh_entities_row_defaults_concrete_kind() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();

    // We must satisfy FK: insert a corpus first.
    conn.execute(
        "INSERT INTO corpora (id, name, kind, source, status, created_at) \
         VALUES ('corp-default', 'Test Corp', 'code', '/tmp', 'registered', datetime('now'))",
        [],
    )
    .expect("insert corpus");

    conn.execute(
        "INSERT INTO entities \
         (id, corpus_id, canonical_name, kind, aliases, appearance_count, confidence) \
         VALUES ('ent-default', 'corp-default', 'TestEntity', 'fn', '[]', 1, 1.0)",
        [],
    )
    .expect("insert entity with defaults");

    let kind: String = conn
        .query_row(
            "SELECT derived_at_kind FROM entities WHERE id = 'ent-default'",
            [],
            |r| r.get(0),
        )
        .expect("query derived_at_kind");

    assert_eq!(
        kind, "concrete",
        "entities.derived_at_kind must default to 'concrete'"
    );
}

// ── Test 10: entities_history unique index rejects true duplicate ─────────────

/// Inserting two `entities_history` rows with identical
/// `(corpus_id, id, derived_at_kind, derived_at_sha)` must fail with a
/// constraint error on the second insert. The first insert must succeed.
///
/// Migration 015 recreated the uniqueness index as a plain column-only index
/// `(corpus_id, id, derived_at_kind, derived_at_sha)` — no COALESCE.
#[test]
fn entities_history_unique_index_rejects_true_duplicate() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();

    // No FK on history tables, so we can insert without seeding a corpus row.
    let insert = |n: i32| -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO entities_history \
             (corpus_id, id, canonical_name, kind, aliases, appearance_count, confidence, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES ('c1', 'e1', 'EntityOne', 'fn', '[]', 1, 1.0, \
                     'concrete', 'abc', 'abc', datetime('now'))",
            rusqlite::params![],
        )
        .map(|_| n as usize)
    };

    // First insert must succeed.
    insert(1).expect("first entities_history insert must succeed");

    // Second insert with identical identity must fail.
    let result = insert(2);
    assert!(
        result.is_err(),
        "second entities_history insert with duplicate (corpus_id, id, derived_at_kind, \
         derived_at_sha) must fail with a constraint error"
    );
}

// ── Test 11: uniqueness indexes are plain column-only (no COALESCE) ───────────

/// Migration 015 dropped the COALESCE-based indexes from 013 and recreated
/// them as plain column-only indexes. Verify that none of the
/// `uq_*_history_identity` indexes reference COALESCE in `sqlite_master`.
#[test]
fn history_uniqueness_indexes_are_plain_column_only() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();

    let index_names = [
        "uq_entities_history_identity",
        "uq_edges_history_identity",
        "uq_entity_purposes_history_identity",
        "uq_entity_contracts_history_identity",
        "uq_entity_blocks_history_identity",
        "uq_summaries_history_identity",
        "uq_themes_history_identity",
        "uq_chunks_history_identity",
    ];

    for name in &index_names {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                rusqlite::params![name],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("index {name} not found in sqlite_master: {e}"));

        assert!(
            !sql.to_uppercase().contains("COALESCE"),
            "index {name} must not contain COALESCE after migration 015; got: {sql}"
        );
    }
}
