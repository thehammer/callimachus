//! Application-level history snapshotting for in-place upserts.
//!
//! Snapshotting fires on two trigger paths:
//!
//! 1. **Cascade-delete** (`cascade.rs`) — dirty chunks/entities are
//!    snapshotted and deleted as one transaction inside
//!    `SqliteBackend::cascade_delete_dirty_subtree`.
//! 2. **Upsert-replace** (this module) — surviving head rows whose
//!    `derived_at_version` is about to change are snapshotted before
//!    the in-place overwrite. Covers `--full` re-runs and
//!    structure-pass re-affirmation of existing entities.
//!
//! ## Predicate semantics
//!
//! For all helpers the snapshotting decision uses these rules (applied in
//! order — first match wins):
//!
//! | Existing `derived_at_version` | Incoming `derived_at_version` | Action |
//! |-------------------------------|-------------------------------|--------|
//! | NULL                          | any                           | skip — pre-migration row, nothing to preserve |
//! | non-null                      | NULL                          | skip — caller doesn't know the version; COALESCE keeps the existing stamp |
//! | non-null `v`                  | same `v`                      | skip — idempotent re-write within the same index run |
//! | non-null `v`                  | different `w`                 | **snapshot** — version changed |
//!
//! Each helper returns `true` when a history row was written, `false` otherwise.
//! This return value is used by pass-level metrics to distinguish cascade-driven
//! archives from upsert-replace snapshots.
//!
//! ## Concurrency
//!
//! All helpers take `&rusqlite::Connection`. In production the connection is
//! held behind `Arc<Mutex<Database>>` inside `SqliteBackend`, so only one
//! thread touches the database at a time. The SELECT + conditional INSERT is
//! therefore serialised by the mutex — no explicit transaction is needed for
//! correctness at the application level. (The snapshot is always written
//! before the upsert, so in the rare event of a crash between them only a
//! spurious — harmless — history row results.)

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{CalError, Result};

// ─────────────────────────────────────────────────────────────────────────────
// Entity
// ─────────────────────────────────────────────────────────────────────────────

/// Snapshot the head entity row into `entities_history` when its
/// `derived_at_version` is about to be replaced by a different non-null value.
///
/// `new_version` is the incoming `derived_at_version` from the upsert.
/// `superseded_at_version` is written to `entities_history.superseded_at_version`
/// (typically the same value as `new_version`).
pub(crate) fn snapshot_if_version_changed_entity(
    conn: &Connection,
    entity_id: &str,
    corpus_id: &str,
    new_version: Option<&str>,
    superseded_at_version: &str,
) -> Result<bool> {
    let Some(new_ver) = new_version else {
        return Ok(false); // incoming NULL — COALESCE keeps existing stamp
    };

    let existing: Option<String> = conn
        .query_row(
            "SELECT derived_at_version FROM entities WHERE id = ?1 AND corpus_id = ?2",
            params![entity_id, corpus_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(CalError::from)?
        .flatten();

    match existing.as_deref() {
        None => Ok(false),                    // no head row yet
        Some(v) if v == new_ver => Ok(false), // same version — idempotent
        Some(_) => {
            // Version changed — snapshot the head row.
            let now = Utc::now().to_rfc3339();
            let rows = conn.execute(
                "INSERT INTO entities_history
                   (id, corpus_id, canonical_name, kind, aliases, description,
                    first_location_uri, last_location_uri, appearance_count, confidence,
                    derived_at_version, superseded_at_version, superseded_at)
                 SELECT id, corpus_id, canonical_name, kind, aliases, description,
                        first_location_uri, last_location_uri, appearance_count, confidence,
                        derived_at_version, ?3, ?4
                 FROM entities
                 WHERE id = ?1 AND corpus_id = ?2",
                params![entity_id, corpus_id, superseded_at_version, now],
            )?;
            Ok(rows > 0)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Edge
// ─────────────────────────────────────────────────────────────────────────────

/// Edge upserts use INSERT OR IGNORE (they never overwrite), so this helper is
/// reserved for future use when edge mutation semantics are added. Suppressed to
/// avoid dead-code noise.
#[allow(dead_code)]
pub(crate) fn snapshot_if_version_changed_edge(
    conn: &Connection,
    edge_id: &str,
    corpus_id: &str,
    new_version: Option<&str>,
    superseded_at_version: &str,
) -> Result<bool> {
    let Some(new_ver) = new_version else {
        return Ok(false);
    };

    let existing: Option<String> = conn
        .query_row(
            "SELECT derived_at_version FROM edges WHERE id = ?1 AND corpus_id = ?2",
            params![edge_id, corpus_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(CalError::from)?
        .flatten();

    match existing.as_deref() {
        None => Ok(false),
        Some(v) if v == new_ver => Ok(false),
        Some(_) => {
            let now = Utc::now().to_rfc3339();
            let rows = conn.execute(
                "INSERT INTO edges_history
                   (id, corpus_id, from_entity_id, to_entity_id, kind,
                    location_uri, confidence, derived_at_version,
                    superseded_at_version, superseded_at)
                 SELECT id, corpus_id, from_entity_id, to_entity_id, kind,
                        location_uri, confidence, derived_at_version,
                        ?3, ?4
                 FROM edges
                 WHERE id = ?1 AND corpus_id = ?2",
                params![edge_id, corpus_id, superseded_at_version, now],
            )?;
            Ok(rows > 0)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EntityPurpose
// ─────────────────────────────────────────────────────────────────────────────

/// `entity_purposes` has a composite PK (entity_id, corpus_id, model).
pub(crate) fn snapshot_if_version_changed_purpose(
    conn: &Connection,
    entity_id: &str,
    corpus_id: &str,
    model: &str,
    new_version: Option<&str>,
    superseded_at_version: &str,
) -> Result<bool> {
    let Some(new_ver) = new_version else {
        return Ok(false);
    };

    let existing: Option<String> = conn
        .query_row(
            "SELECT derived_at_version FROM entity_purposes
             WHERE entity_id = ?1 AND corpus_id = ?2 AND model = ?3",
            params![entity_id, corpus_id, model],
            |r| r.get(0),
        )
        .optional()
        .map_err(CalError::from)?
        .flatten();

    match existing.as_deref() {
        None => Ok(false),
        Some(v) if v == new_ver => Ok(false),
        Some(_) => {
            let now = Utc::now().to_rfc3339();
            let rows = conn.execute(
                "INSERT INTO entity_purposes_history
                   (entity_id, corpus_id, purpose, model, model_tier, generated_at,
                    derived_at_version, superseded_at_version, superseded_at)
                 SELECT entity_id, corpus_id, purpose, model, model_tier, generated_at,
                        derived_at_version, ?4, ?5
                 FROM entity_purposes
                 WHERE entity_id = ?1 AND corpus_id = ?2 AND model = ?3",
                params![entity_id, corpus_id, model, superseded_at_version, now],
            )?;
            Ok(rows > 0)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EntityContract
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn snapshot_if_version_changed_contract(
    conn: &Connection,
    entity_id: &str,
    corpus_id: &str,
    model: &str,
    new_version: Option<&str>,
    superseded_at_version: &str,
) -> Result<bool> {
    let Some(new_ver) = new_version else {
        return Ok(false);
    };

    let existing: Option<String> = conn
        .query_row(
            "SELECT derived_at_version FROM entity_contracts
             WHERE entity_id = ?1 AND corpus_id = ?2 AND model = ?3",
            params![entity_id, corpus_id, model],
            |r| r.get(0),
        )
        .optional()
        .map_err(CalError::from)?
        .flatten();

    match existing.as_deref() {
        None => Ok(false),
        Some(v) if v == new_ver => Ok(false),
        Some(_) => {
            let now = Utc::now().to_rfc3339();
            let rows = conn.execute(
                "INSERT INTO entity_contracts_history
                   (entity_id, corpus_id,
                    is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                    is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                    panic_call_count, debt_markers, assumptions, risks,
                    intent_gap, caller_notes, model, model_tier, generated_at,
                    derived_at_version, superseded_at_version, superseded_at)
                 SELECT entity_id, corpus_id,
                        is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                        is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                        panic_call_count, debt_markers, assumptions, risks,
                        intent_gap, caller_notes, model, model_tier, generated_at,
                        derived_at_version, ?4, ?5
                 FROM entity_contracts
                 WHERE entity_id = ?1 AND corpus_id = ?2 AND model = ?3",
                params![entity_id, corpus_id, model, superseded_at_version, now],
            )?;
            Ok(rows > 0)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// EntityBlock
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn snapshot_if_version_changed_block(
    conn: &Connection,
    block_id: &str,
    corpus_id: &str,
    new_version: Option<&str>,
    superseded_at_version: &str,
) -> Result<bool> {
    let Some(new_ver) = new_version else {
        return Ok(false);
    };

    let existing: Option<String> = conn
        .query_row(
            "SELECT derived_at_version FROM entity_blocks WHERE id = ?1 AND corpus_id = ?2",
            params![block_id, corpus_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(CalError::from)?
        .flatten();

    match existing.as_deref() {
        None => Ok(false),
        Some(v) if v == new_ver => Ok(false),
        Some(_) => {
            let now = Utc::now().to_rfc3339();
            let rows = conn.execute(
                "INSERT INTO entity_blocks_history
                   (id, entity_id, corpus_id, label, description, position,
                    derived_at_version, superseded_at_version, superseded_at)
                 SELECT id, entity_id, corpus_id, label, description, position,
                        derived_at_version, ?3, ?4
                 FROM entity_blocks
                 WHERE id = ?1 AND corpus_id = ?2",
                params![block_id, corpus_id, superseded_at_version, now],
            )?;
            Ok(rows > 0)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Summary
// ─────────────────────────────────────────────────────────────────────────────

/// `summaries` PK is (corpus_id, target_kind, target_id, model).
pub(crate) fn snapshot_if_version_changed_summary(
    conn: &Connection,
    corpus_id: &str,
    target_kind: &str,
    target_id: &str,
    model: &str,
    new_version: Option<&str>,
    superseded_at_version: &str,
) -> Result<bool> {
    let Some(new_ver) = new_version else {
        return Ok(false);
    };

    let existing: Option<String> = conn
        .query_row(
            "SELECT derived_at_version FROM summaries
             WHERE corpus_id = ?1 AND target_kind = ?2 AND target_id = ?3 AND model = ?4",
            params![corpus_id, target_kind, target_id, model],
            |r| r.get(0),
        )
        .optional()
        .map_err(CalError::from)?
        .flatten();

    match existing.as_deref() {
        None => Ok(false),
        Some(v) if v == new_ver => Ok(false),
        Some(_) => {
            let now = Utc::now().to_rfc3339();
            let rows = conn.execute(
                "INSERT INTO summaries_history
                   (id, corpus_id, target_kind, target_id, depth, text,
                    model, model_tier, generated_at,
                    derived_at_version, superseded_at_version, superseded_at)
                 SELECT id, corpus_id, target_kind, target_id, depth, text,
                        model, model_tier, generated_at,
                        derived_at_version, ?5, ?6
                 FROM summaries
                 WHERE corpus_id = ?1 AND target_kind = ?2 AND target_id = ?3 AND model = ?4",
                params![
                    corpus_id,
                    target_kind,
                    target_id,
                    model,
                    superseded_at_version,
                    now
                ],
            )?;
            Ok(rows > 0)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Theme
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) fn snapshot_if_version_changed_theme(
    conn: &Connection,
    theme_id: &str,
    corpus_id: &str,
    new_version: Option<&str>,
    superseded_at_version: &str,
) -> Result<bool> {
    let Some(new_ver) = new_version else {
        return Ok(false);
    };

    let existing: Option<String> = conn
        .query_row(
            "SELECT derived_at_version FROM themes WHERE id = ?1 AND corpus_id = ?2",
            params![theme_id, corpus_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(CalError::from)?
        .flatten();

    match existing.as_deref() {
        None => Ok(false),
        Some(v) if v == new_ver => Ok(false),
        Some(_) => {
            let now = Utc::now().to_rfc3339();
            let rows = conn.execute(
                "INSERT INTO themes_history
                   (id, corpus_id, title, statement, confidence,
                    model, model_tier, generated_at,
                    derived_at_version, superseded_at_version, superseded_at)
                 SELECT id, corpus_id, title, statement, confidence,
                        model, model_tier, generated_at,
                        derived_at_version, ?3, ?4
                 FROM themes
                 WHERE id = ?1 AND corpus_id = ?2",
                params![theme_id, corpus_id, superseded_at_version, now],
            )?;
            Ok(rows > 0)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cascade-delete archive helpers
//
// Unlike the `snapshot_if_version_changed_*` helpers above, these functions
// always write a history row — they are called just before a head row (or its
// whole entity sub-tree) is hard-deleted during cascade invalidation.
// ─────────────────────────────────────────────────────────────────────────────

/// Archive a single entity row into `entities_history`.
/// Returns `true` if a history row was inserted (i.e. the entity existed).
pub(crate) fn archive_entity(
    conn: &Connection,
    entity_id: &str,
    corpus_id: &str,
    superseded_at_version: &str,
) -> Result<bool> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO entities_history
           (id, corpus_id, canonical_name, kind, aliases, description,
            first_location_uri, last_location_uri, appearance_count, confidence,
            derived_at_version, superseded_at_version, superseded_at)
         SELECT id, corpus_id, canonical_name, kind, aliases, description,
                first_location_uri, last_location_uri, appearance_count, confidence,
                derived_at_version, ?3, ?4
         FROM entities
         WHERE id = ?1 AND corpus_id = ?2",
        params![entity_id, corpus_id, superseded_at_version, now],
    )?;
    Ok(rows > 0)
}

/// Archive all edge rows (in both directions) that involve `entity_id`.
/// Returns the number of history rows inserted.
pub(crate) fn archive_edges_for_entity(
    conn: &Connection,
    entity_id: &str,
    superseded_at_version: &str,
) -> Result<u64> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO edges_history
           (id, corpus_id, from_entity_id, to_entity_id, kind,
            location_uri, confidence, derived_at_version,
            superseded_at_version, superseded_at)
         SELECT id, corpus_id, from_entity_id, to_entity_id, kind,
                location_uri, confidence, derived_at_version,
                ?2, ?3
         FROM edges
         WHERE from_entity_id = ?1 OR to_entity_id = ?1",
        params![entity_id, superseded_at_version, now],
    )?;
    Ok(rows as u64)
}

/// Archive all purpose rows for `entity_id`.
/// Returns the number of history rows inserted.
pub(crate) fn archive_purposes_for_entity(
    conn: &Connection,
    entity_id: &str,
    superseded_at_version: &str,
) -> Result<u64> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO entity_purposes_history
           (entity_id, corpus_id, purpose, model, model_tier, generated_at,
            derived_at_version, superseded_at_version, superseded_at)
         SELECT entity_id, corpus_id, purpose, model, model_tier, generated_at,
                derived_at_version, ?2, ?3
         FROM entity_purposes
         WHERE entity_id = ?1",
        params![entity_id, superseded_at_version, now],
    )?;
    Ok(rows as u64)
}

/// Archive all contract rows for `entity_id`.
/// Returns the number of history rows inserted.
pub(crate) fn archive_contracts_for_entity(
    conn: &Connection,
    entity_id: &str,
    superseded_at_version: &str,
) -> Result<u64> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO entity_contracts_history
           (entity_id, corpus_id,
            is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
            is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
            panic_call_count, debt_markers, assumptions, risks,
            intent_gap, caller_notes, model, model_tier, generated_at,
            derived_at_version, superseded_at_version, superseded_at)
         SELECT entity_id, corpus_id,
                is_public, is_must_use, is_deprecated, is_fallible, is_nullable,
                is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete,
                panic_call_count, debt_markers, assumptions, risks,
                intent_gap, caller_notes, model, model_tier, generated_at,
                derived_at_version, ?2, ?3
         FROM entity_contracts
         WHERE entity_id = ?1",
        params![entity_id, superseded_at_version, now],
    )?;
    Ok(rows as u64)
}

/// Archive all block rows for `entity_id`.
/// Returns the number of history rows inserted.
pub(crate) fn archive_blocks_for_entity(
    conn: &Connection,
    entity_id: &str,
    superseded_at_version: &str,
) -> Result<u64> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO entity_blocks_history
           (id, entity_id, corpus_id, label, description, position,
            derived_at_version, superseded_at_version, superseded_at)
         SELECT id, entity_id, corpus_id, label, description, position,
                derived_at_version, ?2, ?3
         FROM entity_blocks
         WHERE entity_id = ?1",
        params![entity_id, superseded_at_version, now],
    )?;
    Ok(rows as u64)
}

/// Archive all summary rows for `target_id` within `corpus_id`.
/// Returns the number of history rows inserted.
pub(crate) fn archive_summaries_for_target(
    conn: &Connection,
    corpus_id: &str,
    target_id: &str,
    superseded_at_version: &str,
) -> Result<u64> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO summaries_history
           (id, corpus_id, target_kind, target_id, depth, text,
            model, model_tier, generated_at,
            derived_at_version, superseded_at_version, superseded_at)
         SELECT id, corpus_id, target_kind, target_id, depth, text,
                model, model_tier, generated_at,
                derived_at_version, ?3, ?4
         FROM summaries
         WHERE corpus_id = ?1 AND target_id = ?2",
        params![corpus_id, target_id, superseded_at_version, now],
    )?;
    Ok(rows as u64)
}

/// Archive a single chunk row into `chunks_history`.
/// Returns `true` if a history row was inserted (i.e. the chunk existed).
pub(crate) fn archive_chunk(
    conn: &Connection,
    chunk_id: &str,
    superseded_at_version: &str,
) -> Result<bool> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO chunks_history
           (id, corpus_id, parent_path, kind, location_uri, content,
            byte_length, created_at, semantic_processed, source_hash,
            introduced_at_version, last_modified_at_version,
            last_modified_commit_message, last_modified_author,
            superseded_at_version, superseded_at)
         SELECT id, corpus_id, parent_path, kind, location_uri, content,
                byte_length, created_at, semantic_processed, source_hash,
                introduced_at_version, last_modified_at_version,
                last_modified_commit_message, last_modified_author,
                ?2, ?3
         FROM chunks
         WHERE id = ?1",
        params![chunk_id, superseded_at_version, now],
    )?;
    Ok(rows > 0)
}

/// Archive a single theme row into `themes_history`.
/// Returns `true` if a history row was inserted (i.e. the theme existed).
pub(crate) fn archive_theme(
    conn: &Connection,
    theme_id: &str,
    corpus_id: &str,
    superseded_at_version: &str,
) -> Result<bool> {
    let now = Utc::now().to_rfc3339();
    let rows = conn.execute(
        "INSERT INTO themes_history
           (id, corpus_id, title, statement, confidence,
            model, model_tier, generated_at,
            derived_at_version, superseded_at_version, superseded_at)
         SELECT id, corpus_id, title, statement, confidence,
                model, model_tier, generated_at,
                derived_at_version, ?3, ?4
         FROM themes
         WHERE id = ?1 AND corpus_id = ?2",
        params![theme_id, corpus_id, superseded_at_version, now],
    )?;
    Ok(rows > 0)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::storage::{Database, SqliteBackend, StorageBackend};
    use crate::types::Entity;

    #[allow(dead_code)]
    fn open() -> (SqliteBackend, Database) {
        let db_backend = SqliteBackend::open_in_memory().unwrap();
        let db_raw = Database::open_in_memory().unwrap();
        (db_backend, db_raw)
    }

    fn seed_corpus_and_entity(db: &SqliteBackend, corpus_id: &str, entity_id: &str, version: &str) {
        use crate::types::Corpus;
        let mut corpus = Corpus::new(
            corpus_id.to_string(),
            "Test".to_string(),
            "code".to_string(),
            "/tmp".to_string(),
        );
        corpus.last_indexed_version = Some(version.to_string());
        db.corpus_insert(&corpus).unwrap();

        let entity = Entity {
            id: entity_id.to_string(),
            corpus_id: corpus_id.to_string(),
            canonical_name: "TestEntity".to_string(),
            kind: "function".to_string(),
            derived_at_version: Some(version.to_string()),
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();
    }

    // ── entity snapshot predicate tests ──────────────────────────────────────

    /// UPSERT with a new derived_at_version snapshots the old row.
    #[test]
    fn entity_upsert_with_changed_version_snapshots_old_row() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus_and_entity(&db, "corp", "ent-1", "git:v1");

        // Upsert same entity with a different version.
        let entity = Entity {
            id: "ent-1".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "TestEntity".to_string(),
            kind: "function".to_string(),
            derived_at_version: Some("git:v2".to_string()),
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();

        // Verify history row exists.
        let guard = db.db_for_test();
        let count: i64 = guard
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM entities_history WHERE id = 'ent-1' AND superseded_at_version = 'git:v2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "expected 1 history row after version change");
    }

    /// UPSERT with the same derived_at_version does NOT snapshot.
    #[test]
    fn entity_upsert_with_same_version_does_not_snapshot() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus_and_entity(&db, "corp", "ent-2", "git:v1");

        let entity = Entity {
            id: "ent-2".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "TestEntity".to_string(),
            kind: "function".to_string(),
            derived_at_version: Some("git:v1".to_string()),
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();

        let guard = db.db_for_test();
        let count: i64 = guard
            .conn()
            .query_row("SELECT COUNT(*) FROM entities_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "same version should not produce history row");
    }

    /// UPSERT with None incoming version does NOT snapshot; existing stamp preserved.
    #[test]
    fn entity_upsert_with_null_incoming_does_not_snapshot() {
        let db = SqliteBackend::open_in_memory().unwrap();
        seed_corpus_and_entity(&db, "corp", "ent-3", "git:v1");

        let entity = Entity {
            id: "ent-3".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "TestEntity".to_string(),
            kind: "function".to_string(),
            derived_at_version: None, // caller doesn't know version
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();

        let guard = db.db_for_test();
        // No history row.
        let hist: i64 = guard
            .conn()
            .query_row("SELECT COUNT(*) FROM entities_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hist, 0);
        // COALESCE should have preserved "git:v1".
        let stamp: Option<String> = guard
            .conn()
            .query_row(
                "SELECT derived_at_version FROM entities WHERE id = 'ent-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stamp.as_deref(),
            Some("git:v1"),
            "existing stamp must be preserved"
        );
    }

    /// UPSERT against a pre-migration row (NULL existing) does NOT snapshot.
    #[test]
    fn entity_upsert_with_null_existing_does_not_snapshot() {
        use crate::types::Corpus;
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "corp".to_string(),
            "T".to_string(),
            "code".to_string(),
            "/tmp".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        // Insert entity without derived_at_version (simulates pre-migration row).
        let entity = Entity {
            id: "ent-4".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "Old".to_string(),
            kind: "function".to_string(),
            derived_at_version: None,
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();

        // Now upsert with a real version — should stamp, not snapshot.
        let entity2 = Entity {
            derived_at_version: Some("git:v1".to_string()),
            ..entity.clone()
        };
        db.entity_upsert(&entity2).unwrap();

        let guard = db.db_for_test();
        let hist: i64 = guard
            .conn()
            .query_row("SELECT COUNT(*) FROM entities_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hist, 0, "NULL existing should not produce history row");
    }

    // ── derived_at_version stamping ──────────────────────────────────────────

    /// derived_at_version round-trips through upsert + read.
    #[test]
    fn derived_at_version_stamped_on_upsert() {
        use crate::types::Corpus;
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "corp".to_string(),
            "T".to_string(),
            "code".to_string(),
            "/tmp".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        let entity = Entity {
            id: "ent-5".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "E".to_string(),
            kind: "function".to_string(),
            derived_at_version: Some("git:abc".to_string()),
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();

        let fetched = db.entity_get_by_id("ent-5").unwrap().unwrap();
        assert_eq!(fetched.derived_at_version.as_deref(), Some("git:abc"));
    }

    /// COALESCE: upsert with None after a real stamp preserves the real stamp.
    #[test]
    fn derived_at_version_coalesces_on_upsert() {
        use crate::types::Corpus;
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "corp".to_string(),
            "T".to_string(),
            "code".to_string(),
            "/tmp".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        let entity = Entity {
            id: "ent-6".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "E".to_string(),
            kind: "function".to_string(),
            derived_at_version: Some("git:abc".to_string()),
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();

        // Second upsert with None — should preserve "git:abc".
        let entity2 = Entity {
            derived_at_version: None,
            ..entity
        };
        db.entity_upsert(&entity2).unwrap();

        let fetched = db.entity_get_by_id("ent-6").unwrap().unwrap();
        assert_eq!(fetched.derived_at_version.as_deref(), Some("git:abc"));
    }
}
