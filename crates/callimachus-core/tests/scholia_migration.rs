//! RED-phase behavioral tests for the scholia migration (migration 009).
//!
//! These tests define the expected behavior after Phase 2 of the Pinakes
//! terminology migration:
//!
//! - Migration 009 renames the `corrections` table to `scholia`.
//! - `correction_store.rs` SQL is updated to reference `scholia`.
//! - The `Correction`/`CorrectionKind` Rust types and `StorageBackend` methods
//!   (`correction_insert`, `correction_list`, `correction_delete`, etc.) are
//!   unchanged — only the underlying SQL table name changes.
//!
//! All tests will fail until migration 009 is applied and `correction_store.rs`
//! references `scholia` instead of `corrections`. That is intentional: these
//! tests are the contract that the implementation must satisfy.
//!
//! # Coverage
//!
//! 1. `corrections_table_does_not_exist_after_migration_009` — schema-level:
//!    the old table name must be absent.
//! 2. `scholia_table_exists_after_migration_009` — schema-level:
//!    the new table name must be present.
//! 3. `scholion_insert_is_readable_via_correction_list` — API round-trip:
//!    insert + list works end-to-end.
//! 4. `multiple_scholia_round_trip` — list returns all rows.
//! 5. `scholia_are_scoped_by_corpus` — corpus scoping is preserved.
//! 6. `scholion_deletion_removes_targeted_row` — delete by ID works.
//! 7. `correction_delete_returns_false_for_unknown_id` — deletion contract
//!    for a missing ID.
//! 8. `list_all_returns_scholia_across_corpora` — list_all aggregates
//!    across every corpus.
//! 9. `merge_correction_round_trips_via_scholia` — non-Rename variant
//!    survives the table rename.

use callimachus_core::{
    Database,
    corrections::types::CorrectionKind,
    storage::{SqliteBackend, StorageBackend},
    types::Corpus,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_backend() -> SqliteBackend {
    SqliteBackend::open_in_memory().unwrap()
}

/// Insert a minimal corpus row so FK constraints on `scholia.corpus_id` are
/// satisfied.  Silently ignores a duplicate-corpus error.
fn seed_corpus(db: &dyn StorageBackend, corpus_id: &str) {
    let corpus = Corpus::new(
        corpus_id.to_string(),
        format!("{corpus_id} corpus"),
        "code".to_string(),
        "/tmp/dummy".to_string(),
    );
    let _ = db.corpus_insert(&corpus);
}

fn rename_kind(entity_id: &str, new_name: &str) -> CorrectionKind {
    CorrectionKind::Rename {
        entity_id: entity_id.to_string(),
        new_name: new_name.to_string(),
    }
}

// ── Test 1: the `corrections` table must not exist after migration 009 ─────────

/// After all migrations (including 009) run, the table named `corrections` must
/// be absent from the schema. The data lives in `scholia`.
///
/// Uses `Database::open_in_memory()` directly to access the raw rusqlite
/// connection and query `sqlite_master`.  `Database` is re-exported from
/// `callimachus_core`, so no crate-private access is required.
///
/// If migration 009 has not yet been written, this test fails because the
/// `corrections` table still exists.
#[test]
fn corrections_table_does_not_exist_after_migration_009() {
    let db = Database::open_in_memory().unwrap();

    let old_table_exists: bool = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='corrections'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0;

    assert!(
        !old_table_exists,
        "table 'corrections' must not exist after migration 009 renames it to 'scholia'"
    );
}

// ── Test 2: the `scholia` table must exist after migration 009 ────────────────

/// After all migrations (including 009) run, a table named `scholia` must be
/// present and queryable.
///
/// If migration 009 has not yet been written, this test fails because no
/// `scholia` table was ever created.
#[test]
fn scholia_table_exists_after_migration_009() {
    let db = Database::open_in_memory().unwrap();

    let new_table_exists: bool = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='scholia'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0;

    assert!(
        new_table_exists,
        "table 'scholia' must exist after migration 009 renames 'corrections'"
    );
}

// ── Test 3: scholion insert is readable via correction_list ───────────────────

/// Inserting a correction (scholion) and reading it back via the public API
/// should work regardless of the underlying table name.
///
/// Pre-migration: the data lives in `corrections`.
/// Post-migration: the data lives in `scholia`.
/// The StorageBackend contract must hold in both cases.
#[test]
fn scholion_insert_is_readable_via_correction_list() {
    let db = make_backend();
    let corpus_id = "corpus1";
    seed_corpus(&db, corpus_id);

    db.correction_insert(Some(corpus_id), None, &rename_kind("e1", "NewName"))
        .expect("correction_insert should succeed");

    let scholia = db
        .correction_list(corpus_id)
        .expect("correction_list should succeed");

    assert_eq!(
        scholia.len(),
        1,
        "expected exactly one scholion after a single insert"
    );

    let s = &scholia[0];
    assert_eq!(s.corpus_id.as_deref(), Some(corpus_id));
    match &s.kind {
        CorrectionKind::Rename {
            entity_id,
            new_name,
        } => {
            assert_eq!(entity_id, "e1");
            assert_eq!(new_name, "NewName");
        }
        other => panic!("unexpected kind: {:?}", other),
    }
}

// ── Test 4: multiple scholia round-trip in insertion order ────────────────────

/// The scholia table stores an append-only log; `correction_list` returns rows
/// ordered by `applied_at ASC`.  Insert three corrections and verify all three
/// come back.
#[test]
fn multiple_scholia_round_trip() {
    let db = make_backend();
    let corpus_id = "corpus2";
    seed_corpus(&db, corpus_id);

    db.correction_insert(Some(corpus_id), None, &rename_kind("e1", "Alpha"))
        .unwrap();
    db.correction_insert(Some(corpus_id), None, &rename_kind("e2", "Beta"))
        .unwrap();
    db.correction_insert(Some(corpus_id), None, &rename_kind("e3", "Gamma"))
        .unwrap();

    let scholia = db.correction_list(corpus_id).unwrap();
    assert_eq!(
        scholia.len(),
        3,
        "expected three scholia after three inserts"
    );

    // Verify payload content for each row.
    let names: Vec<&str> = scholia
        .iter()
        .map(|s| match &s.kind {
            CorrectionKind::Rename { new_name, .. } => new_name.as_str(),
            other => panic!("unexpected kind: {:?}", other),
        })
        .collect();

    assert!(names.contains(&"Alpha"), "Alpha scholion not found");
    assert!(names.contains(&"Beta"), "Beta scholion not found");
    assert!(names.contains(&"Gamma"), "Gamma scholion not found");
}

// ── Test 5: scholia from different corpora are scoped correctly ───────────────

/// `correction_list` must only return scholia belonging to the requested corpus.
/// Data inserted for corpus B must not appear when listing corpus A.
#[test]
fn scholia_are_scoped_by_corpus() {
    let db = make_backend();
    seed_corpus(&db, "corpus-a");
    seed_corpus(&db, "corpus-b");

    db.correction_insert(Some("corpus-a"), None, &rename_kind("e1", "ForA"))
        .unwrap();
    db.correction_insert(Some("corpus-b"), None, &rename_kind("e2", "ForB"))
        .unwrap();

    let a_scholia = db.correction_list("corpus-a").unwrap();
    let b_scholia = db.correction_list("corpus-b").unwrap();

    assert_eq!(
        a_scholia.len(),
        1,
        "corpus-a should have exactly one scholion"
    );
    assert_eq!(
        b_scholia.len(),
        1,
        "corpus-b should have exactly one scholion"
    );

    match &a_scholia[0].kind {
        CorrectionKind::Rename { new_name, .. } => assert_eq!(new_name, "ForA"),
        other => panic!("unexpected kind: {:?}", other),
    }
    match &b_scholia[0].kind {
        CorrectionKind::Rename { new_name, .. } => assert_eq!(new_name, "ForB"),
        other => panic!("unexpected kind: {:?}", other),
    }
}

// ── Test 6: deletion removes exactly the targeted scholion ────────────────────

/// `correction_delete` removes the row by ID.  After deletion, `correction_list`
/// must no longer return that scholion.  Other scholia for the same corpus
/// must be unaffected.
#[test]
fn scholion_deletion_removes_targeted_row() {
    let db = make_backend();
    let corpus_id = "corpus3";
    seed_corpus(&db, corpus_id);

    let id_to_delete = db
        .correction_insert(Some(corpus_id), None, &rename_kind("e1", "WillBeDeleted"))
        .unwrap();

    db.correction_insert(Some(corpus_id), None, &rename_kind("e2", "Survivor"))
        .unwrap();

    // Two scholia exist.
    assert_eq!(db.correction_list(corpus_id).unwrap().len(), 2);

    let deleted = db.correction_delete(&id_to_delete).unwrap();
    assert!(
        deleted,
        "correction_delete should return true for a known ID"
    );

    let remaining = db.correction_list(corpus_id).unwrap();
    assert_eq!(
        remaining.len(),
        1,
        "one scholion should remain after deletion"
    );
    match &remaining[0].kind {
        CorrectionKind::Rename { new_name, .. } => assert_eq!(new_name, "Survivor"),
        other => panic!("unexpected kind: {:?}", other),
    }
}

// ── Test 8: list_all returns scholia across all corpora ───────────────────────

/// `correction_list_all` aggregates scholia from every corpus.
#[test]
fn list_all_returns_scholia_across_corpora() {
    let db = make_backend();
    seed_corpus(&db, "cx");
    seed_corpus(&db, "cy");

    db.correction_insert(Some("cx"), None, &rename_kind("e1", "X"))
        .unwrap();
    db.correction_insert(Some("cy"), None, &rename_kind("e2", "Y"))
        .unwrap();

    let all = db.correction_list_all().unwrap();
    assert!(
        all.len() >= 2,
        "correction_list_all should return at least 2 scholia"
    );
}

// ── Test 7: deletion of a nonexistent ID returns false ───────────────────────

/// `correction_delete` must return `false` when the supplied ID does not match
/// any row. It must not return an error.
#[test]
fn correction_delete_returns_false_for_unknown_id() {
    let db = make_backend();

    let deleted = db
        .correction_delete("00000000-0000-0000-0000-nonexistent")
        .unwrap();

    assert!(
        !deleted,
        "correction_delete must return false when the ID does not exist"
    );
}

// ── Test 8: Merge correction survives the table rename ────────────────────────

/// All `CorrectionKind` variants must round-trip through the `scholia` table.
/// Verify the `Merge` variant explicitly because it carries three entity IDs
/// (entity_a_id, entity_b_id, canonical_id) and is representative of the
/// JSON-payload serialisation surviving the rename.
#[test]
fn merge_correction_round_trips_via_scholia() {
    let db = make_backend();
    let corpus_id = "corpus-merge";
    seed_corpus(&db, corpus_id);

    let kind = CorrectionKind::Merge {
        entity_a_id: "ea".to_string(),
        entity_b_id: "eb".to_string(),
        canonical_id: "ea".to_string(),
    };
    db.correction_insert(Some(corpus_id), None, &kind).unwrap();

    let scholia = db.correction_list(corpus_id).unwrap();
    assert_eq!(scholia.len(), 1, "one Merge scholion must be stored");

    match &scholia[0].kind {
        CorrectionKind::Merge {
            entity_a_id,
            entity_b_id,
            canonical_id,
        } => {
            assert_eq!(entity_a_id, "ea", "entity_a_id must be preserved");
            assert_eq!(entity_b_id, "eb", "entity_b_id must be preserved");
            assert_eq!(canonical_id, "ea", "canonical_id must be preserved");
        }
        other => panic!("expected Merge scholion, got: {:?}", other),
    }
}

// ── Deprecation warning (CLI integration test note) ───────────────────────────
//
// The `calli correct …` command must emit a deprecation warning to stderr:
//   warning: 'calli correct' is deprecated, use 'calli scholion apply'
//
// This is best tested by spawning the `calli` binary and capturing its stderr.
// No binary-spawning test infrastructure currently exists in this codebase.
// When CLI integration test infra is added, cover this behavior with:
//
//   let output = std::process::Command::new("calli")
//       .args(["--db", ":memory:", "correct", "corp1", "rename", "e1", "NewName"])
//       .output()
//       .unwrap();
//   let stderr = String::from_utf8_lossy(&output.stderr);
//   assert!(
//       stderr.contains("deprecated"),
//       "calli correct must print a deprecation warning to stderr"
//   );
