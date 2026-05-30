//! Cascade invalidation: archive then delete artifacts for dirty source files.
//!
//! When the history pass detects that one or more source files have changed,
//! their derived artifacts (entities, edges, purposes, contracts, blocks,
//! summaries, and the chunks themselves) are stale and must be swept before
//! the indexing passes re-derive them from scratch.
//!
//! [`run`] is the public entry point. It identifies the chunks whose source
//! file is marked dirty in the [`ChangeManifest`] and hands them to
//! [`history_layer::commit`] — the single writer of provenance archival +
//! tombstones. The history layer archives every affected artifact into the
//! corresponding `*_history` table, deletes the head rows (one SQLite write
//! transaction), and additionally archives the corpus's themes so they pick up
//! an honest per-commit `Concrete(sha)` lineage on any source change (closing
//! the `head-mode-theme-archival-missing` bug).
//!
//! The cascade no longer touches `*_history` or themes directly — that is the
//! history layer's job. When all sources are clean, this pass is a no-op.

use std::sync::Arc;

use crate::{
    indexing::{
        change_manifest::ChangeManifest,
        history_layer::{self, CommitPlan, WalkDirection},
    },
    storage::{StorageBackend, run_log::PassStats},
    types::Corpus,
};

/// Run the cascade-invalidation sweep for all dirty source files.
///
/// Returns a `PassStats` that counts archived (→ `processed`) chunks.
///
/// # Behaviour
///
/// - When `manifest.all_dirty` is true, **all** chunks in the corpus are
///   archived and deleted so the downstream passes start with a blank slate.
/// - When `manifest.dirty_count() == 0` (clean run), this function returns
///   immediately with zero stats.
/// - Otherwise, only chunks at dirty source paths are swept.
pub async fn run(
    db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    manifest: &ChangeManifest,
) -> anyhow::Result<PassStats> {
    let mut stats = PassStats::default();

    // Nothing to do when no sources changed.
    if !manifest.all_dirty && manifest.dirty_count() == 0 {
        tracing::info!("[cascade] no dirty sources — skipping invalidation sweep");
        return Ok(stats);
    }

    let this_version = manifest.current_version.clone();

    // Collect the IDs of all chunks that need to be swept.
    let all_chunks = db.chunk_list(&corpus.id)?;
    let dirty_chunk_ids: Vec<String> = if manifest.all_dirty {
        all_chunks.iter().map(|c| c.id.clone()).collect()
    } else {
        all_chunks
            .iter()
            .filter(|c| manifest.is_dirty_for_chunk(c))
            .map(|c| c.id.clone())
            .collect()
    };

    // Themes are corpus-level and supersede on *any* source change, even when
    // no chunk matches a dirty path (e.g. a file deletion). We therefore route
    // through the history layer whenever there are dirty sources, asking it to
    // archive themes regardless of whether dirty chunks were found.
    let archive_themes = manifest.all_dirty || manifest.dirty_count() > 0;

    if dirty_chunk_ids.is_empty() && !archive_themes {
        tracing::info!(
            "[cascade] manifest has dirty sources but no matching chunks found — nothing to sweep"
        );
        return Ok(stats);
    }

    tracing::info!(
        "[cascade] sweeping {} dirty chunk(s) for corpus '{}' at version {} (archive_themes={})",
        dirty_chunk_ids.len(),
        corpus.id,
        this_version,
        archive_themes,
    );

    let plan = CommitPlan {
        dirty_chunk_ids,
        archive_themes,
        tombstones: Vec::new(),
    };

    let commit_stats = history_layer::commit(
        db.as_ref(),
        &corpus.id,
        &this_version,
        WalkDirection::Head,
        &plan,
    )?;

    tracing::info!(
        "[cascade] archived {} chunk(s), {} entity(ies), {} theme(s)",
        commit_stats.chunks_archived,
        commit_stats.entities_archived,
        commit_stats.themes_archived,
    );

    stats.processed = commit_stats.chunks_archived;
    Ok(stats)
}
