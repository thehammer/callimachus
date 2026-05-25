//! Virtual-head query layer for backward backfill.
//!
//! [`VirtualHead`] presents the derived state of a corpus AS-IT-WAS at a
//! specific commit SHA, by querying `*_history` tables (and the current head
//! tables) filtered to `derived_at_version = target_sha`.
//!
//! ## Why SHA-based rather than timestamp-based
//!
//! History rows produced by the backward backfill walker have their
//! `superseded_at` set to the wall-clock time of the backfill run, NOT to the
//! logical time of the commit. Because the walk processes commits
//! newest-older → oldest-older, older commits end up with *larger*
//! `superseded_at` timestamps — the opposite of commit chronology. A
//! timestamp-based UNION ALL predicate would therefore return wrong rows.
//! Filtering by `derived_at_version = target_sha` is monotonically correct for
//! both forward and backward walks.
//!
//! ## When rows are available
//!
//! VirtualHead is useful in two scenarios:
//!
//! 1. **Same backfill walk** — chunk/structure passes run first and write entity
//!    rows to `entities_history` with `derived_at_version = git:<sha>`. When
//!    theme pass runs later in the same iteration, `VirtualHead` finds those
//!    rows.
//!
//! 2. **Separate `--passes theme` re-run** — a prior full backfill has already
//!    populated `entities_history`. `VirtualHead` re-reads them.
//!
//! If no rows exist for `target_sha` (e.g. structure was never backfilled for
//! that commit), `entity_list` returns empty and the theme pass skips — the
//! expected behaviour.

use std::sync::Arc;

use crate::error::Result;
use crate::storage::StorageBackend;
use crate::types::Entity;

/// Presents the corpus's derived state as it was at `target_sha`.
///
/// Reads are served from `*_history` tables and the current head tables,
/// filtered to `derived_at_version = target_sha`.
///
/// Constructed once per backfill iteration in
/// [`walk_history_backward`](crate::indexing::history_walk::walk_history_backward)
/// and surfaced to derived-state-reading passes through
/// [`ReadView::Virtual`](crate::indexing::pipeline::ReadView::Virtual).
pub struct VirtualHead {
    db: Arc<dyn StorageBackend>,
    corpus_id: String,
    target_sha: String,
}

impl std::fmt::Debug for VirtualHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualHead")
            .field("corpus_id", &self.corpus_id)
            .field("target_sha", &self.target_sha)
            .finish_non_exhaustive()
    }
}

impl VirtualHead {
    /// Construct a `VirtualHead` for `target_sha` in `corpus_id`.
    ///
    /// `db` **must** be the real (non-wrapper) storage backend so that reads
    /// go to the actual SQLite tables. The `BackfillStorageWrapper` intercepts
    /// writes but transparently delegates `entity_list_at_version` to the real
    /// backend, so passing the wrapper also works in practice.
    pub fn new(db: Arc<dyn StorageBackend>, corpus_id: String, target_sha: String) -> Self {
        Self {
            db,
            corpus_id,
            target_sha,
        }
    }

    /// List all entities with `derived_at_version = target_sha`.
    ///
    /// Combines rows from both the head `entities` table (entities still at
    /// HEAD whose version matches) and `entities_history` (entities that were
    /// superseded after the target commit).
    pub fn entity_list(&self) -> Result<Vec<Entity>> {
        self.db
            .entity_list_at_version(&self.corpus_id, &self.target_sha)
    }

    /// Count entities with `derived_at_version = target_sha`.
    pub fn entity_count(&self) -> Result<u64> {
        self.db
            .entity_count_at_version(&self.corpus_id, &self.target_sha)
    }

    /// Find entities by canonical-name fragment at `target_sha`.
    ///
    /// Returns entities (case-insensitive substring match on `canonical_name`
    /// or any alias) that have `derived_at_version = target_sha`.
    pub fn entity_find_by_name(&self, name: &str) -> Result<Vec<Entity>> {
        let lower = name.to_lowercase();
        let all = self.entity_list()?;
        Ok(all
            .into_iter()
            .filter(|e| {
                e.canonical_name.to_lowercase().contains(&lower)
                    || e.aliases.iter().any(|a| a.to_lowercase().contains(&lower))
            })
            .collect())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::storage::{SqliteBackend, StorageBackend};
    use crate::types::{Corpus, Entity};

    use super::VirtualHead;

    // ── Fixtures ──────────────────────────────────────────────────────────────

    fn seed_corpus(db: &SqliteBackend, corpus_id: &str) {
        let corpus = Corpus::new(
            corpus_id.to_string(),
            "Test".to_string(),
            "code".to_string(),
            "/tmp".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
    }

    /// Insert an entity with a given `derived_at_version` directly into the
    /// head table via `entity_upsert`.
    fn seed_entity(db: &SqliteBackend, corpus_id: &str, entity_id: &str, version: &str) {
        let entity = Entity {
            id: entity_id.to_string(),
            corpus_id: corpus_id.to_string(),
            canonical_name: format!("Entity {entity_id}"),
            kind: "function".to_string(),
            derived_at_version: Some(version.to_string()),
            ..Default::default()
        };
        db.entity_upsert(&entity).unwrap();
    }

    fn make_vh(db: Arc<dyn StorageBackend>, corpus_id: &str, target_sha: &str) -> VirtualHead {
        VirtualHead::new(db, corpus_id.to_string(), target_sha.to_string())
    }

    // ── entity_list / entity_count ────────────────────────────────────────────

    /// A head entity whose `derived_at_version` matches `target_sha` is included.
    #[test]
    fn active_at_includes_unsuperseded_head_rows() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");
        seed_entity(&db, "corp", "ent-1", "git:abc");

        let vh = make_vh(
            Arc::clone(&db) as Arc<dyn StorageBackend>,
            "corp",
            "git:abc",
        );
        let entities = vh.entity_list().unwrap();
        assert_eq!(entities.len(), 1, "expected 1 entity from head");
        assert_eq!(entities[0].id, "ent-1");
        assert_eq!(vh.entity_count().unwrap(), 1);
    }

    /// A head entity whose `derived_at_version` does NOT match is excluded.
    #[test]
    fn active_at_excludes_head_rows_with_different_version() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");
        seed_entity(&db, "corp", "ent-1", "git:xyz"); // wrong version

        let vh = make_vh(
            Arc::clone(&db) as Arc<dyn StorageBackend>,
            "corp",
            "git:abc",
        );
        assert!(
            vh.entity_list().unwrap().is_empty(),
            "entity with different version should be excluded"
        );
        assert_eq!(vh.entity_count().unwrap(), 0);
    }

    /// After an entity is superseded (upsert with new version snapshots old row
    /// into history), VirtualHead at the old version returns the history row
    /// and VirtualHead at the new version returns the head row.
    #[test]
    fn active_at_excludes_superseded_rows_returns_history_version() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");

        // Seed at v1 then upsert at v2 — this snapshots v1 into entities_history.
        seed_entity(&db, "corp", "ent-1", "git:v1");
        let entity_v2 = Entity {
            id: "ent-1".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "Entity ent-1 v2".to_string(),
            kind: "function".to_string(),
            derived_at_version: Some("git:v2".to_string()),
            ..Default::default()
        };
        db.entity_upsert(&entity_v2).unwrap();

        // VirtualHead at v1 → history row.
        let vh_v1 = make_vh(Arc::clone(&db) as Arc<dyn StorageBackend>, "corp", "git:v1");
        let ents_v1 = vh_v1.entity_list().unwrap();
        assert_eq!(ents_v1.len(), 1, "expected 1 entity at v1 (from history)");
        assert_eq!(
            ents_v1[0].derived_at_version.as_deref(),
            Some("git:v1"),
            "returned entity should have v1"
        );

        // VirtualHead at v2 → head row.
        let vh_v2 = make_vh(Arc::clone(&db) as Arc<dyn StorageBackend>, "corp", "git:v2");
        let ents_v2 = vh_v2.entity_list().unwrap();
        assert_eq!(ents_v2.len(), 1, "expected 1 entity at v2 (from head)");
        assert_eq!(
            ents_v2[0].derived_at_version.as_deref(),
            Some("git:v2"),
            "returned entity should have v2"
        );
    }

    /// Querying a target SHA that has rows only in entities_history (not head)
    /// returns those history rows.
    #[test]
    fn active_at_returns_history_version_when_superseded_after_target() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");

        // v1 → v2 → v3: each upsert snapshots the previous into history.
        seed_entity(&db, "corp", "ent-1", "git:v1");
        let make_ent = |ver: &str| Entity {
            id: "ent-1".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "E".to_string(),
            kind: "function".to_string(),
            derived_at_version: Some(ver.to_string()),
            ..Default::default()
        };
        db.entity_upsert(&make_ent("git:v2")).unwrap();
        db.entity_upsert(&make_ent("git:v3")).unwrap();

        // VirtualHead at v2 must return the v2 history row (v2 was in between).
        let vh = make_vh(Arc::clone(&db) as Arc<dyn StorageBackend>, "corp", "git:v2");
        let ents = vh.entity_list().unwrap();
        assert_eq!(ents.len(), 1, "expected 1 entity at v2");
        assert_eq!(ents[0].derived_at_version.as_deref(), Some("git:v2"));
    }

    /// VirtualHead::new builds correctly from a SHA that is current HEAD (no
    /// history rows yet). entity_list returns the head entity; entity_count = 1.
    #[test]
    fn cutoff_when_target_is_current_head() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");
        seed_entity(&db, "corp", "ent-1", "git:head");

        // No history rows exist for "git:head" yet (it is the current HEAD).
        let vh = make_vh(
            Arc::clone(&db) as Arc<dyn StorageBackend>,
            "corp",
            "git:head",
        );
        let ents = vh.entity_list().unwrap();
        assert_eq!(ents.len(), 1);
        assert_eq!(ents[0].id, "ent-1");
    }

    /// When the target SHA has no matching rows at all, entity_list is empty.
    #[test]
    fn empty_when_no_rows_at_version() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");
        seed_entity(&db, "corp", "ent-1", "git:other");

        let vh = make_vh(
            Arc::clone(&db) as Arc<dyn StorageBackend>,
            "corp",
            "git:notexist",
        );
        assert!(vh.entity_list().unwrap().is_empty());
        assert_eq!(vh.entity_count().unwrap(), 0);
    }

    // ── entity_find_by_name ───────────────────────────────────────────────────

    /// entity_find_by_name filters by canonical_name fragment at the target SHA.
    #[test]
    fn find_by_name_returns_matching_entities_at_version() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");

        // Two entities at v1; one matches "foo".
        for (id, name) in [("ent-foo", "FooHandler"), ("ent-bar", "BarHandler")] {
            let e = Entity {
                id: id.to_string(),
                corpus_id: "corp".to_string(),
                canonical_name: name.to_string(),
                kind: "struct".to_string(),
                derived_at_version: Some("git:v1".to_string()),
                ..Default::default()
            };
            db.entity_upsert(&e).unwrap();
        }

        let vh = make_vh(Arc::clone(&db) as Arc<dyn StorageBackend>, "corp", "git:v1");
        let found = vh.entity_find_by_name("foo").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ent-foo");
    }

    /// entity_find_by_name matches aliases too.
    #[test]
    fn find_by_name_matches_aliases() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");

        let e = Entity {
            id: "ent-1".to_string(),
            corpus_id: "corp".to_string(),
            canonical_name: "MainHandler".to_string(),
            kind: "struct".to_string(),
            aliases: vec!["FooProcessor".to_string()],
            derived_at_version: Some("git:v1".to_string()),
            ..Default::default()
        };
        db.entity_upsert(&e).unwrap();

        let vh = make_vh(Arc::clone(&db) as Arc<dyn StorageBackend>, "corp", "git:v1");
        // "foo" matches the alias "FooProcessor".
        let found = vh.entity_find_by_name("foo").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "ent-1");
    }

    /// find_by_name at a different version returns nothing.
    #[test]
    fn find_by_name_excludes_wrong_version() {
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        seed_corpus(&db, "corp");
        seed_entity(&db, "corp", "ent-foo", "git:v2");

        let vh = make_vh(Arc::clone(&db) as Arc<dyn StorageBackend>, "corp", "git:v1");
        assert!(
            vh.entity_find_by_name("foo").unwrap().is_empty(),
            "wrong version should return nothing"
        );
    }
}
