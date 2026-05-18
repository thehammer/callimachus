use callimachus_core::indexing::change_manifest::{ChangeKind, ChangedSource, CommitMeta};
use std::path::Path;

/// Basic metadata read from a git repository.
#[derive(Debug, Clone)]
pub struct GitInfo {
    pub branch: String,
    pub commit: String, // short SHA (7 chars)
    pub dirty: bool,    // uncommitted changes present
}

/// Read git repository metadata from `repo_path`.
///
/// Returns `None` if the path is not a git repository — not an error.
/// Returns an error only if the repository exists but is unreadable.
pub fn read_git_info(repo_path: &Path) -> anyhow::Result<Option<GitInfo>> {
    let repo = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("failed to open git repo: {e}")),
    };

    // Current branch name (or HEAD if detached).
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => {
            // Unborn branch (no commits yet) — treat as non-git.
            return Ok(None);
        }
    };

    let branch = if head.is_branch() {
        head.shorthand().unwrap_or("HEAD").to_string()
    } else {
        "HEAD".to_string()
    };

    // Short commit SHA.
    let commit_oid = head
        .target()
        .ok_or_else(|| anyhow::anyhow!("HEAD has no target"))?;
    let short = &commit_oid.to_string()[..7];
    let commit = short.to_string();

    // Dirty check: any modified files in index or working tree.
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(false);
    opts.exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut opts))?;
    let dirty = statuses.iter().any(|s| {
        let st = s.status();
        st.intersects(
            git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_NEW
                | git2::Status::INDEX_DELETED
                | git2::Status::WT_MODIFIED
                | git2::Status::WT_DELETED,
        )
    });

    Ok(Some(GitInfo {
        branch,
        commit,
        dirty,
    }))
}

/// Compute the current version reference for a git repository.
///
/// Returns `"git:<full-oid>"` when the path is a git repository with at least
/// one commit.  Returns `None` when the path is not a git repo or has no HEAD.
pub fn current_git_version(repo_path: &Path) -> anyhow::Result<Option<String>> {
    let repo = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("failed to open git repo: {e}")),
    };
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return Ok(None), // unborn branch
    };
    let oid = head
        .target()
        .ok_or_else(|| anyhow::anyhow!("HEAD has no target OID"))?;
    Ok(Some(format!("git:{oid}")))
}

/// Compute the diff between two git versions and return the list of changed sources.
///
/// Both `from_version` and `to_version` must be `"git:<full-oid>"` strings.
/// Returns `None` when the repository cannot be opened or a version string
/// cannot be parsed — the caller falls back to a full rescan.
pub fn diff_between(
    repo_path: &Path,
    from_version: &str,
    to_version: &str,
) -> anyhow::Result<Option<Vec<ChangedSource>>> {
    // Parse OID strings from "git:<oid>" prefix.
    let from_oid = match parse_git_version(from_version) {
        Some(s) => s,
        None => return Ok(None),
    };
    let to_oid = match parse_git_version(to_version) {
        Some(s) => s,
        None => return Ok(None),
    };

    let repo = match git2::Repository::open(repo_path) {
        Ok(r) => r,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("failed to open git repo: {e}")),
    };

    // Parse OIDs.
    let from_oid = match git2::Oid::from_str(&from_oid) {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };
    let to_oid = match git2::Oid::from_str(&to_oid) {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };

    // Find the tree for each commit.
    let from_commit = repo.find_commit(from_oid)?;
    let to_commit = repo.find_commit(to_oid)?;
    let from_tree = from_commit.tree()?;
    let to_tree = to_commit.tree()?;

    // Collect commit metadata from `to_commit`.
    let to_author = to_commit.author();
    let to_meta = CommitMeta {
        sha: to_oid.to_string(),
        message: to_commit.message().unwrap_or("").trim().to_string(),
        author: format!(
            "{} <{}>",
            to_author.name().unwrap_or(""),
            to_author.email().unwrap_or("")
        ),
        date: {
            let time = to_commit.time();
            chrono::DateTime::from_timestamp(time.seconds(), 0)
                .map(|dt: chrono::DateTime<chrono::Utc>| dt.to_rfc3339())
                .unwrap_or_default()
        },
    };

    let diff = repo.diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)?;

    let mut changed: Vec<ChangedSource> = Vec::new();
    diff.foreach(
        &mut |delta, _| {
            let kind = match delta.status() {
                git2::Delta::Added => ChangeKind::Added,
                git2::Delta::Deleted => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            };
            // Use the new-file path (or old-file path for deletes).
            let file = if delta.status() == git2::Delta::Deleted {
                delta.old_file()
            } else {
                delta.new_file()
            };
            if let Some(p) = file.path() {
                changed.push(ChangedSource {
                    path: p.to_string_lossy().to_string(),
                    kind,
                    commit_meta: Some(to_meta.clone()),
                });
            }
            true
        },
        None,
        None,
        None,
    )?;

    Ok(Some(changed))
}

/// Extract the raw OID hex string from a `"git:<oid>"` version string.
fn parse_git_version(version: &str) -> Option<String> {
    version.strip_prefix("git:").map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn non_git_dir_returns_none() {
        let dir = std::env::temp_dir();
        let result = read_git_info(&dir);
        assert!(result.is_ok(), "should not error on non-git directory");
        assert!(
            result.unwrap().is_none(),
            "should return None for non-git directory"
        );
    }

    #[test]
    fn callimachus_repo_returns_info() {
        // Find the callimachus repo root by walking up from this file.
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // Go up to workspace root (4 levels: callimachus-adapter-code → adapters → crates → root)
        for _ in 0..3 {
            path.pop();
        }

        let git_dir = path.join(".git");
        if !git_dir.exists() {
            // Skip if running outside a git repo (e.g., in a release tarball).
            return;
        }

        let result = read_git_info(&path);
        assert!(result.is_ok(), "should not error on callimachus repo");
        let info = result.unwrap();
        assert!(info.is_some(), "callimachus repo should return git info");
        let info = info.unwrap();
        assert!(!info.commit.is_empty(), "commit should not be empty");
        assert!(!info.branch.is_empty(), "branch should not be empty");
    }
}
