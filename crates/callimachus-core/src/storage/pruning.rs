//! History pruning — `prune_history` storage-level types and tests.
//!
//! This module defines [`PruneStats`], the return type for
//! [`StorageBackend::prune_history`].  The implementation lives in
//! `sqlite.rs`; this file also hosts the storage-level integration tests.

/// Statistics returned by [`StorageBackend::prune_history`].
///
/// All counts reflect the rows (or SHAs) that were deleted (real run) or
/// *would* be deleted (dry run).
#[derive(Debug, Default)]
pub struct PruneStats {
    /// Number of distinct `superseded_at_sha` SHAs that were retained.
    pub supersession_shas_kept: usize,
    /// Number of distinct `superseded_at_sha` SHAs that were pruned.
    pub supersession_shas_pruned: usize,
    /// Rows removed (or that would be removed) from `entities_history`.
    pub rows_pruned_entities_history: usize,
    /// Rows removed (or that would be removed) from `edges_history`.
    pub rows_pruned_edges_history: usize,
    /// Rows removed (or that would be removed) from `entity_purposes_history`.
    pub rows_pruned_entity_purposes_history: usize,
    /// Rows removed (or that would be removed) from `entity_contracts_history`.
    pub rows_pruned_entity_contracts_history: usize,
    /// Rows removed (or that would be removed) from `entity_blocks_history`.
    pub rows_pruned_entity_blocks_history: usize,
    /// Rows removed (or that would be removed) from `summaries_history`.
    pub rows_pruned_summaries_history: usize,
    /// Rows removed (or that would be removed) from `chunks_history`.
    pub rows_pruned_chunks_history: usize,
    /// Rows removed (or that would be removed) from `themes_history`.
    pub rows_pruned_themes_history: usize,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::storage::StorageBackend;
    use crate::storage::sqlite::SqliteBackend;
    use crate::types::Corpus;

    /// Helper: create an in-memory SQLite backend with a seeded corpus.
    fn make_backend_with_corpus(corpus_id: &str) -> SqliteBackend {
        let backend = SqliteBackend::open_in_memory().expect("in-memory backend");
        let corpus = Corpus::new(
            corpus_id.to_string(),
            corpus_id.to_string(),
            "code".to_string(),
            "/tmp/fake".to_string(),
        );
        backend.corpus_insert(&corpus).expect("insert corpus");
        backend
    }

    /// Insert one row into each of the 8 `*_history` tables for the given
    /// corpus / supersession SHA.  Uses the exact column names from migration 012.
    fn insert_history_rows(
        backend: &SqliteBackend,
        corpus_id: &str,
        sha: &str,
        superseded_at: &str,
    ) {
        let db = backend.db_for_test();
        let conn = db.conn();

        // entities_history
        conn.execute(
            "INSERT INTO entities_history \
             (corpus_id, id, canonical_name, kind, aliases, appearance_count, confidence, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, ?3, 'fn', '[]', 1, 1.0, 'concrete', ?4, ?4, ?5)",
            rusqlite::params![
                corpus_id,
                format!("ent-{corpus_id}-{sha}"),
                format!("Entity_{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert entities_history");

        // edges_history
        conn.execute(
            "INSERT INTO edges_history \
             (corpus_id, id, from_entity_id, to_entity_id, kind, location_uri, confidence, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, ?3, ?3, 'calls', 'file://loc', 1.0, 'concrete', ?4, ?4, ?5)",
            rusqlite::params![
                corpus_id,
                format!("edge-{corpus_id}-{sha}"),
                format!("ent-{corpus_id}-{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert edges_history");

        // entity_purposes_history (no `id` column)
        conn.execute(
            "INSERT INTO entity_purposes_history \
             (corpus_id, entity_id, purpose, model, model_tier, generated_at, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, 'does things', 'gpt-4', 'standard', '2024-01-01T00:00:00Z', \
                     'concrete', ?3, ?3, ?4)",
            rusqlite::params![
                corpus_id,
                format!("ent-{corpus_id}-{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert entity_purposes_history");

        // entity_contracts_history (no `id` column)
        conn.execute(
            "INSERT INTO entity_contracts_history \
             (corpus_id, entity_id, \
              is_public, is_must_use, is_deprecated, is_fallible, is_nullable, \
              is_mutating, is_diverging, has_panic_risk, has_unsafe, is_incomplete, \
              panic_call_count, debt_markers, assumptions, risks, \
              model, model_tier, generated_at, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, \
                     0, 0, 0, 0, 0, \
                     0, 0, 0, 0, 0, \
                     0, '[]', '[]', '[]', \
                     'gpt-4', 'standard', '2024-01-01T00:00:00Z', \
                     'concrete', ?3, ?3, ?4)",
            rusqlite::params![
                corpus_id,
                format!("ent-{corpus_id}-{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert entity_contracts_history");

        // entity_blocks_history
        conn.execute(
            "INSERT INTO entity_blocks_history \
             (corpus_id, id, entity_id, label, description, position, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, ?3, 'signature', 'fn foo()', 0, 'concrete', ?4, ?4, ?5)",
            rusqlite::params![
                corpus_id,
                format!("blk-{corpus_id}-{sha}"),
                format!("ent-{corpus_id}-{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert entity_blocks_history");

        // summaries_history
        conn.execute(
            "INSERT INTO summaries_history \
             (corpus_id, id, target_kind, target_id, depth, text, model, model_tier, \
              generated_at, derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, 'entity', ?3, 'brief', 'summary text', 'gpt-4', 'standard', \
                     '2024-01-01T00:00:00Z', 'concrete', ?4, ?4, ?5)",
            rusqlite::params![
                corpus_id,
                format!("sum-{corpus_id}-{sha}"),
                format!("ent-{corpus_id}-{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert summaries_history");

        // chunks_history (uses introduced_at_version / last_modified_at_version,
        // not introduced_at_version/last_modified_at_version — those are NOT dropped by migration 015)
        conn.execute(
            "INSERT INTO chunks_history \
             (corpus_id, id, kind, location_uri, content, byte_length, created_at, \
              semantic_processed, introduced_at_version, last_modified_at_version, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, 'file', 'file://chunk', 'content', 7, '2024-01-01T00:00:00Z', \
                     0, ?3, ?3, 'concrete', ?3, ?3, ?4)",
            rusqlite::params![
                corpus_id,
                format!("chunk-{corpus_id}-{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert chunks_history");

        // themes_history
        conn.execute(
            "INSERT INTO themes_history \
             (corpus_id, id, title, statement, confidence, model_tier, generated_at, \
              derived_at_kind, derived_at_sha, superseded_at_sha, superseded_at) \
             VALUES (?1, ?2, ?3, 'a theme statement', 0.9, 'standard', '2024-01-01T00:00:00Z', \
                     'concrete', ?4, ?4, ?5)",
            rusqlite::params![
                corpus_id,
                format!("theme-{corpus_id}-{sha}"),
                format!("Theme_{sha}"),
                sha,
                superseded_at,
            ],
        )
        .expect("insert themes_history");
    }

    fn count_history_rows(backend: &SqliteBackend, corpus_id: &str, table: &str) -> i64 {
        let db = backend.db_for_test();
        let conn = db.conn();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE corpus_id = ?1"),
            rusqlite::params![corpus_id],
            |r| r.get(0),
        )
        .expect("count rows")
    }

    fn total_history_rows(backend: &SqliteBackend, corpus_id: &str) -> i64 {
        let tables = [
            "entities_history",
            "edges_history",
            "entity_purposes_history",
            "entity_contracts_history",
            "entity_blocks_history",
            "summaries_history",
            "chunks_history",
            "themes_history",
        ];
        tables
            .iter()
            .map(|t| count_history_rows(backend, corpus_id, t))
            .sum()
    }

    #[test]
    fn prune_keeps_last_n_supersession_shas() {
        let backend = make_backend_with_corpus("corpus-a");

        // Seed 5 supersession SHAs with ascending timestamps.
        let shas = [
            ("sha1", "2024-01-01T00:00:00Z"),
            ("sha2", "2024-02-01T00:00:00Z"),
            ("sha3", "2024-03-01T00:00:00Z"),
            ("sha4", "2024-04-01T00:00:00Z"),
            ("sha5", "2024-05-01T00:00:00Z"),
        ];
        for (sha, ts) in &shas {
            insert_history_rows(&backend, "corpus-a", sha, ts);
        }

        // Prune, keeping the 3 newest SHAs (sha3, sha4, sha5).
        let stats = backend
            .prune_history("corpus-a", 3, false)
            .expect("prune_history");

        assert_eq!(stats.supersession_shas_kept, 3);
        assert_eq!(stats.supersession_shas_pruned, 2);

        // The 2 oldest SHAs (sha1, sha2) should be gone from every table.
        let db = backend.db_for_test();
        let conn = db.conn();
        for table in &[
            "entities_history",
            "edges_history",
            "entity_purposes_history",
            "entity_contracts_history",
            "entity_blocks_history",
            "summaries_history",
            "chunks_history",
            "themes_history",
        ] {
            let remaining: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table} \
                         WHERE corpus_id = 'corpus-a' \
                           AND superseded_at_sha IN ('sha1','sha2')"
                    ),
                    [],
                    |r| r.get(0),
                )
                .expect("count old rows");
            assert_eq!(
                remaining, 0,
                "{table}: expected 0 rows for pruned SHAs, got {remaining}"
            );

            let kept: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table} \
                         WHERE corpus_id = 'corpus-a' \
                           AND superseded_at_sha IN ('sha3','sha4','sha5')"
                    ),
                    [],
                    |r| r.get(0),
                )
                .expect("count kept rows");
            assert_eq!(
                kept, 3,
                "{table}: expected 3 rows for kept SHAs (one per SHA), got {kept}"
            );
        }
    }

    #[test]
    fn prune_dry_run_reports_counts_without_deleting() {
        let backend = make_backend_with_corpus("corpus-dry");

        let shas = [
            ("sha1", "2024-01-01T00:00:00Z"),
            ("sha2", "2024-02-01T00:00:00Z"),
            ("sha3", "2024-03-01T00:00:00Z"),
            ("sha4", "2024-04-01T00:00:00Z"),
            ("sha5", "2024-05-01T00:00:00Z"),
        ];
        for (sha, ts) in &shas {
            insert_history_rows(&backend, "corpus-dry", sha, ts);
        }

        let total_before = total_history_rows(&backend, "corpus-dry");

        let stats = backend
            .prune_history("corpus-dry", 3, true)
            .expect("prune_history dry run");

        // Stats should indicate work would be done.
        assert_eq!(stats.supersession_shas_pruned, 2);
        assert!(
            stats.rows_pruned_entities_history > 0,
            "expected non-zero entities_history count"
        );
        assert!(
            stats.rows_pruned_chunks_history > 0,
            "expected non-zero chunks_history count"
        );

        // But no rows should actually be deleted.
        let total_after = total_history_rows(&backend, "corpus-dry");
        assert_eq!(
            total_before, total_after,
            "dry run must not delete any rows"
        );
    }

    #[test]
    fn prune_keep_larger_than_history_is_no_op() {
        let backend = make_backend_with_corpus("corpus-noop");

        let shas = [
            ("sha1", "2024-01-01T00:00:00Z"),
            ("sha2", "2024-02-01T00:00:00Z"),
            ("sha3", "2024-03-01T00:00:00Z"),
            ("sha4", "2024-04-01T00:00:00Z"),
            ("sha5", "2024-05-01T00:00:00Z"),
        ];
        for (sha, ts) in &shas {
            insert_history_rows(&backend, "corpus-noop", sha, ts);
        }

        let total_before = total_history_rows(&backend, "corpus-noop");

        let stats = backend
            .prune_history("corpus-noop", 100, false)
            .expect("prune_history no-op");

        assert_eq!(stats.supersession_shas_kept, 5);
        assert_eq!(stats.supersession_shas_pruned, 0);
        assert_eq!(stats.rows_pruned_entities_history, 0);
        assert_eq!(stats.rows_pruned_edges_history, 0);
        assert_eq!(stats.rows_pruned_entity_purposes_history, 0);
        assert_eq!(stats.rows_pruned_entity_contracts_history, 0);
        assert_eq!(stats.rows_pruned_entity_blocks_history, 0);
        assert_eq!(stats.rows_pruned_summaries_history, 0);
        assert_eq!(stats.rows_pruned_chunks_history, 0);
        assert_eq!(stats.rows_pruned_themes_history, 0);

        let total_after = total_history_rows(&backend, "corpus-noop");
        assert_eq!(total_before, total_after, "no rows should be deleted");
    }

    #[test]
    fn prune_atomicity() {
        let backend = make_backend_with_corpus("corpus-atomic");

        let shas = [
            ("sha1", "2024-01-01T00:00:00Z"),
            ("sha2", "2024-02-01T00:00:00Z"),
            ("sha3", "2024-03-01T00:00:00Z"),
        ];
        for (sha, ts) in &shas {
            insert_history_rows(&backend, "corpus-atomic", sha, ts);
        }

        // Drop one of the history tables to force a mid-transaction failure.
        {
            let db = backend.db_for_test();
            let conn = db.conn();
            conn.execute_batch("DROP TABLE themes_history")
                .expect("drop themes_history");
        }

        // prune_history should return an Err.
        let result = backend.prune_history("corpus-atomic", 1, false);
        assert!(
            result.is_err(),
            "expected error when themes_history is missing"
        );

        // Verify rollback: the other 7 tables still have their full row counts.
        // (themes_history is dropped so we skip it here.)
        let remaining_tables = [
            "entities_history",
            "edges_history",
            "entity_purposes_history",
            "entity_contracts_history",
            "entity_blocks_history",
            "summaries_history",
            "chunks_history",
        ];
        let total_remaining: i64 = remaining_tables
            .iter()
            .map(|t| count_history_rows(&backend, "corpus-atomic", t))
            .sum();
        // Each of the 7 tables had 3 rows (one per SHA) = 21 total.
        assert_eq!(
            total_remaining, 21,
            "rollback should leave all 7 remaining tables intact"
        );
    }

    #[test]
    fn prune_other_corpus_unaffected() {
        let backend = make_backend_with_corpus("corpus-a");
        // Manually insert a second corpus.
        {
            let db = backend.db_for_test();
            let conn = db.conn();
            conn.execute(
                "INSERT INTO corpora (id, name, source, kind, status, created_at) \
                 VALUES ('corpus-b', 'corpus-b', '/tmp/b', 'code', 'ready', '2024-01-01T00:00:00Z')",
                [],
            )
            .expect("insert corpus-b");
        }

        let shas_a = [
            ("sha-a1", "2024-01-01T00:00:00Z"),
            ("sha-a2", "2024-02-01T00:00:00Z"),
            ("sha-a3", "2024-03-01T00:00:00Z"),
        ];
        let shas_b = [
            ("sha-b1", "2024-01-15T00:00:00Z"),
            ("sha-b2", "2024-02-15T00:00:00Z"),
            ("sha-b3", "2024-03-15T00:00:00Z"),
        ];
        for (sha, ts) in &shas_a {
            insert_history_rows(&backend, "corpus-a", sha, ts);
        }
        for (sha, ts) in &shas_b {
            insert_history_rows(&backend, "corpus-b", sha, ts);
        }

        let b_total_before = total_history_rows(&backend, "corpus-b");

        // Prune corpus-a, keeping only 1 SHA.
        let stats = backend
            .prune_history("corpus-a", 1, false)
            .expect("prune corpus-a");

        assert_eq!(stats.supersession_shas_pruned, 2, "corpus-a: 2 SHAs pruned");

        // corpus-b must be untouched.
        let b_total_after = total_history_rows(&backend, "corpus-b");
        assert_eq!(
            b_total_before, b_total_after,
            "corpus-b rows must not change"
        );

        // corpus-a should only have 1 SHA's rows remaining: 8 tables × 1 row = 8.
        let a_total_after = total_history_rows(&backend, "corpus-a");
        assert_eq!(a_total_after, 8, "corpus-a: only newest SHA's rows remain");
    }
}
