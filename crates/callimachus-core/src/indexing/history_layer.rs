//! The honest-provenance history layer — the single writer of provenance
//! archival and tombstones.
//!
//! Before this module existed, three code paths wrote to the `*_history`
//! tables: the cascade (HEAD-mode supersession), the diff-based walker's
//! `copy_unchanged_artifacts` (PR #34's copy-forward mitigation), and the
//! backfill wrapper. They could disagree about what rows existed at a given
//! SHA — the root cause of the *middle-out duplicate-row* bug.
//!
//! Under honest provenance the rule is simple: **one writer per
//! `(artifact, SHA)`**. The cascade and the walker now route every archival
//! and tombstone write through [`commit`]. Combined with the
//! `UNIQUE(corpus_id, id, derived_at_kind, derived_at_sha)` indexes on every
//! `*_history` table (migration 013) and the idempotent `INSERT OR IGNORE`
//! archive helpers, a divergent duplicate row is now impossible by
//! construction.
//!
//! ## What [`commit`] does
//!
//! Given the diff between the neighbour commit and `this_version`, plus the
//! head artifacts the pipeline produced for the dirty subtree, it:
//!
//! 1. **Archives the about-to-be-superseded head rows** for the dirty subtree
//!    into `*_history`, stamped with their existing provenance and
//!    `superseded_at_sha = this_version`, then deletes the head rows so the
//!    passes re-derive them fresh (the cascade's archive-then-delete step).
//! 2. **Archives corpus themes** when any source changed, giving themes a
//!    per-commit `Concrete(sha)` lineage (closes
//!    `head-mode-theme-archival-missing`).
//! 3. **Writes tombstones** for artifacts present in the previous state but
//!    absent from this one, tagged `Concrete(this_version)`.
//!
//! Provenance *refinement* (narrowing a `RangePredating` tag during a backward
//! walk) is performed directly against [`StorageBackend::refine_provenance`];
//! it does not pass through [`commit`] because it mutates head tags in place
//! rather than writing history.

use crate::storage::StorageBackend;
use crate::types::provenance::Provenance;

/// Which walk produced this commit. Recorded for clarity at call sites and to
/// let the history layer reason about direction where it matters; the archival
/// mechanics are identical across directions (idempotent, SHA-keyed writes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkDirection {
    /// Ordinary incremental indexing at HEAD (no history walk).
    Head,
    /// Forward first-parent walk, oldest → newest.
    Forward,
    /// Backward first-parent backfill, newest → oldest.
    Backward,
}

/// A removed artifact to tombstone, tagged `Concrete(this_version)`.
#[derive(Clone, Debug)]
pub struct RemovedArtifact {
    /// `'chunk' | 'entity' | 'edge' | 'embedding'` — the tombstone kind.
    pub kind: &'static str,
    pub id: String,
    /// Optional reason, e.g. `"absent_in_substrate"` or `"renamed_to=<id>"`.
    pub reason: Option<String>,
}

/// The work a single commit's history write must perform. Callers populate the
/// fields relevant to their path; an empty plan is a no-op.
#[derive(Clone, Debug, Default)]
pub struct CommitPlan {
    /// Head chunk ids whose source files changed — archived into
    /// `chunks_history` (with their derived entities/edges/…) and deleted from
    /// head so the passes re-derive them. This is the cascade's supersession set.
    pub dirty_chunk_ids: Vec<String>,
    /// Archive the corpus's themes (they are corpus-level and supersede on any
    /// source change — PRD "re-derive per commit").
    pub archive_themes: bool,
    /// Artifacts that existed in the neighbour state but are gone at this
    /// commit — each gets a `Concrete(this_version)` tombstone.
    pub tombstones: Vec<RemovedArtifact>,
}

impl CommitPlan {
    /// `true` when this plan would write nothing.
    pub fn is_empty(&self) -> bool {
        self.dirty_chunk_ids.is_empty() && !self.archive_themes && self.tombstones.is_empty()
    }
}

/// Counts of what a [`commit`] call wrote, for logging and pass metrics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitStats {
    pub chunks_archived: u64,
    pub entities_archived: u64,
    pub themes_archived: u64,
    pub tombstones_written: u64,
}

/// Apply `plan` at `this_version`. The sole entry point for provenance
/// archival + tombstone writes from the cascade and the walker.
///
/// `this_version` is the version reference (`git:<oid>`) of the commit being
/// processed; archived rows are superseded at it and tombstones are tagged
/// `Concrete(this_version)`.
pub fn commit(
    db: &dyn StorageBackend,
    corpus_id: &str,
    this_version: &str,
    _direction: WalkDirection,
    plan: &CommitPlan,
) -> anyhow::Result<CommitStats> {
    let mut stats = CommitStats::default();

    if plan.is_empty() {
        return Ok(stats);
    }

    // 1. Archive + delete the dirty subtree (chunks and their derived
    //    entity sub-trees). Transactional inside the backend.
    if !plan.dirty_chunk_ids.is_empty() {
        let cascade =
            db.cascade_delete_dirty_subtree(corpus_id, &plan.dirty_chunk_ids, this_version)?;
        stats.chunks_archived += cascade.chunks_archived;
        stats.entities_archived += cascade.entities_archived;
    }

    // 2. Archive corpus themes on any source change. The head theme rows are
    //    left in place for the theme pass to re-derive; the archived rows carry
    //    superseded_at_sha = this_version.
    if plan.archive_themes {
        stats.themes_archived += db.archive_themes_for_corpus(corpus_id, this_version)?;
    }

    // 3. Tombstone removed artifacts at Concrete(this_version).
    let prov = Provenance::concrete(this_version);
    for t in &plan.tombstones {
        db.tombstone_insert(corpus_id, t.kind, &t.id, &prov, t.reason.as_deref())?;
        stats.tombstones_written += 1;
    }

    Ok(stats)
}
