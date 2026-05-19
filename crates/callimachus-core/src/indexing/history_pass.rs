//! Stage 0 — history-aware change detection pass.
//!
//! `run` computes the current version reference for the corpus source and
//! compares it against `corpus.last_indexed_version`.  It returns a
//! `ChangeManifest` that downstream passes use to skip unchanged sources and
//! chunks.
//!
//! Version writing (back to `corpora.last_indexed_version`) is done by the
//! pipeline orchestrator *after* all passes succeed, not here.  This ensures
//! partial failures don't advance the version anchor.

use std::sync::Arc;

use crate::{
    adapter::SourceAdapter,
    indexing::change_manifest::ChangeManifest,
    storage::{StorageBackend, run_log::PassStats},
    types::Corpus,
};

use super::pipeline::IndexOptions;

/// Run Stage 0 — compute the change manifest for `corpus`.
///
/// Returns `(manifest, stats)` where `stats.processed` is the number of
/// dirty source paths (0 means nothing changed).
pub async fn run(
    _db: Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: Arc<dyn SourceAdapter>,
    opts: &IndexOptions,
) -> anyhow::Result<(ChangeManifest, PassStats)> {
    let mut stats = PassStats::default();

    // Compute current version via the adapter.
    let current = adapter.current_version(&corpus.source)?;

    let last = corpus.last_indexed_version.as_deref();

    let manifest = if opts.full {
        // --full: ignore stored version, treat everything as dirty.
        tracing::info!("[history] --full flag set — all sources dirty");
        stats.processed = 0; // individual counts tallied by chunk_pass
        ChangeManifest::all_dirty(current)
    } else if last.is_none() {
        // First run: no stored version.
        tracing::info!("[history] no prior version — first run, all sources dirty");
        ChangeManifest::all_dirty(current)
    } else if last == Some(current.as_str()) {
        // No changes since last run.
        tracing::info!("[history] version unchanged ({current}) — nothing to process");
        stats.skipped = 1; // signal to caller that we short-circuited
        ChangeManifest::empty(current)
    } else {
        // Compute the diff.
        let from = last.unwrap(); // safe: checked above
        tracing::info!("[history] computing diff {from} → {current}");
        let changed = adapter.changed_sources(&corpus.source, Some(from), &current)?;
        let dirty_count = changed.len() as u64;
        tracing::info!("[history] {dirty_count} dirty source(s)");
        stats.processed = dirty_count;
        ChangeManifest::from_changed(current, changed)
    };

    Ok((manifest, stats))
}
