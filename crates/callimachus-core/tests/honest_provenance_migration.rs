//! RED-phase behavioral tests for migration 013 (honest provenance schema).
//!
//! Opens a fresh in-memory database (which runs all migrations through 013)
//! and asserts the resulting schema shape via `PRAGMA table_info(...)`,
//! `PRAGMA index_list(...)`, and `sqlite_master` queries. Also verifies the
//! backfill-SQL contract and the history-table uniqueness constraint.
//!
//! All tests use `Database::open_in_memory()` directly so they can reach the
//! raw `rusqlite::Connection` via `db.conn()`. No `StorageBackend` methods are
//! exercised here — this file is purely schema-level.
//!
//! # Coverage
//!
//! 1. `entities_head_table_has_provenance_and_content_hash_columns` — the three
//!    new columns on `entities` exist after migration 013.
//! 2. `chunks_head_table_has_new_columns` — `chunks` gained `derived_at_kind`,
//!    `derived_at_sha`, `file_shape_hash`, and `entity_id_list`.
//! 3. `embeddings_head_table_has_provenance_and_context_hash_columns` — the
//!    three new columns on `embeddings` exist.
//! 4. `all_head_tables_have_provenance_column_pair` — every head table that
//!    participated in Part 1 of the migration has both `derived_at_kind` and
//!    `derived_at_sha`.
//! 5. `legacy_derived_at_version_retained_on_entities` — `entities` still has
//!    the backward-compat `derived_at_version` column from migration 012.
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
//! 10. `backfill_sql_copies_legacy_sha_into_derived_at_sha` — the exact UPDATE
//!     SQL shape from Part 8 of the migration propagates `derived_at_version`
//!     into `derived_at_sha` for rows that still have `derived_at_sha=''`.
//! 11. `entities_history_unique_index_rejects_true_duplicate` — inserting two
//!     `entities_history` rows with identical `(corpus_id, id, derived_at_kind,
//!     derived_at_sha)` where `derived_at_sha` is non-empty fails with a
//!     constraint error on the second insert.

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

// ── Test 5: legacy derived_at_version is retained on entities ────────────────

/// Migration 013 retains `entities.derived_at_version` for backward compat.
/// It must still be present after migration 013 runs.
#[test]
fn legacy_derived_at_version_retained_on_entities() {
    let db = Database::open_in_memory().unwrap();

    assert!(
        has_column(&db, "entities", "derived_at_version"),
        "entities must still have derived_at_version (legacy compat) after migration 013"
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

// ── Test 10: backfill SQL copies legacy SHA into derived_at_sha ───────────────

/// The Part-8 backfill statement `UPDATE entities SET derived_at_sha =
/// COALESCE(derived_at_version,'') WHERE derived_at_sha=''` must propagate
/// `derived_at_version` into `derived_at_sha` for rows that still have the
/// empty default.
///
/// On a fresh DB the migration has already run, so we simulate the pre-backfill
/// state by inserting a row with an explicit `derived_at_sha=''` (overriding the
/// default) and `derived_at_version='deadbeef'`, then re-running the backfill
/// statement.
#[test]
fn backfill_sql_copies_legacy_sha_into_derived_at_sha() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();

    conn.execute(
        "INSERT INTO corpora (id, name, kind, source, status, created_at) \
         VALUES ('corp-backfill', 'BF Corp', 'code', '/tmp', 'registered', datetime('now'))",
        [],
    )
    .expect("insert corpus");

    // Insert with explicit derived_at_sha='' and derived_at_version='deadbeef'
    // to simulate a row written before the backfill ran.
    conn.execute(
        "INSERT INTO entities \
         (id, corpus_id, canonical_name, kind, aliases, appearance_count, confidence, \
          derived_at_version, derived_at_sha) \
         VALUES ('ent-bf', 'corp-backfill', 'BFEntity', 'fn', '[]', 1, 1.0, 'deadbeef', '')",
        [],
    )
    .expect("insert entity with blank derived_at_sha");

    // Re-run the Part-8 backfill SQL from migration 013.
    conn.execute(
        "UPDATE entities SET derived_at_sha = COALESCE(derived_at_version, '') \
         WHERE derived_at_sha = ''",
        [],
    )
    .expect("backfill UPDATE must succeed");

    let sha: String = conn
        .query_row(
            "SELECT derived_at_sha FROM entities WHERE id = 'ent-bf'",
            [],
            |r| r.get(0),
        )
        .expect("query derived_at_sha after backfill");

    assert_eq!(
        sha, "deadbeef",
        "derived_at_sha must equal the legacy derived_at_version after backfill"
    );

    let kind: String = conn
        .query_row(
            "SELECT derived_at_kind FROM entities WHERE id = 'ent-bf'",
            [],
            |r| r.get(0),
        )
        .expect("query derived_at_kind after backfill");

    assert_eq!(
        kind, "concrete",
        "derived_at_kind must remain 'concrete' (the default) after backfill"
    );
}

// ── Test 11: entities_history unique index rejects true duplicate ─────────────

/// Inserting two `entities_history` rows with identical
/// `(corpus_id, id, derived_at_kind, derived_at_sha)` — where `derived_at_sha`
/// is NON-empty — must fail with a constraint error on the second insert.
/// The first insert must succeed.
///
/// The uniqueness index from migration 013 is
/// `uq_entities_history_identity` keyed on
/// `(corpus_id, id, derived_at_kind, COALESCE(NULLIF(derived_at_sha,''),
/// derived_at_version, ''))`. For a non-empty `derived_at_sha` the COALESCE
/// resolves to `derived_at_sha`, so duplicates are rejected.
#[test]
fn entities_history_unique_index_rejects_true_duplicate() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn();

    // No FK on history tables (per migration 012 comments), so we can insert
    // without seeding a corpus row.
    let insert = |n: i32| -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO entities_history \
             (corpus_id, id, canonical_name, kind, aliases, appearance_count, confidence, \
              derived_at_kind, derived_at_sha, superseded_at_version, superseded_at) \
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
