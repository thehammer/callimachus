//! Forward and backward first-parent history walk.
//!
//! [`walk_history_forward`] collects the first-parent ancestry of a code
//! corpus's git repository from a starting commit up to HEAD, then runs the
//! full indexing pipeline at each commit in chronological order.  Because the
//! foundation from PR #27 stamps every artifact with `derived_at_version` and
//! archives superseded rows into `*_history` tables, walking forward and
//! re-running the pipeline at each commit naturally populates the history
//! tables with intermediate states.
//!
//! [`walk_history_backward`] (Phase 2, PR #29) is the complement: given a
//! corpus that is already indexed at HEAD, it walks the first-parent ancestry
//! *backwards* from HEAD's parent down to `--from <sha>` and populates the
//! `*_history` tables for those older states without touching the head tables.
//! Because each commit's artifacts are written via [`BackfillStorageWrapper`],
//! the head tables are never modified and `last_indexed_version` is preserved.
//!
//! # Working-directory isolation
//!
//! Each commit is materialised into a [`TempDir`] via `git2`'s
//! `checkout_tree` + `target_dir`.  This writes blobs to the temp dir without
//! touching the repository's `HEAD`, index, or working tree.
//!
//! [`BackfillStorageWrapper`]: crate::storage::BackfillStorageWrapper

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
        pipeline::{IndexMode, IndexOptions, IndexPipeline, IndexResult},
    },
    storage::{BackfillStorageWrapper, BackfillSupersession},
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

/// Walk the first-parent git history of `corpus.source` *backward* from
/// HEAD's parent down to `walk.from_sha`, populating `*_history` tables for
/// each older commit without touching the head tables.
///
/// ## Pre-conditions
///
/// * The corpus **must** have been previously ingested (i.e.
///   `corpus_get_last_indexed_version` must return `Some`).  If it has not,
///   this function returns an error with the message "has not been ingested".
///
/// * `walk.from_sha` **must** be `Some`.  A `None` value (which means "start
///   from the repository root" in the forward walk) is rejected here because
///   the backward walk is always an explicit range operation.
///
/// ## Iteration order
///
/// Commits are processed newest-older → oldest-older.  This invariant ensures
/// that when artifact A first appears at commit C(k) and disappears at C(k+1),
/// writing C(k+2)'s history row first (superseded by HEAD), then C(k+1)
/// (superseded by C(k+2)), and finally C(k) (superseded by C(k+1)) produces a
/// correct, gapless supersession chain without requiring any UPDATE of already-
/// written history rows.
///
/// ## Head table safety
///
/// Each commit is run through an `IndexPipeline` whose `db` is a
/// [`BackfillStorageWrapper`] wrapping the real backend.  The wrapper
/// intercepts all artifact-upsert methods and routes them to `*_history`
/// tables; the real head tables are never touched.  `corpus_set_last_indexed_version`
/// is a NO-OP on the wrapper, so the HEAD version anchor is preserved.
pub async fn walk_history_backward(
    pipeline: &IndexPipeline,
    corpus: &Corpus,
    opts: IndexOptions,
    walk: WalkOptions,
) -> Result<WalkStats> {
    // ── Pre-conditions ────────────────────────────────────────────────────────

    // Require corpus to be already ingested.
    let _head_version = pipeline
        .db
        .corpus_get_last_indexed_version(&corpus.id)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "corpus '{}' has not been ingested — run `calli ingest {}` first",
                corpus.id,
                corpus.id
            )
        })?;

    // Require an explicit --from <sha>.
    let from_sha = walk.from_sha.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "--from <sha> is required for backfill; \
             use `calli history backfill {} --from <sha>`",
            corpus.id
        )
    })?;

    // ── Git setup ─────────────────────────────────────────────────────────────

    let source_path = Path::new(&corpus.source);
    let repo = Repository::open(source_path)
        .with_context(|| format!("opening git repo at {}", source_path.display()))?;

    // Snapshot HEAD before any work.
    let original_head_oid = repo
        .head()
        .context("reading HEAD")?
        .target()
        .context("HEAD has no target OID")?;

    // Resolve --from <sha> (must be on HEAD's first-parent ancestry).
    let from_oid = resolve_from_sha(&repo, Some(from_sha))?;

    // Collect first-parent chain: chronological (oldest → newest) includes HEAD.
    let all_commits = collect_first_parent_chronological(&repo, from_oid, original_head_oid)?;

    // Backfill targets = everything except HEAD, reversed → newest-older first.
    let backfill_targets: Vec<Oid> = all_commits
        .iter()
        .copied()
        // Drop HEAD (last element) — it is already indexed in the head tables.
        .take(all_commits.len().saturating_sub(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if backfill_targets.is_empty() {
        bail!("no commits to backfill (from_sha is HEAD's direct parent with no ancestors?)");
    }

    // ── Seed supersession resolver from HEAD ──────────────────────────────────

    let supersession = Arc::new(BackfillSupersession::seeded_from(
        pipeline.db.as_ref(),
        &corpus.id,
    )?);

    // ── Walk ──────────────────────────────────────────────────────────────────

    let mut stats = WalkStats::default();

    for (i, oid) in backfill_targets.iter().enumerate() {
        let version = format!("git:{oid}");
        tracing::info!(
            "[backfill] {}/{} → {}",
            i + 1,
            backfill_targets.len(),
            &oid.to_string()[..8]
        );

        // Set the "next-newer" version as the fallback supersession target for
        // artifacts that are absent from the supersession map (i.e. they exist
        // in this commit's tree but not in HEAD and were not seen in any
        // already-processed newer iteration).
        //
        // Iteration order is newest-older → oldest-older, so:
        //   i=0 → we are processing the newest non-HEAD commit;
        //          its next-newer is HEAD.
        //   i>0 → its next-newer is backfill_targets[i-1] (the commit we just processed).
        let next_newer_version = if i == 0 {
            format!("git:{original_head_oid}")
        } else {
            format!("git:{}", backfill_targets[i - 1])
        };
        supersession.set_current_commit(next_newer_version);

        // Materialise the tree (fully synchronous; no git2 handles across await).
        let td = materialise_tree(&repo, *oid)?;

        // Build a per-commit corpus pointing at the temp tree.
        let mut commit_corpus = corpus.clone();
        commit_corpus.source = td.path().to_string_lossy().into_owned();

        // Build a BackfillStorageWrapper for this iteration.
        let wrapper = Arc::new(BackfillStorageWrapper::new(
            Arc::clone(&pipeline.db),
            version.clone(),
            Arc::clone(&supersession),
        ));

        // Build a per-commit IndexPipeline backed by the wrapper.
        let commit_pipeline = IndexPipeline {
            db: wrapper,
            adapter: Arc::clone(&pipeline.adapter),
            llm: Arc::clone(&pipeline.llm),
            embedder: pipeline.embedder.clone(),
        };

        // Build options: supply an all-dirty manifest and enable backfill mode.
        let manifest = ChangeManifest::all_dirty(version.clone());
        let mut commit_opts = opts.clone();
        commit_opts
            .passes
            .retain(|p| *p != crate::types::Pass::History);
        commit_opts.change_manifest = Some(manifest);
        commit_opts.mode = IndexMode::HistoryBackfill;

        // Run the pipeline.  The wrapper ensures all writes go to *_history;
        // corpus_set_last_indexed_version is a NO-OP on the wrapper.
        let result = commit_pipeline.run(&commit_corpus, commit_opts).await?;

        stats.commits_processed += 1;
        stats.absorb(result);

        // td drops here, removing the temp tree.
        drop(td);
    }

    // Safety check: HEAD must not have moved.
    debug_assert_eq!(
        repo.head().ok().and_then(|h| h.target()),
        Some(original_head_oid),
        "HEAD moved during backward history walk — this is a bug"
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

    // ── Backward backfill tests ───────────────────────────────────────────────

    /// Build a corpus + pipeline pair backed by an in-memory SQLite DB.
    /// Returns `(db, corpus, pipeline, repo_td)`.
    async fn setup_ingested_corpus(
        commits: &[(&str, &str)],
    ) -> (Arc<SqliteBackend>, Corpus, IndexPipeline, TempDir, Vec<Oid>) {
        use crate::adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        };
        use crate::types::{Chunk, Entity, Location};

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
                let corpus_id = "bf-test";
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
                    corpus_id: "bf-test".to_string(),
                    path: uri.to_string(),
                })
            }
        }

        let (td, _repo, oids) = build_linear_repo(commits);
        let repo_path = td.path().to_string_lossy().into_owned();

        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "bf-test".to_string(),
            "Backfill Test".to_string(),
            "code".to_string(),
            repo_path.clone(),
        );
        db.corpus_insert(&corpus).unwrap();

        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(SimpleAdapter),
            llm: Arc::new(callimachus_llm::DryRunProvider::new()),
            embedder: None,
        };

        // Ingest HEAD commit (forward walk from root) so last_indexed_version is set.
        let walk_opts = WalkOptions {
            from_sha: None,
            skip_confirm: true,
        };
        let index_opts = IndexOptions {
            passes: vec![Pass::History, Pass::Chunk, Pass::Structure],
            ..IndexOptions::default()
        };
        walk_history_forward(&pipeline, &corpus, index_opts, walk_opts)
            .await
            .expect("forward walk failed in setup");

        (db, corpus, pipeline, td, oids)
    }

    /// Test: commits are processed newest-older → oldest-older (not chronological).
    #[tokio::test]
    async fn backfill_reverse_chronological_order() {
        let (db, corpus, pipeline, _td, oids) = setup_ingested_corpus(&[
            ("a.txt", "v1"),
            ("b.txt", "v2"),
            ("c.txt", "v3"),
            ("d.txt", "v4"),
        ])
        .await;

        // Backfill from oids[0] (oldest) — targets are oids[0..3) reversed: oids[2], oids[1], oids[0].
        let from_sha = oids[0].to_string();
        let stats = walk_history_backward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: Some(from_sha),
                skip_confirm: true,
            },
        )
        .await
        .expect("backfill failed");

        // 3 commits backfilled (oids[0], [1], [2]); HEAD (oids[3]) excluded.
        assert_eq!(stats.commits_processed, 3);

        // History rows should all have introduced_at_version matching those 3 commits.
        // Note: chunks_history uses introduced_at_version, not derived_at_version.
        let db_guard = db.db_for_test();
        let conn = db_guard.conn();
        for &oid in &oids[..3] {
            let v = format!("git:{oid}");
            let cnt: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM chunks_history WHERE introduced_at_version = ?1",
                    rusqlite::params![v],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                cnt > 0,
                "expected history rows with introduced_at_version={v}, got 0"
            );
        }
    }

    /// Test: head chunk count is unchanged after a backward backfill.
    #[tokio::test]
    async fn backfill_head_untouched() {
        let (db, corpus, pipeline, _td, oids) =
            setup_ingested_corpus(&[("a.txt", "v1"), ("b.txt", "v2"), ("c.txt", "v3")]).await;

        let head_count_before = db.chunk_count("bf-test").unwrap();
        let head_version_before = db
            .corpus_get_last_indexed_version("bf-test")
            .unwrap()
            .expect("last_indexed_version must be set");

        let from_sha = oids[0].to_string();
        walk_history_backward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: Some(from_sha),
                skip_confirm: true,
            },
        )
        .await
        .expect("backfill failed");

        let head_count_after = db.chunk_count("bf-test").unwrap();
        let head_version_after = db
            .corpus_get_last_indexed_version("bf-test")
            .unwrap()
            .expect("last_indexed_version must still be set");

        assert_eq!(
            head_count_before, head_count_after,
            "head chunk count changed after backfill"
        );
        assert_eq!(
            head_version_before, head_version_after,
            "last_indexed_version changed after backfill"
        );
    }

    /// Test: supersession chain across backfilled commits is correct.
    ///
    /// Commit order (oldest → newest): C0, C1, C2 (HEAD).
    /// Backfill walks: C1 first (superseded by HEAD version), then C0 (superseded by C1 version).
    /// So history for C1 has superseded_at_version = git:<C2>, and
    /// history for C0 has superseded_at_version = git:<C1>.
    #[tokio::test]
    async fn backfill_supersession_chain_correct() {
        let (db, corpus, pipeline, _td, oids) =
            setup_ingested_corpus(&[("a.txt", "v1"), ("b.txt", "v2"), ("c.txt", "v3")]).await;

        let from_sha = oids[0].to_string();
        walk_history_backward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: Some(from_sha),
                skip_confirm: true,
            },
        )
        .await
        .expect("backfill failed");

        let db_guard = db.db_for_test();
        let conn = db_guard.conn();

        let v1 = format!("git:{}", oids[0]);
        let v2 = format!("git:{}", oids[1]);
        let v3 = format!("git:{}", oids[2]);

        // C0's history row must be superseded by C1 (the commit immediately newer).
        // chunks_history uses introduced_at_version (not derived_at_version).
        let c0_superseded: String = conn
            .query_row(
                "SELECT superseded_at_version FROM chunks_history \
                 WHERE introduced_at_version = ?1 LIMIT 1",
                rusqlite::params![v1],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "MISSING".to_string());
        assert_eq!(
            c0_superseded, v2,
            "C0 history should be superseded by C1 ({v2}), got {c0_superseded}"
        );

        // C1's history row must be superseded by C2 (HEAD).
        let c1_superseded: String = conn
            .query_row(
                "SELECT superseded_at_version FROM chunks_history \
                 WHERE introduced_at_version = ?1 LIMIT 1",
                rusqlite::params![v2],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "MISSING".to_string());
        assert_eq!(
            c1_superseded, v3,
            "C1 history should be superseded by C2/HEAD ({v3}), got {c1_superseded}"
        );
    }

    /// Test: an artifact present in an older commit but absent from HEAD
    /// appears in history with a superseded_at_version equal to the commit
    /// that first lacked it.
    ///
    /// Commit C0 has "gone.txt"; C1 (HEAD) removes it.
    /// Backfilling C0 should produce a history row for "gone.txt"
    /// with superseded_at_version = git:<C1>.
    #[tokio::test]
    async fn backfill_artifact_missing_from_head() {
        // Build a repo where C0 adds "gone.txt" + "stays.txt",
        // C1 (HEAD) removes "gone.txt" (only overwrites "stays.txt" in our
        // simplified model, but the chunk for "gone.txt" disappears from HEAD).
        //
        // Because `build_linear_repo` adds/overwrites files without deleting
        // previous ones, we simulate removal by using an adapter that only
        // discovers .txt files in the temp tree — so "gone.txt" won't appear
        // in C1's tree unless we actually wrote it there.
        //
        // We build the repo manually: C0 has gone.txt + stays.txt; C1 only stays.txt.
        use git2::{Repository, Signature};

        let td = TempDir::new().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        let sig = Signature::now("Test", "t@t.com").unwrap();

        // C0: add both files.
        fs::write(td.path().join("gone.txt"), "old-content").unwrap();
        fs::write(td.path().join("stays.txt"), "content").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("gone.txt")).unwrap();
        idx.add_path(Path::new("stays.txt")).unwrap();
        idx.write().unwrap();
        let tree0_oid = idx.write_tree().unwrap();
        let tree0 = repo.find_tree(tree0_oid).unwrap();
        let c0 = repo
            .commit(Some("HEAD"), &sig, &sig, "C0: add both", &tree0, &[])
            .unwrap();

        // C1: remove gone.txt from the index, keep stays.txt.
        let mut idx = repo.index().unwrap();
        idx.read(true).unwrap();
        idx.remove_path(Path::new("gone.txt")).unwrap();
        idx.write().unwrap();
        let tree1_oid = idx.write_tree().unwrap();
        let tree1 = repo.find_tree(tree1_oid).unwrap();
        let c0_commit = repo.find_commit(c0).unwrap();
        let c1 = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "C1: remove gone.txt",
                &tree1,
                &[&c0_commit],
            )
            .unwrap();

        // Set up DB + pipeline.
        let repo_path = td.path().to_string_lossy().into_owned();
        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "bf-gone".to_string(),
            "Backfill Gone Test".to_string(),
            "code".to_string(),
            repo_path.clone(),
        );
        db.corpus_insert(&corpus).unwrap();

        use crate::adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        };
        use crate::types::{Chunk, Entity, Location};

        struct GoneAdapter;
        #[async_trait::async_trait]
        impl SourceAdapter for GoneAdapter {
            fn kind(&self) -> &str {
                "code"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
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
                let corpus_id = "bf-gone";
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
            async fn extract_structure(&self, _c: &Chunk) -> anyhow::Result<ExtractedStructure> {
                Ok(ExtractedStructure {
                    parent_path: None,
                    child_paths: vec![],
                    structural_entities: vec![],
                    structural_edges: vec![],
                })
            }
            async fn extract_with_llm(
                &self,
                _c: &Chunk,
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
                _c: &Chunk,
                _llm: &dyn callimachus_llm::LlmProvider,
                _depth: &str,
            ) -> anyhow::Result<Option<String>> {
                Ok(None)
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
                    corpus_id: "bf-gone".to_string(),
                    path: uri.to_string(),
                })
            }
        }

        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(GoneAdapter),
            llm: Arc::new(callimachus_llm::DryRunProvider::new()),
            embedder: None,
        };

        // Forward-walk (ingest HEAD = C1, which has only stays.txt).
        walk_history_forward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::History, Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: None,
                skip_confirm: true,
            },
        )
        .await
        .expect("forward walk failed");

        // HEAD should have 1 chunk (stays.txt only).
        assert_eq!(db.chunk_count("bf-gone").unwrap(), 1);

        // Backward backfill from C0.
        let from_sha = c0.to_string();
        walk_history_backward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: Some(from_sha),
                skip_confirm: true,
            },
        )
        .await
        .expect("backfill failed");

        // Head must still have only 1 chunk.
        assert_eq!(
            db.chunk_count("bf-gone").unwrap(),
            1,
            "head grew after backfill"
        );

        // History for gone.txt must exist, superseded by C1.
        let db_guard = db.db_for_test();
        let conn = db_guard.conn();
        let v_c0 = format!("git:{c0}");
        let v_c1 = format!("git:{c1}");

        // chunks_history uses introduced_at_version (not derived_at_version).
        // The chunk id column is "id", and location_uri contains "gone".
        let gone_history: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT introduced_at_version, superseded_at_version \
                     FROM chunks_history \
                     WHERE location_uri LIKE '%gone%'",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };

        assert!(
            !gone_history.is_empty(),
            "expected history row for gone.txt"
        );
        for (introduced, superseded) in &gone_history {
            assert_eq!(
                introduced, &v_c0,
                "gone.txt history introduced_at_version should be {v_c0}, got {introduced}"
            );
            assert_eq!(
                superseded, &v_c1,
                "gone.txt history superseded_at_version should be {v_c1} (HEAD), got {superseded}"
            );
        }
    }

    /// Test: backfill fails with a clear error when the corpus has not been ingested.
    #[tokio::test]
    async fn backfill_requires_existing_ingest() {
        let (_td, _repo, _oids) = build_linear_repo(&[("a.txt", "a"), ("b.txt", "b")]);
        let repo_path = _td.path().to_string_lossy().into_owned();

        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "not-ingested".to_string(),
            "Not Ingested".to_string(),
            "code".to_string(),
            repo_path,
        );
        db.corpus_insert(&corpus).unwrap();

        use crate::adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        };
        use crate::types::{Chunk, Entity};

        struct NoopAdapter;
        #[async_trait::async_trait]
        impl SourceAdapter for NoopAdapter {
            fn kind(&self) -> &str {
                "code"
            }
            fn version(&self) -> &str {
                "0"
            }
            async fn discover(&self, _: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
                Ok(vec![])
            }
            async fn chunk(&self, _: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
                Ok(vec![])
            }
            async fn extract_structure(&self, _: &Chunk) -> anyhow::Result<ExtractedStructure> {
                Ok(ExtractedStructure {
                    parent_path: None,
                    child_paths: vec![],
                    structural_entities: vec![],
                    structural_edges: vec![],
                })
            }
            async fn extract_with_llm(
                &self,
                _: &Chunk,
                _: &dyn callimachus_llm::LlmProvider,
            ) -> anyhow::Result<Option<ExtractedSemantic>> {
                Ok(None)
            }
            async fn summarize(
                &self,
                _: &Chunk,
                _: &dyn callimachus_llm::LlmProvider,
                _: &str,
            ) -> anyhow::Result<Option<String>> {
                Ok(None)
            }
            async fn resolve_aliases(
                &self,
                _: &[Entity],
                _: &dyn callimachus_llm::LlmProvider,
            ) -> anyhow::Result<Vec<EntityMerge>> {
                Ok(vec![])
            }
            fn format_location(&self, chunk: &Chunk) -> String {
                chunk.location.path.clone()
            }
            fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
                Ok(LocationRef {
                    corpus_id: "not-ingested".to_string(),
                    path: uri.to_string(),
                })
            }
        }

        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(NoopAdapter),
            llm: Arc::new(callimachus_llm::DryRunProvider::new()),
            embedder: None,
        };

        let err = walk_history_backward(
            &pipeline,
            &corpus,
            IndexOptions::default(),
            WalkOptions {
                from_sha: Some("HEAD".to_string()),
                skip_confirm: true,
            },
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("has not been ingested"),
            "expected 'has not been ingested' in error, got: {err}"
        );
    }

    /// Test: backfill fails with a clear error when `from_sha` is None.
    #[tokio::test]
    async fn backfill_requires_from_sha() {
        let (_, corpus, pipeline, _td, _) =
            setup_ingested_corpus(&[("a.txt", "a"), ("b.txt", "b")]).await;

        let err = walk_history_backward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: None, // <-- missing
                skip_confirm: true,
            },
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("--from <sha> is required"),
            "expected '--from <sha> is required' in error, got: {err}"
        );
    }
}
