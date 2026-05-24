//! Cascade invalidation: archive then delete artifacts for dirty source files.
//!
//! When the history pass detects that one or more source files have changed,
//! their derived artifacts (entities, edges, purposes, contracts, blocks,
//! summaries, and the chunks themselves) are stale and must be swept before
//! the indexing passes re-derive them from scratch.
//!
//! [`run`] is the public entry point. It:
//!
//! 1. Identifies chunks whose source file is marked dirty in the
//!    [`ChangeManifest`].
//! 2. Delegates to `StorageBackend::cascade_delete_dirty_subtree`, which
//!    archives every affected artifact into the corresponding `*_history`
//!    table and then deletes the head rows — all inside a single SQLite
//!    write transaction for atomicity.
//!
//! When all sources are clean (no dirty files), this pass is a no-op.

use std::sync::Arc;

use crate::{
    indexing::change_manifest::ChangeManifest,
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

    let superseded_at_version = &manifest.current_version;

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

    if dirty_chunk_ids.is_empty() {
        tracing::info!(
            "[cascade] manifest has dirty sources but no matching chunks found — nothing to sweep"
        );
        return Ok(stats);
    }

    tracing::info!(
        "[cascade] sweeping {} dirty chunk(s) for corpus '{}' at version {}",
        dirty_chunk_ids.len(),
        corpus.id,
        superseded_at_version,
    );

    let cascade_stats =
        db.cascade_delete_dirty_subtree(&corpus.id, &dirty_chunk_ids, superseded_at_version)?;

    tracing::info!(
        "[cascade] archived {} chunk(s), {} entity(ies)",
        cascade_stats.chunks_archived,
        cascade_stats.entities_archived,
    );

    stats.processed = cascade_stats.chunks_archived;
    Ok(stats)
}
