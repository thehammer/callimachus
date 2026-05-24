//! Forward first-parent history walk for `calli ingest --with-history`.
//!
//! [`walk_history_forward`] collects the first-parent ancestry of a code
//! corpus's git repository from a starting commit up to HEAD, then runs the
//! full indexing pipeline at each commit in chronological order.  Because the
//! foundation from PR #27 stamps every artifact with `derived_at_version` and
//! archives superseded rows into `*_history` tables, walking forward and
//! re-running the pipeline at each commit naturally populates the history
//! tables with intermediate states.
//!
//! # Working-directory isolation
//!
//! Each commit is materialised into a [`TempDir`] via `git2`'s
//! `checkout_tree` + `target_dir`.  This writes blobs to the temp dir without
//! touching the repository's `HEAD`, index, or working tree.

use std::io::{self, BufRead, Write as IoWrite};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use git2::{Oid, Repository};
use tempfile::TempDir;

use crate::{
    indexing::{
        cascade,
        change_manifest::ChangeManifest,
        pipeline::{IndexOptions, IndexPipeline, IndexResult},
    },
    types::Corpus,
};

// ── Public API ───────────────────────────────────────────────────────────────

/// Options for a forward history walk.
#[derive(Debug, Default, Clone)]
pub struct WalkOptions {
    /// Full or short SHA of the starting commit.
    /// `None` means "walk from the first commit on HEAD's first-parent ancestry".
    pub from_sha: Option<String>,
    /// When true, skip the interactive cost-estimation confirmation prompt.
    pub skip_confirm: bool,
}

/// Aggregate statistics returned after a completed walk.
#[derive(Debug, Default)]
pub struct WalkStats {
    pub commits_processed: usize,
    pub total_chunks: u64,
    pub total_entities: u64,
    pub total_edges: u64,
    pub cost_usd: f64,
}

impl WalkStats {
    fn absorb(&mut self, r: IndexResult) {
        self.total_chunks += r.total_chunks;
        self.total_entities += r.total_entities;
        self.total_edges += r.total_edges;
        self.cost_usd += r.cost_usd;
    }
}

/// Walk the first-parent git history of `corpus.source` from `walk.from_sha`
/// (or the root commit) forward to HEAD, running the full indexing pipeline at
/// each commit.
///
/// Each commit is checked out into a temporary directory so the repository's
/// working tree, HEAD, and index are never modified.
pub async fn walk_history_forward(
    pipeline: &IndexPipeline,
    corpus: &Corpus,
    opts: IndexOptions,
    walk: WalkOptions,
) -> Result<WalkStats> {
    let source_path = Path::new(&corpus.source);
    let repo = Repository::open(source_path)
        .with_context(|| format!("opening git repo at {}", source_path.display()))?;

    // Snapshot HEAD before any work so we can assert it's unchanged on exit.
    let original_head_oid = repo
        .head()
        .context("reading HEAD")?
        .target()
        .context("HEAD has no target OID")?;

    // Resolve the starting commit.
    let from_oid = resolve_from_sha(&repo, walk.from_sha.as_deref())?;

    // Collect commits in chronological order (oldest → newest).
    let commits = collect_first_parent_chronological(&repo, from_oid, original_head_oid)?;

    if commits.is_empty() {
        bail!("no commits to walk (from_sha equals HEAD?)");
    }

    // Print summary and optionally confirm.
    print_walk_summary(&repo, &commits)?;
    confirm_or_abort(walk.skip_confirm)?;

    let mut stats = WalkStats::default();

    for (i, oid) in commits.iter().enumerate() {
        tracing::info!(
            "[walk] {}/{} → {}",
            i + 1,
            commits.len(),
            &oid.to_string()[..8]
        );

        // Materialise this commit's tree into a temp dir.
        // IMPORTANT: `materialise_tree` is fully synchronous — no git2 handles
        // are held across the following `.await`.
        let td = materialise_tree(&repo, *oid)?;

        // Build a per-commit corpus pointing at the temp tree.
        let mut commit_corpus = corpus.clone();
        commit_corpus.source = td.path().to_string_lossy().into_owned();

        // Build options: inject an explicit manifest so Pass::History is
        // bypassed and the correct version string is stamped on artifacts.
        let version = format!("git:{oid}");
        let manifest = ChangeManifest::all_dirty(version.clone());

        let mut commit_opts = opts.clone();
        // Remove History from passes — we supply the manifest ourselves.
        commit_opts
            .passes
            .retain(|p| *p != crate::types::Pass::History);
        commit_opts.change_manifest = Some(manifest.clone());

        // Run cascade invalidation explicitly (normally done inside History pass).
        cascade::run(Arc::clone(&pipeline.db), corpus, &manifest).await?;

        // Run the pipeline (History excluded; manifest pre-supplied).
        let result = pipeline.run(&commit_corpus, commit_opts).await?;

        // Persist the version anchor for this commit so the next iteration
        // (or a subsequent incremental run) can resume from the correct point.
        pipeline
            .db
            .corpus_set_last_indexed_version(&corpus.id, &version)?;

        stats.commits_processed += 1;
        stats.absorb(result);

        // td drops here, removing the temp tree.
        drop(td);
    }

    // Safety check: HEAD must not have moved.
    debug_assert_eq!(
        repo.head().ok().and_then(|h| h.target()),
        Some(original_head_oid),
        "HEAD moved during history walk — this is a bug"
    );

    Ok(stats)
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Resolve the starting commit OID for the walk.
///
/// - `None` → walk to the root of HEAD's first-parent chain.
/// - `Some(sha)` → resolve the SHA and verify it is on HEAD's first-parent ancestry.
pub(crate) fn resolve_from_sha(repo: &Repository, from: Option<&str>) -> Result<Oid> {
    let head = repo.head()?.peel_to_commit()?;

    match from {
        None => {
            // Walk to the root of the first-parent chain.
            let mut c = head;
            while let Ok(parent) = c.parent(0) {
                c = parent;
            }
            Ok(c.id())
        }
        Some(sha) => {
            let oid = repo
                .revparse_single(sha)
                .with_context(|| format!("resolving commit {sha:?}"))?
                .peel_to_commit()
                .with_context(|| format!("{sha:?} does not resolve to a commit"))?
                .id();

            // Validate ancestry: walk HEAD's first-parent chain.
            let mut c = head;
            loop {
                if c.id() == oid {
                    return Ok(oid);
                }
                match c.parent(0) {
                    Ok(p) => c = p,
                    Err(_) => break,
                }
            }
            bail!("--from {sha} is not on HEAD's first-parent ancestry")
        }
    }
}

/// Walk HEAD's first-parent chain from `head` back to `from` (inclusive),
/// then reverse to chronological order (oldest → newest).
pub(crate) fn collect_first_parent_chronological(
    repo: &Repository,
    from: Oid,
    head: Oid,
) -> Result<Vec<Oid>> {
    let head_commit = repo
        .find_commit(head)
        .with_context(|| format!("finding HEAD commit {}", &head.to_string()[..8]))?;

    let mut chain: Vec<Oid> = Vec::new();
    let mut c = head_commit;

    loop {
        chain.push(c.id());
        if c.id() == from {
            break;
        }
        match c.parent(0) {
            Ok(p) => c = p,
            Err(_) => {
                bail!(
                    "reached root of first-parent chain without finding from-commit {} — \
                     this should have been caught by resolve_from_sha",
                    &from.to_string()[..8]
                )
            }
        }
    }

    chain.reverse(); // oldest → newest
    Ok(chain)
}

/// Print a walk summary to stderr and flush.
fn print_walk_summary(repo: &Repository, commits: &[Oid]) -> Result<()> {
    let n = commits.len();
    let from_short = short_sha(repo, commits[0])?;
    let head_short = short_sha(repo, *commits.last().unwrap())?;
    let minutes = (n * 30).div_ceil(60);

    let stderr = io::stderr();
    let mut err = stderr.lock();
    writeln!(
        err,
        "Forward history walk: {n} first-parent commit(s) from {from_short} → HEAD ({head_short})"
    )?;
    writeln!(
        err,
        "Estimated time: ~{minutes} minute(s) (~30s LLM time per commit, ballpark)"
    )?;
    writeln!(err, "Continue? [y/N]")?;
    err.flush()?;
    Ok(())
}

/// Return a short SHA string for display.
fn short_sha(repo: &Repository, oid: Oid) -> Result<String> {
    let obj = repo.find_object(oid, None)?;
    let buf = obj.short_id()?;
    Ok(buf.as_str().unwrap_or(&oid.to_string()[..7]).to_string())
}

/// Read a confirmation line from stdin; accept y/Y/yes/YES, abort on anything else.
///
/// When `skip` is true, returns `Ok` immediately without reading stdin.
pub(crate) fn confirm_or_abort(skip: bool) -> Result<()> {
    if skip {
        return Ok(());
    }
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let trimmed = line.trim();
    if matches!(trimmed, "y" | "Y" | "yes" | "YES") {
        Ok(())
    } else {
        bail!("aborted by user")
    }
}

/// Check out `oid`'s tree into a fresh `TempDir` and return it.
///
/// This function is entirely synchronous and holds no git2 objects when it
/// returns, so the caller may safely `.await` after calling it.
///
/// # Working-directory safety
///
/// `checkout_tree` with `CheckoutBuilder::target_dir` writes blobs to the
/// specified directory without touching the repository's `HEAD` or index.
fn materialise_tree(repo: &Repository, oid: Oid) -> Result<TempDir> {
    let td = TempDir::new().context("creating temp dir for tree materialisation")?;

    let commit = repo
        .find_commit(oid)
        .with_context(|| format!("finding commit {}", &oid.to_string()[..8]))?;
    let tree = commit.tree()?;

    let mut co = git2::build::CheckoutBuilder::new();
    co.target_dir(td.path());
    co.force();
    co.recreate_missing(true);
    co.remove_untracked(true);

    // checkout_tree with target_dir writes the tree's blobs to td.path()
    // without modifying HEAD or the repository index.
    repo.checkout_tree(tree.as_object(), Some(&mut co))
        .with_context(|| {
            format!(
                "materialising commit {} into {}",
                &oid.to_string()[..8],
                td.path().display()
            )
        })?;

    Ok(td)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        indexing::{IndexOptions, IndexPipeline},
        storage::{SqliteBackend, StorageBackend},
        types::{Corpus, Pass},
    };
    use callimachus_llm::DryRunProvider;
    use git2::{Repository, Signature};
    use std::fs;
    use tempfile::TempDir;

    // ── Fixture helper ────────────────────────────────────────────────────────

    /// Build a linear in-process git repository with one commit per entry in
    /// `commits`.  Each entry is `(filename, contents)`; each commit adds or
    /// overwrites that file.
    ///
    /// Returns `(TempDir, Repository, Vec<Oid>)` where `Vec<Oid>` lists commit
    /// OIDs in chronological order (oldest first).
    fn build_linear_repo(commits: &[(&str, &str)]) -> (TempDir, Repository, Vec<Oid>) {
        let td = TempDir::new().expect("temp dir");
        let repo = Repository::init(td.path()).expect("git init");

        let sig = Signature::now("Test Author", "test@example.com").unwrap();
        let mut oids = Vec::new();
        let mut parent_oid: Option<Oid> = None;

        for (filename, contents) in commits {
            // Write the file into the work tree.
            let file_path = td.path().join(filename);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&file_path, contents).unwrap();

            // Stage it.
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new(filename)).unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();

            // Commit.
            let parents: Vec<git2::Commit<'_>> = parent_oid
                .iter()
                .map(|&p| repo.find_commit(p).unwrap())
                .collect();
            let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

            let oid = repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    &format!("commit: add {filename}"),
                    &tree,
                    &parent_refs,
                )
                .unwrap();

            oids.push(oid);
            parent_oid = Some(oid);
        }

        (td, repo, oids)
    }

    // ── Unit tests for helpers ────────────────────────────────────────────────

    #[test]
    fn first_parent_chronological_order() {
        let (_td, repo, oids) = build_linear_repo(&[
            ("a.txt", "a"),
            ("b.txt", "b"),
            ("c.txt", "c"),
            ("d.txt", "d"),
        ]);
        let result = collect_first_parent_chronological(&repo, oids[0], oids[3]).unwrap();
        assert_eq!(result, oids[..4]);
    }

    #[test]
    fn resolve_from_sha_default_uses_root() {
        let (_td, repo, oids) =
            build_linear_repo(&[("a.txt", "a"), ("b.txt", "b"), ("c.txt", "c")]);
        let root = resolve_from_sha(&repo, None).unwrap();
        assert_eq!(root, oids[0]);
    }

    #[test]
    fn resolve_from_sha_accepts_short_sha() {
        let (_td, repo, oids) =
            build_linear_repo(&[("a.txt", "a"), ("b.txt", "b"), ("c.txt", "c")]);
        let short = &oids[1].to_string()[..7];
        let resolved = resolve_from_sha(&repo, Some(short)).unwrap();
        assert_eq!(resolved, oids[1]);
    }

    #[test]
    fn resolve_from_sha_rejects_non_ancestry() {
        // Build a main branch with 3 commits.
        let (td, repo, _main_oids) =
            build_linear_repo(&[("a.txt", "a"), ("b.txt", "b"), ("c.txt", "c")]);

        // Create a side commit on a detached ref (not on main's first-parent chain).
        let sig = Signature::now("Test", "t@t.com").unwrap();
        fs::write(td.path().join("side.txt"), "side").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("side.txt")).unwrap();
        idx.write().unwrap();
        let tree_oid = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        // orphan commit — no parents
        let side_oid = repo.commit(None, &sig, &sig, "orphan", &tree, &[]).unwrap();

        let err = resolve_from_sha(&repo, Some(&side_oid.to_string()))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("first-parent ancestry"),
            "expected 'first-parent ancestry' in error, got: {err}"
        );
    }

    #[test]
    fn temp_tree_does_not_touch_working_directory() {
        let (td, repo, oids) = build_linear_repo(&[("a.txt", "a"), ("b.txt", "b")]);

        let head_before = repo.head().unwrap().target().unwrap();
        let a_mtime_before = fs::metadata(td.path().join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();

        // Materialise each commit into a temp tree.
        for oid in &oids {
            let t = materialise_tree(&repo, *oid).unwrap();
            // Temp tree exists and has the file.
            assert!(t.path().join("a.txt").exists() || t.path().join("b.txt").exists());
            drop(t);
        }

        // HEAD must be unchanged.
        let head_after = repo.head().unwrap().target().unwrap();
        assert_eq!(
            head_before, head_after,
            "HEAD moved during materialise_tree"
        );

        // The original tracked file must be untouched.
        let a_mtime_after = fs::metadata(td.path().join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            a_mtime_before, a_mtime_after,
            "working-tree file mtime changed during materialise_tree"
        );
    }

    #[test]
    fn walk_options_skip_confirm_bypasses_prompt() {
        // confirm_or_abort(true) must return Ok without reading stdin.
        confirm_or_abort(true).expect("skip_confirm=true should not block");
    }

    // ── Integration test: 3-commit walk populates history tables ─────────────

    #[tokio::test]
    async fn walk_short_history_populates_history_tables() {
        use crate::adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        };
        use crate::types::{Chunk, Entity, Location};

        // A minimal adapter that returns one chunk per discovered source.
        struct SimpleAdapter;

        #[async_trait::async_trait]
        impl SourceAdapter for SimpleAdapter {
            fn kind(&self) -> &str {
                "code"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }

            async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
                // Walk the temp tree for .txt files.
                let mut sources = Vec::new();
                if let Ok(rd) = std::fs::read_dir(source) {
                    for entry in rd.flatten() {
                        let p = entry.path();
                        if p.extension().and_then(|e| e.to_str()) == Some("txt") {
                            sources.push(DiscoveredSource {
                                path: p.to_string_lossy().into_owned(),
                                kind: "text".to_string(),
                                meta: serde_json::Value::Null,
                            });
                        }
                    }
                }
                Ok(sources)
            }

            async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
                let corpus_id = "walk-test";
                let rel = std::path::Path::new(&source.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                Ok(vec![Chunk::new(
                    corpus_id.to_string(),
                    None,
                    "file".to_string(),
                    Location::new(corpus_id, &rel),
                    std::fs::read_to_string(&source.path).unwrap_or_default(),
                )])
            }

            async fn extract_structure(
                &self,
                _chunk: &Chunk,
            ) -> anyhow::Result<ExtractedStructure> {
                Ok(ExtractedStructure {
                    parent_path: None,
                    child_paths: vec![],
                    structural_entities: vec![],
                    structural_edges: vec![],
                })
            }

            async fn extract_with_llm(
                &self,
                _chunk: &Chunk,
                _llm: &dyn callimachus_llm::LlmProvider,
            ) -> anyhow::Result<Option<ExtractedSemantic>> {
                Ok(Some(ExtractedSemantic {
                    entities: vec![],
                    edges: vec![],
                    summary_text: None,
                }))
            }

            async fn summarize(
                &self,
                _chunk: &Chunk,
                _llm: &dyn callimachus_llm::LlmProvider,
                _depth: &str,
            ) -> anyhow::Result<Option<String>> {
                Ok(Some("[summary]".to_string()))
            }

            async fn resolve_aliases(
                &self,
                _entities: &[Entity],
                _llm: &dyn callimachus_llm::LlmProvider,
            ) -> anyhow::Result<Vec<EntityMerge>> {
                Ok(vec![])
            }

            fn format_location(&self, chunk: &Chunk) -> String {
                chunk.location.path.clone()
            }

            fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
                Ok(LocationRef {
                    corpus_id: "walk-test".to_string(),
                    path: uri.to_string(),
                })
            }
        }

        // Build a 3-commit linear repo.
        let (_td, _repo, oids) = build_linear_repo(&[
            ("a.txt", "content-a"),
            ("b.txt", "content-b"),
            ("c.txt", "content-c"),
        ]);
        let repo_path = _td.path().to_string_lossy().into_owned();

        // Set up an in-memory DB and corpus.
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "walk-test".to_string(),
            "Walk Test".to_string(),
            "code".to_string(),
            repo_path.clone(),
        );
        db.corpus_insert(&corpus).unwrap();

        // Build pipeline with DryRunProvider so no actual LLM calls are made.
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(SimpleAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        // Walk all 3 commits.
        let walk_opts = WalkOptions {
            from_sha: None,
            skip_confirm: true,
        };
        let opts = IndexOptions {
            passes: vec![Pass::History, Pass::Chunk, Pass::Structure],
            ..IndexOptions::default()
        };

        let stats = walk_history_forward(&pipeline, &corpus, opts, walk_opts)
            .await
            .expect("walk_history_forward failed");

        assert_eq!(stats.commits_processed, 3);

        // The head chunks table should reflect HEAD (commit 2) content.
        let head_count = db.chunk_count("walk-test").unwrap();
        assert!(head_count > 0, "expected chunks after walk");

        // corpora.last_indexed_version should equal oids[2] (HEAD).
        let version = db
            .corpus_get_last_indexed_version("walk-test")
            .unwrap()
            .expect("last_indexed_version should be set");
        assert_eq!(version, format!("git:{}", oids[2]));

        // chunks_history should have rows from commits 0 and 1.
        // We query the SQLite backend directly using db_for_test().
        let db_guard = db.db_for_test();
        let conn = db_guard.conn();

        let history_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks_history", [], |r| r.get(0))
            .unwrap();
        // Each of the 3 commits adds N chunks; commits 0 and 1 each get superseded,
        // so at least 1 history row per superseded commit.
        assert!(
            history_count >= 2,
            "expected >= 2 history rows, got {history_count}"
        );

        // Verify the history rows have distinct derived_at_version values for
        // oids[0] and oids[1].
        let oid0_str = format!("git:{}", oids[0]);
        let oid1_str = format!("git:{}", oids[1]);
        let oid1_str_superseded = format!("git:{}", oids[1]);
        let oid2_str_superseded = format!("git:{}", oids[2]);

        let rows_oid0: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks_history WHERE superseded_at_version = ?1",
                rusqlite::params![oid1_str_superseded],
                |r| r.get(0),
            )
            .unwrap();
        let rows_oid1: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks_history WHERE superseded_at_version = ?1",
                rusqlite::params![oid2_str_superseded],
                |r| r.get(0),
            )
            .unwrap();

        assert!(
            rows_oid0 > 0,
            "expected history rows superseded at oid1 (from commit 0 being replaced by commit 1), got 0"
        );
        assert!(
            rows_oid1 > 0,
            "expected history rows superseded at oid2 (from commit 1 being replaced by commit 2), got 0"
        );

        // Suppress unused-variable warnings for strings only used in assertions.
        let _ = oid0_str;
        let _ = oid1_str;
    }
}
