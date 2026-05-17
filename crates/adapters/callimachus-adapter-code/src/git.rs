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
