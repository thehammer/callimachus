//! Change-detection data types shared between adapters and the indexing engine.
//!
//! These are the plain-data inputs/outputs of an adapter's Stage-0 change
//! detection (`SourceAdapter::changed_sources`). The richer `ChangeManifest`
//! that consumes them — with its dirty-path bookkeeping — lives in
//! `callimachus-core`, since only the indexing engine needs it.

// ── Change kind ───────────────────────────────────────────────────────────────

/// Whether a source file was added, modified, or deleted since the last run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

// ── Commit metadata ───────────────────────────────────────────────────────────

/// Metadata from the git commit that last touched a source file.
/// Only populated for git-backed code corpora.
#[derive(Debug, Clone)]
pub struct CommitMeta {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub date: String,
}

// ── ChangedSource ─────────────────────────────────────────────────────────────

/// One entry in the change manifest: a source-file path with its change kind
/// and optional git metadata.
#[derive(Debug, Clone)]
pub struct ChangedSource {
    /// Path relative to the corpus source root (matches the path that
    /// `adapter.discover` / `adapter.chunk` operates on).
    pub path: String,
    pub kind: ChangeKind,
    /// Populated only when the adapter has git history available.
    pub commit_meta: Option<CommitMeta>,
}
