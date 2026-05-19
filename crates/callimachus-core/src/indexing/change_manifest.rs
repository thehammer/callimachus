//! Stage 0 change manifest — the result of a history-aware change-detection pass.
//!
//! A `ChangeManifest` describes which source files have changed since the last
//! successful pipeline run and carries the version reference (git commit SHA or
//! hash-of-hashes) that the pipeline writes back to `corpora.last_indexed_version`
//! after a successful run.
//!
//! Downstream passes use `ChangeManifest::is_dirty` / `is_dirty_for_chunk` to
//! decide whether to process a source file or a chunk.  When `all_dirty` is true
//! (first run, `--full`, or a pass invoked without a preceding History pass) every
//! source / chunk is processed.

use std::collections::{HashMap, HashSet};

use crate::types::Chunk;

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

// ── ChangeManifest ────────────────────────────────────────────────────────────

/// Result of the Stage 0 history pass.
///
/// Downstream passes should call `is_dirty` or `is_dirty_for_chunk` to decide
/// whether to process a given source or chunk.  When `all_dirty` is true the
/// answer is always "yes" (first run, `--full`, or no History pass).
///
/// The `current_version` string is written back to `corpora.last_indexed_version`
/// after the full pipeline succeeds; it is the anchor for the *next* run.
#[derive(Debug, Clone)]
pub struct ChangeManifest {
    /// Version reference for the corpus at the *end* of this run.
    /// Prefix: `"git:<full-oid>"` for git repos, `"v1-tree:<hex-digest>"` otherwise.
    pub current_version: String,
    /// When true, every source is considered dirty (overrides `dirty_paths`).
    pub all_dirty: bool,
    /// Set of source-file paths that changed since `last_indexed_version`.
    /// Meaningful only when `all_dirty` is false.
    dirty_paths: HashSet<String>,
    /// Per-path commit metadata, keyed by source path.
    commit_metadata: HashMap<String, CommitMeta>,
}

impl ChangeManifest {
    /// All-dirty manifest: every source / chunk will be processed.
    /// Use on first run, after `--full`, or when the History pass is absent.
    pub fn all_dirty(current_version: impl Into<String>) -> Self {
        Self {
            current_version: current_version.into(),
            all_dirty: true,
            dirty_paths: HashSet::new(),
            commit_metadata: HashMap::new(),
        }
    }

    /// Empty (no changes) manifest: nothing needs processing.
    /// Use when the stored version matches the current version.
    pub fn empty(current_version: impl Into<String>) -> Self {
        Self {
            current_version: current_version.into(),
            all_dirty: false,
            dirty_paths: HashSet::new(),
            commit_metadata: HashMap::new(),
        }
    }

    /// Construct from a list of changed sources.
    pub fn from_changed(current_version: impl Into<String>, changed: Vec<ChangedSource>) -> Self {
        let mut dirty_paths = HashSet::new();
        let mut commit_metadata = HashMap::new();
        for cs in changed {
            if let Some(meta) = cs.commit_meta {
                commit_metadata.insert(cs.path.clone(), meta);
            }
            dirty_paths.insert(cs.path);
        }
        Self {
            current_version: current_version.into(),
            all_dirty: false,
            dirty_paths,
            commit_metadata,
        }
    }

    /// Returns true if the given source path should be re-indexed.
    pub fn is_dirty(&self, path: &str) -> bool {
        self.all_dirty || self.dirty_paths.contains(path)
    }

    /// Returns true if the chunk's underlying source file should be re-indexed.
    ///
    /// The source file path is extracted from the chunk's location URI by:
    /// 1. Stripping the `calli://` scheme prefix.
    /// 2. Dropping everything up to and including the first `/` (corpus-id segment).
    /// 3. Taking everything before the first `#` (symbol-anchor fragment).
    ///
    /// Example: `calli://admin-portal/src/app/Services/VisitService.php#VisitService/part5`
    /// → `src/app/Services/VisitService.php`.
    ///
    /// For book/wiki chunks (no `#` fragment) step 3 is a no-op.
    pub fn is_dirty_for_chunk(&self, chunk: &Chunk) -> bool {
        if self.all_dirty {
            return true;
        }
        let path = file_path_from_uri(&chunk.location.uri);
        self.dirty_paths.contains(path)
    }

    /// Return the commit metadata for a source path, if any.
    pub fn commit_meta_for(&self, path: &str) -> Option<&CommitMeta> {
        self.commit_metadata.get(path)
    }

    /// Number of dirty source paths (0 when `all_dirty` is true — use `all_dirty`
    /// to check for the "process everything" case).
    pub fn dirty_count(&self) -> usize {
        self.dirty_paths.len()
    }
}

// ── URI helper ────────────────────────────────────────────────────────────────

/// Extract the relative source file path from a `calli://` location URI.
///
/// Steps:
/// 1. Strip `calli://` prefix.
/// 2. Drop everything up to and including the first `/` (corpus-id segment).
/// 3. Take everything before the first `#` (symbol-anchor fragment).
///
/// Example: `calli://admin-portal/src/app/Services/VisitService.php#VisitService/part5`
/// → `src/app/Services/VisitService.php`.
pub(crate) fn file_path_from_uri(uri: &str) -> &str {
    let without_scheme = uri.strip_prefix("calli://").unwrap_or(uri);
    let without_corpus = without_scheme
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or(without_scheme);
    without_corpus.split('#').next().unwrap_or(without_corpus)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Chunk, Location};

    fn make_chunk(uri: &str) -> Chunk {
        Chunk {
            id: "abc".into(),
            corpus_id: "c".into(),
            parent_path: None,
            kind: "function".into(),
            location: Location {
                corpus_id: "c".into(),
                path: uri.to_string(),
                uri: uri.to_string(),
            },
            content: "fn foo() {}".into(),
            byte_length: 11,
            created_at: "2024-01-01T00:00:00Z".into(),
            source_hash: None,
            introduced_at_version: None,
            last_modified_at_version: None,
        }
    }

    #[test]
    fn file_path_from_uri_with_fragment() {
        assert_eq!(
            file_path_from_uri(
                "calli://admin-portal/src/app/Services/VisitService.php#VisitService/part5"
            ),
            "src/app/Services/VisitService.php"
        );
    }

    #[test]
    fn file_path_from_uri_without_fragment() {
        assert_eq!(
            file_path_from_uri("calli://my-corpus/docs/guide.md"),
            "docs/guide.md"
        );
    }

    #[test]
    fn file_path_from_uri_no_scheme() {
        // Graceful degradation: if the URI doesn't have the calli:// prefix,
        // strip the corpus segment and return the rest.
        assert_eq!(
            file_path_from_uri("some-corpus/docs/guide.md"),
            "docs/guide.md"
        );
    }

    #[test]
    fn all_dirty_marks_every_path_dirty() {
        let m = ChangeManifest::all_dirty("git:abc123");
        assert!(m.is_dirty("any/path.rs"));
        assert!(m.is_dirty("another/path.md"));
    }

    #[test]
    fn empty_marks_nothing_dirty() {
        let m = ChangeManifest::empty("git:abc123");
        assert!(!m.is_dirty("some/path.rs"));
        assert!(!m.is_dirty("other/path.rs"));
    }

    #[test]
    fn from_changed_marks_only_listed_paths_dirty() {
        let changed = vec![
            ChangedSource {
                path: "src/foo.rs".into(),
                kind: ChangeKind::Modified,
                commit_meta: None,
            },
            ChangedSource {
                path: "src/bar.rs".into(),
                kind: ChangeKind::Added,
                commit_meta: None,
            },
        ];
        let m = ChangeManifest::from_changed("git:abc", changed);
        assert!(m.is_dirty("src/foo.rs"));
        assert!(m.is_dirty("src/bar.rs"));
        assert!(!m.is_dirty("src/baz.rs"));
    }

    #[test]
    fn is_dirty_for_chunk_extracts_path_from_uri() {
        let changed = vec![ChangedSource {
            path: "src/app/Services/VisitService.php".into(),
            kind: ChangeKind::Modified,
            commit_meta: None,
        }];
        let m = ChangeManifest::from_changed("git:abc", changed);

        let dirty_chunk =
            make_chunk("calli://admin-portal/src/app/Services/VisitService.php#VisitService/part5");
        let clean_chunk = make_chunk("calli://admin-portal/src/app/Services/Other.php");

        assert!(m.is_dirty_for_chunk(&dirty_chunk));
        assert!(!m.is_dirty_for_chunk(&clean_chunk));
    }

    #[test]
    fn commit_meta_roundtrip() {
        let changed = vec![ChangedSource {
            path: "src/lib.rs".into(),
            kind: ChangeKind::Modified,
            commit_meta: Some(CommitMeta {
                sha: "deadbeef".into(),
                message: "fix: something".into(),
                author: "Alice <alice@example.com>".into(),
                date: "2024-01-01".into(),
            }),
        }];
        let m = ChangeManifest::from_changed("git:deadbeef", changed);
        let meta = m.commit_meta_for("src/lib.rs").unwrap();
        assert_eq!(meta.sha, "deadbeef");
        assert_eq!(meta.author, "Alice <alice@example.com>");
    }
}
