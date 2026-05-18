use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::{storage::StorageBackend, types::Corpus};

/// The set of changes detected since the last index run.
#[derive(Debug, Default)]
pub struct ChangeSet {
    /// Paths (relative to corpus source root or absolute) of files that changed.
    pub changed_paths: Vec<String>,
    /// Chunk IDs that should be removed (source file deleted or no longer produced).
    pub deleted_chunk_ids: Vec<String>,
    /// Detected change strategy.
    pub strategy: ChangeStrategy,
}

#[derive(Debug, Clone, Default)]
pub enum ChangeStrategy {
    /// Compare source mtime to corpus last_indexed_at.
    Mtime { since: DateTime<Utc> },
    /// Use git to list files changed since a commit/ref.
    Git { since_ref: String },
    /// Full reindex (no prior indexed_at or strategy unavailable).
    #[default]
    Full,
}

/// Force a full changeset for `corpus`: returns all source paths without
/// consulting `last_indexed_at` or git history.
///
/// Use this when `--full` is passed on the CLI to override auto-selection.
pub fn detect_full(corpus: &Corpus, db: &dyn StorageBackend) -> anyhow::Result<ChangeSet> {
    let source_path = std::path::Path::new(&corpus.source);
    let changed_paths = if source_path.exists() {
        collect_all_source_paths(source_path)
    } else {
        vec![]
    };
    let deleted_chunk_ids = if !source_path.exists() {
        db.chunk_list_ids(&corpus.id)?
    } else {
        vec![]
    };
    Ok(ChangeSet {
        changed_paths,
        deleted_chunk_ids,
        strategy: ChangeStrategy::Full,
    })
}

/// Detect which source files changed since the last index run (or since `since`).
///
/// Strategy selection order:
/// 1. `since` looks like a git ref AND corpus source path has a `.git` dir → `Git`.
/// 2. `since` is an ISO 8601 date string → `Mtime` with parsed timestamp.
/// 3. `since` is None AND corpus has `last_indexed_at` → `Mtime` with `last_indexed_at`.
/// 4. Otherwise → `Full` (caller should warn and run all chunks).
pub fn detect(
    corpus: &Corpus,
    db: &dyn StorageBackend,
    since: Option<&str>,
) -> anyhow::Result<ChangeSet> {
    let source_path = Path::new(&corpus.source);

    // Determine which git root to check (parent dir for single files, dir itself for dirs).
    let git_root_candidate = if source_path.is_file() {
        source_path.parent().unwrap_or(source_path)
    } else {
        source_path
    };
    let has_git = git_root_candidate.join(".git").exists();

    let strategy = determine_strategy(corpus, since, has_git)?;

    match &strategy {
        ChangeStrategy::Full => {
            // Return all existing chunks as "to be re-indexed" via changed_paths.
            let changed_paths = if source_path.exists() {
                collect_all_source_paths(source_path)
            } else {
                vec![]
            };
            let deleted_chunk_ids = if !source_path.exists() {
                // Source was deleted entirely — all chunks are orphaned.
                db.chunk_list_ids(&corpus.id)?
            } else {
                vec![]
            };
            Ok(ChangeSet {
                changed_paths,
                deleted_chunk_ids,
                strategy,
            })
        }

        ChangeStrategy::Mtime { since: since_ts } => {
            detect_mtime(corpus, db, source_path, *since_ts, strategy)
        }

        ChangeStrategy::Git { since_ref } => detect_git(
            corpus,
            db,
            source_path,
            git_root_candidate,
            since_ref.clone(),
            strategy,
        ),
    }
}

// ---------------------------------------------------------------------------
// Strategy selection
// ---------------------------------------------------------------------------

fn determine_strategy(
    corpus: &Corpus,
    since: Option<&str>,
    has_git: bool,
) -> anyhow::Result<ChangeStrategy> {
    if let Some(s) = since {
        // Try to parse as ISO 8601 date / datetime first.
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(s) {
            return Ok(ChangeStrategy::Mtime {
                since: ts.with_timezone(&Utc),
            });
        }
        // Try YYYY-MM-DD
        if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            let ts = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
            return Ok(ChangeStrategy::Mtime { since: ts });
        }
        // Otherwise treat as git ref if .git exists.
        if has_git {
            return Ok(ChangeStrategy::Git {
                since_ref: s.to_string(),
            });
        }
        // No git and couldn't parse as date — fall through to Full.
        return Ok(ChangeStrategy::Full);
    }

    // No --since flag: use corpus.last_indexed_at if available.
    if let Some(ref last) = corpus.last_indexed_at
        && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(last)
    {
        return Ok(ChangeStrategy::Mtime {
            since: ts.with_timezone(&Utc),
        });
    }

    Ok(ChangeStrategy::Full)
}

// ---------------------------------------------------------------------------
// Mtime strategy
// ---------------------------------------------------------------------------

fn detect_mtime(
    corpus: &Corpus,
    db: &dyn StorageBackend,
    source_path: &Path,
    since: DateTime<Utc>,
    strategy: ChangeStrategy,
) -> anyhow::Result<ChangeSet> {
    let mut changed_paths = Vec::new();
    let mut deleted_chunk_ids = Vec::new();

    if !source_path.exists() {
        // Entire source gone — all chunks are orphaned.
        deleted_chunk_ids = db.chunk_list_ids(&corpus.id)?;
        return Ok(ChangeSet {
            changed_paths,
            deleted_chunk_ids,
            strategy,
        });
    }

    // Walk the source path (works for both single files and directories).
    for entry in walkdir::WalkDir::new(source_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| {
                let dur = t.duration_since(std::time::UNIX_EPOCH).ok()?;
                DateTime::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
            });

        if let Some(mtime) = mtime
            && mtime > since
        {
            changed_paths.push(entry.path().to_string_lossy().into_owned());
        }
    }

    Ok(ChangeSet {
        changed_paths,
        deleted_chunk_ids,
        strategy,
    })
}

// ---------------------------------------------------------------------------
// Git strategy
// ---------------------------------------------------------------------------

fn detect_git(
    corpus: &Corpus,
    db: &dyn StorageBackend,
    source_path: &Path,
    git_root: &Path,
    since_ref: String,
    strategy: ChangeStrategy,
) -> anyhow::Result<ChangeSet> {
    // Run: git diff --name-only <since_ref> HEAD
    let changed_output = std::process::Command::new("git")
        .args([
            "-C",
            &git_root.to_string_lossy(),
            "diff",
            "--name-only",
            &since_ref,
            "HEAD",
        ])
        .output()?;

    let changed_rel_paths: Vec<String> = String::from_utf8_lossy(&changed_output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // Build absolute paths; filter to those that actually exist.
    let changed_paths: Vec<String> = changed_rel_paths
        .iter()
        .map(|rel| git_root.join(rel).to_string_lossy().into_owned())
        .filter(|abs| Path::new(abs).exists())
        .collect();

    // Run: git diff --name-only --diff-filter=D <since_ref> HEAD to find deleted files.
    let deleted_output = std::process::Command::new("git")
        .args([
            "-C",
            &git_root.to_string_lossy(),
            "diff",
            "--name-only",
            "--diff-filter=D",
            &since_ref,
            "HEAD",
        ])
        .output()?;

    let deleted_rel_paths: Vec<String> = String::from_utf8_lossy(&deleted_output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // For each deleted file, find chunks whose location_uri starts with
    // a path derived from the source root.
    let mut deleted_chunk_ids: Vec<String> = Vec::new();
    if !deleted_rel_paths.is_empty() {
        let source_prefix = source_path.to_string_lossy().into_owned();
        let all_chunks = db.chunk_list(&corpus.id)?;
        let deleted_abs: HashSet<String> = deleted_rel_paths
            .iter()
            .map(|rel| git_root.join(rel).to_string_lossy().into_owned())
            .collect();

        for chunk in &all_chunks {
            // Match by checking if the chunk's location path corresponds to a deleted file.
            // For directory-based sources, the location_uri path may encode the relative file path.
            let loc_path = &chunk.location.path;
            for del_path in &deleted_abs {
                // Compute relative path of deleted file from source root.
                let rel = del_path
                    .strip_prefix(&source_prefix)
                    .unwrap_or(del_path)
                    .trim_start_matches('/');
                if loc_path.starts_with(rel) || loc_path == rel {
                    deleted_chunk_ids.push(chunk.id.clone());
                    break;
                }
            }
        }
    }

    Ok(ChangeSet {
        changed_paths,
        deleted_chunk_ids,
        strategy,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_all_source_paths(source_path: &Path) -> Vec<String> {
    if source_path.is_file() {
        return vec![source_path.to_string_lossy().into_owned()];
    }
    walkdir::WalkDir::new(source_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::{
        storage::{SqliteBackend, StorageBackend},
        types::{Chunk, Corpus, Location},
    };

    use super::detect;

    fn make_corpus(source: &str) -> (SqliteBackend, Corpus) {
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "test".to_string(),
            "Test".to_string(),
            "fake".to_string(),
            source.to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
        (db, corpus)
    }

    /// Seed a chunk into the DB so we can test deleted_chunk_ids logic.
    fn seed_chunk(db: &SqliteBackend, corpus_id: &str, id: &str, location_path: &str) {
        let chunk = Chunk {
            id: id.to_string(),
            corpus_id: corpus_id.to_string(),
            parent_path: None,
            kind: "scene".to_string(),
            location: Location::new(corpus_id, location_path),
            content: format!("content of {}", id),
            byte_length: 10,
            created_at: chrono::Utc::now().to_rfc3339(),
            source_hash: None,
            introduced_at_version: None,
            last_modified_at_version: None,
        };
        db.chunk_upsert(&chunk).unwrap();
    }

    #[test]
    fn mtime_strategy_detects_recently_created_files() {
        let dir = tempfile::tempdir().unwrap();
        let file_a = dir.path().join("a.txt");
        let file_b = dir.path().join("b.txt");
        std::fs::write(&file_a, "hello").unwrap();
        std::fs::write(&file_b, "world").unwrap();

        // Both files were just created (mtime ≈ now).
        // Setting since to 2 hours in the past → both files should be detected.
        let since_past = (Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        let mut corpus = Corpus::new(
            "test".to_string(),
            "Test".to_string(),
            "fake".to_string(),
            dir.path().to_string_lossy().into_owned(),
        );
        corpus.last_indexed_at = Some(since_past);

        let db = SqliteBackend::open_in_memory().unwrap();
        db.corpus_insert(&corpus).unwrap();

        let change_set = detect(&corpus, &db, None).unwrap();
        assert_eq!(
            change_set.changed_paths.len(),
            2,
            "both files should be detected"
        );
    }

    #[test]
    fn mtime_strategy_no_changes_when_since_is_future() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        // since = 1 hour in the future → no files should be detected.
        let since_future = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let (db, mut corpus) = make_corpus(dir.path().to_str().unwrap());
        corpus.last_indexed_at = Some(since_future);
        // Update the corpus source to point at the real dir.
        corpus.source = dir.path().to_string_lossy().into_owned();
        db.corpus_insert(&corpus).ok(); // may fail if already inserted

        let change_set = detect(&corpus, &db, None).unwrap();
        assert_eq!(
            change_set.changed_paths.len(),
            0,
            "no files should be newer than future since"
        );
    }

    #[test]
    fn full_strategy_when_no_last_indexed_at() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x.txt"), "x").unwrap();

        let corpus = Corpus::new(
            "test".to_string(),
            "Test".to_string(),
            "fake".to_string(),
            dir.path().to_string_lossy().into_owned(),
        );
        let db = SqliteBackend::open_in_memory().unwrap();
        db.corpus_insert(&corpus).unwrap();

        let change_set = detect(&corpus, &db, None).unwrap();
        matches!(change_set.strategy, super::ChangeStrategy::Full);
        assert_eq!(change_set.changed_paths.len(), 1);
    }

    #[test]
    fn deleted_source_returns_all_chunk_ids_as_deleted() {
        let (db, mut corpus) = make_corpus("/nonexistent/path/that/does/not/exist.epub");
        corpus.last_indexed_at = Some(Utc::now().to_rfc3339());
        db.corpus_insert(&corpus).ok(); // ignore duplicate key

        seed_chunk(&db, "test", "chunk-1", "ch/1");
        seed_chunk(&db, "test", "chunk-2", "ch/2");

        let change_set = detect(&corpus, &db, None).unwrap();
        assert_eq!(change_set.changed_paths.len(), 0);
        assert_eq!(change_set.deleted_chunk_ids.len(), 2);
    }

    #[test]
    #[cfg(unix)]
    fn git_strategy_selected_when_since_is_ref_and_git_exists() {
        // Only runs when `git` is in PATH.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        // Init a git repo.
        std::process::Command::new("git")
            .args(["-C", dir.path().to_str().unwrap(), "init"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-C",
                dir.path().to_str().unwrap(),
                "config",
                "user.email",
                "test@test.com",
            ])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args([
                "-C",
                dir.path().to_str().unwrap(),
                "config",
                "user.name",
                "Test",
            ])
            .output()
            .unwrap();

        let file = dir.path().join("a.txt");
        std::fs::write(&file, "initial").unwrap();

        std::process::Command::new("git")
            .args(["-C", dir.path().to_str().unwrap(), "add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", dir.path().to_str().unwrap(), "commit", "-m", "init"])
            .output()
            .unwrap();

        // Modify the file and commit.
        std::fs::write(&file, "modified").unwrap();
        std::process::Command::new("git")
            .args(["-C", dir.path().to_str().unwrap(), "add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["-C", dir.path().to_str().unwrap(), "commit", "-m", "change"])
            .output()
            .unwrap();

        let (db, corpus) = make_corpus(dir.path().to_str().unwrap());

        let change_set = detect(&corpus, &db, Some("HEAD~1")).unwrap();
        assert!(matches!(
            change_set.strategy,
            super::ChangeStrategy::Git { .. }
        ));
        assert_eq!(change_set.changed_paths.len(), 1);
        assert!(change_set.changed_paths[0].ends_with("a.txt"));
    }
}
