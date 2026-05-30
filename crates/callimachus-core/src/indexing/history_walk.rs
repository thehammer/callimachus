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
        history_layer::{self, CommitPlan, RemovedArtifact, WalkDirection},
        pipeline::{IndexMode, IndexOptions, IndexPipeline, IndexResult, ReadView},
    },
    storage::{BackfillStorageWrapper, BackfillSupersession, VirtualHead},
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

        // First commit (root / `--from` start): no neighbour to diff against, so
        // derive everything from scratch. Subsequent commits diff against the
        // previously-processed (older) commit and re-derive only the changed
        // files, copying every unchanged file's artifacts forward instead.
        let neighbour = (i > 0).then(|| format!("git:{}", commits[i - 1]));
        let manifest = match &neighbour {
            None => ChangeManifest::all_dirty(version.clone()),
            Some(neighbour) => {
                let changed =
                    pipeline
                        .adapter
                        .changed_sources(&corpus.source, Some(neighbour), &version)?;
                ChangeManifest::from_changed(version.clone(), changed)
            }
        };

        // Snapshot the head artifacts *before* the cascade sweeps them, so we
        // can tombstone the ones that don't come back (deleted files, removed
        // functions, dropped edges). Artifact ids that are re-derived keep the
        // same id; ones that vanish between the pre- and post-pipeline states
        // are genuine removals. Captured corpus-wide: unchanged artifacts are
        // never deleted under the diff-based walk, so they appear in both
        // snapshots and are not tombstoned — only true disappearances are.
        let (pre_chunks, pre_entities, pre_edges): (
            std::collections::HashSet<String>,
            std::collections::HashSet<String>,
            std::collections::HashSet<String>,
        ) = if neighbour.is_some() {
            (
                pipeline
                    .db
                    .chunk_list(&corpus.id)?
                    .into_iter()
                    .map(|c| c.id)
                    .collect(),
                pipeline
                    .db
                    .entity_list(&corpus.id)?
                    .into_iter()
                    .map(|e| e.id)
                    .collect(),
                pipeline
                    .db
                    .edge_list(&corpus.id)?
                    .into_iter()
                    .map(|e| e.id)
                    .collect(),
            )
        } else {
            Default::default()
        };

        let mut commit_opts = opts.clone();
        // Remove History from passes — we supply the manifest ourselves.
        commit_opts
            .passes
            .retain(|p| *p != crate::types::Pass::History);
        commit_opts.change_manifest = Some(manifest.clone());

        // Archive + delete the dirty subtree (and supersede themes) via the
        // history layer — the sole writer of provenance + tombstones. No
        // copy-forward of unchanged artifacts: rows untouched by this commit's
        // diff keep their existing (older, honest) provenance tag.
        cascade::run(Arc::clone(&pipeline.db), corpus, &manifest).await?;

        // Run the pipeline (History excluded; manifest pre-supplied). The passes
        // re-derive only the dirty subtree and stamp it at this commit.
        let result = pipeline.run(&commit_corpus, commit_opts).await?;

        // Tombstone artifacts present before the diff but absent after
        // re-derivation (deleted files, removed functions, dropped edges).
        if neighbour.is_some() {
            let post_chunks: std::collections::HashSet<String> = pipeline
                .db
                .chunk_list(&corpus.id)?
                .into_iter()
                .map(|c| c.id)
                .collect();
            let post_entities: std::collections::HashSet<String> = pipeline
                .db
                .entity_list(&corpus.id)?
                .into_iter()
                .map(|e| e.id)
                .collect();
            let post_edges: std::collections::HashSet<String> = pipeline
                .db
                .edge_list(&corpus.id)?
                .into_iter()
                .map(|e| e.id)
                .collect();

            let removed: Vec<RemovedArtifact> =
                removed_artifacts("chunk", &pre_chunks, &post_chunks)
                    .chain(removed_artifacts("entity", &pre_entities, &post_entities))
                    .chain(removed_artifacts("edge", &pre_edges, &post_edges))
                    .collect();

            if !removed.is_empty() {
                history_layer::commit(
                    pipeline.db.as_ref(),
                    &corpus.id,
                    &version,
                    WalkDirection::Forward,
                    &CommitPlan {
                        dirty_chunk_ids: Vec::new(),
                        archive_themes: false,
                        tombstones: removed,
                    },
                )?;
            }
        }

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

    // ── Resume cursor ──────────────────────────────────────────────────────────
    //
    // The next commit to process is determined purely from the in-memory
    // `backfill_targets` vector and the persisted cursor — never inferred from
    // on-disk history (the root cause of the resume-stuck bug). The cursor holds
    // the version of the next target to process; on entry we skip every target
    // newer than it (already completed in a prior run). A `None` cursor means
    // "start from the newest target". Re-processing the cursor's own target is
    // safe and intended: writes are idempotent (`INSERT OR IGNORE`), so a run
    // that died mid-iteration is simply re-attempted and then advances.
    let start_index = match pipeline.db.corpus_get_backfill_cursor(&corpus.id)? {
        Some(cursor) => backfill_targets
            .iter()
            .position(|oid| format!("git:{oid}") == cursor)
            .unwrap_or(0),
        None => 0,
    };
    if start_index > 0 {
        tracing::info!(
            "[backfill] resuming at target {}/{} (cursor {})",
            start_index + 1,
            backfill_targets.len(),
            &backfill_targets[start_index].to_string()[..8],
        );
    }

    // ── Walk ──────────────────────────────────────────────────────────────────

    let mut stats = WalkStats::default();

    for (i, oid) in backfill_targets.iter().enumerate() {
        // Skip targets completed by a previous (interrupted) run.
        if i < start_index {
            continue;
        }
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
        supersession.set_current_commit(next_newer_version.clone());

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

        // Diff this commit against the neighbour we just processed (the
        // next-newer commit, or HEAD on the first step) and re-derive only the
        // changed files. Artifacts untouched by this commit's diff are NOT
        // copied forward — under honest provenance their existing (newer) rows
        // already carry the right tag, and a backward walk never fabricates a
        // duplicate row for unchanged substrate.
        let changed = pipeline.adapter.changed_sources(
            &corpus.source,
            Some(&next_newer_version),
            &version,
        )?;
        let manifest = ChangeManifest::from_changed(version.clone(), changed);

        let mut commit_opts = opts.clone();
        commit_opts
            .passes
            .retain(|p| *p != crate::types::Pass::History);
        commit_opts.change_manifest = Some(manifest.clone());
        commit_opts.mode = IndexMode::HistoryBackfill;

        // Attach a VirtualHead so that entity-reading passes (e.g. theme pass)
        // see the historical entity state for this commit rather than HEAD.
        // The real `pipeline.db` (not the wrapper) is used here so that reads
        // go to the actual SQLite tables, including `entities_history`.
        let virtual_head =
            VirtualHead::new(Arc::clone(&pipeline.db), corpus.id.clone(), version.clone());
        commit_opts.read_view = Some(Arc::new(ReadView::Virtual(virtual_head)));

        // Run the pipeline.  The wrapper ensures all writes go to *_history;
        // corpus_set_last_indexed_version is a NO-OP on the wrapper.
        let result = commit_pipeline.run(&commit_corpus, commit_opts).await?;

        // Advance the resume cursor to the *next* target (older) commit, or
        // clear it when this was the last one. Written via the real backend
        // (not the wrapper) so it persists across runs. This is the resume-bug
        // fix: the next commit to process is recorded explicitly, never inferred
        // from on-disk history.
        let next_cursor = backfill_targets.get(i + 1).map(|o| format!("git:{o}"));
        pipeline
            .db
            .corpus_set_backfill_cursor(&corpus.id, next_cursor.as_deref())?;

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

/// Build [`RemovedArtifact`] tombstone entries for ids present in `pre` but
/// absent from `post` — the artifacts a commit's diff removed.
fn removed_artifacts<'a>(
    kind: &'static str,
    pre: &'a std::collections::HashSet<String>,
    post: &'a std::collections::HashSet<String>,
) -> impl Iterator<Item = RemovedArtifact> + 'a {
    pre.iter()
        .filter(move |id| !post.contains(*id))
        .map(move |id| RemovedArtifact {
            kind,
            id: id.clone(),
            reason: Some("absent_in_substrate".to_string()),
        })
}

/// Resolve `--back N` to an `Oid` by walking HEAD's first-parent ancestry
/// N steps backward.
///
/// - `--back 1` resolves to HEAD's first parent (HEAD~1).
/// - `--back 2` resolves to HEAD~2, and so on.
/// - If `N` exceeds the available first-parent history, the function clamps to
///   the root commit and logs a single INFO line.
/// - Returns `Err` if `n == 0`.
pub fn resolve_back_n_sha(repo: &Repository, n: u32) -> Result<Oid> {
    if n == 0 {
        anyhow::bail!("--back must be >= 1");
    }

    let head = repo.head()?.peel_to_commit()?;

    // Step 1: move to HEAD's first parent (HEAD~1).
    let mut current = match head.parent(0) {
        Ok(p) => p,
        Err(_) => {
            // HEAD itself is the root commit — clamp here.
            tracing::info!("--back N exceeded available history; clamping to root commit");
            return Ok(head.id());
        }
    };

    // Walk n-1 more steps along first-parent.
    for _ in 1..n {
        match current.parent(0) {
            Ok(p) => current = p,
            Err(_) => {
                tracing::info!("--back N exceeded available history; clamping to root commit");
                return Ok(current.id());
            }
        }
    }

    Ok(current.id())
}

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

    // ── Unit tests for resolve_back_n_sha ─────────────────────────────────────

    #[test]
    fn resolve_back_n_walks_first_parent() {
        // 5 linear commits: oids[0] (root) … oids[4] (HEAD).
        let (_td, repo, oids) = build_linear_repo(&[
            ("a.txt", "a"),
            ("b.txt", "b"),
            ("c.txt", "c"),
            ("d.txt", "d"),
            ("e.txt", "e"),
        ]);
        // --back 3 should return HEAD~3 = oids[1].
        let result = resolve_back_n_sha(&repo, 3).unwrap();
        assert_eq!(result, oids[1], "--back 3 should equal HEAD~3 (oids[1])");
    }

    #[test]
    fn resolve_back_n_clamps_to_root() {
        // 5 linear commits: oids[0] (root) … oids[4] (HEAD).
        let (_td, repo, oids) = build_linear_repo(&[
            ("a.txt", "a"),
            ("b.txt", "b"),
            ("c.txt", "c"),
            ("d.txt", "d"),
            ("e.txt", "e"),
        ]);
        // N larger than available history should clamp to the root commit.
        let result = resolve_back_n_sha(&repo, 100).unwrap();
        assert_eq!(
            result, oids[0],
            "--back 100 should clamp to root commit (oids[0])"
        );
    }

    #[test]
    fn resolve_back_n_zero_is_error() {
        let (_td, repo, _oids) = build_linear_repo(&[("a.txt", "a"), ("b.txt", "b")]);
        let err = resolve_back_n_sha(&repo, 0).unwrap_err().to_string();
        assert!(
            err.contains("--back must be >= 1"),
            "expected '--back must be >= 1' in error, got: {err}"
        );
    }

    // ── Diff-based adapter shared helpers ─────────────────────────────────────

    /// A SourceAdapter that tracks how many times `extract_with_llm` is called
    /// and which file paths are passed to it. Uses an Arc<Mutex<Vec<String>>>
    /// so we can inspect it from outside the async pipeline.
    ///
    /// It overrides `changed_sources` with a real git2 diff so that unchanged
    /// files are not considered dirty. Only `.txt` files are reported.
    struct TrackingAdapter {
        corpus_id: &'static str,
        /// Appended with the file_name each time extract_with_llm is called.
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl TrackingAdapter {
        fn new(corpus_id: &'static str) -> (Self, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    corpus_id,
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    #[async_trait::async_trait]
    impl crate::adapter::SourceAdapter for TrackingAdapter {
        fn kind(&self) -> &str {
            "code"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }

        async fn discover(
            &self,
            source: &str,
        ) -> anyhow::Result<Vec<crate::adapter::DiscoveredSource>> {
            let mut sources = Vec::new();
            if let Ok(rd) = std::fs::read_dir(source) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|e| e.to_str()) == Some("txt") {
                        sources.push(crate::adapter::DiscoveredSource {
                            path: p.to_string_lossy().into_owned(),
                            kind: "text".to_string(),
                            meta: serde_json::Value::Null,
                        });
                    }
                }
            }
            Ok(sources)
        }

        async fn chunk(
            &self,
            source: &crate::adapter::DiscoveredSource,
        ) -> anyhow::Result<Vec<crate::types::Chunk>> {
            let corpus_id = self.corpus_id;
            let rel = std::path::Path::new(&source.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            Ok(vec![crate::types::Chunk::new(
                corpus_id.to_string(),
                None,
                "file".to_string(),
                crate::types::Location::new(corpus_id, &rel),
                std::fs::read_to_string(&source.path).unwrap_or_default(),
            )])
        }

        async fn extract_structure(
            &self,
            _chunk: &crate::types::Chunk,
        ) -> anyhow::Result<crate::adapter::ExtractedStructure> {
            Ok(crate::adapter::ExtractedStructure {
                parent_path: None,
                child_paths: vec![],
                structural_entities: vec![],
                structural_edges: vec![],
            })
        }

        async fn extract_with_llm(
            &self,
            chunk: &crate::types::Chunk,
            _llm: &dyn callimachus_llm::LlmProvider,
        ) -> anyhow::Result<Option<crate::adapter::ExtractedSemantic>> {
            // Record which file triggered the LLM call.
            let file_name = std::path::Path::new(&chunk.location.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| chunk.location.path.clone());
            self.calls.lock().unwrap().push(file_name);
            Ok(Some(crate::adapter::ExtractedSemantic {
                entities: vec![],
                edges: vec![],
                summary_text: None,
            }))
        }

        async fn summarize(
            &self,
            _chunk: &crate::types::Chunk,
            _llm: &dyn callimachus_llm::LlmProvider,
            _depth: &str,
        ) -> anyhow::Result<Option<String>> {
            Ok(None)
        }

        async fn resolve_aliases(
            &self,
            _entities: &[crate::types::Entity],
            _llm: &dyn callimachus_llm::LlmProvider,
        ) -> anyhow::Result<Vec<crate::adapter::EntityMerge>> {
            Ok(vec![])
        }

        fn format_location(&self, chunk: &crate::types::Chunk) -> String {
            chunk.location.path.clone()
        }

        fn parse_location(&self, uri: &str) -> anyhow::Result<crate::adapter::LocationRef> {
            Ok(crate::adapter::LocationRef {
                corpus_id: self.corpus_id.to_string(),
                path: uri.to_string(),
            })
        }

        /// Override with a real git2 diff so only actually-changed .txt files
        /// are returned as dirty. Unchanged files return an empty diff entry
        /// (i.e. they are NOT included in the returned Vec).
        fn changed_sources(
            &self,
            source_path: &str,
            from_version: Option<&str>,
            to_version: &str,
        ) -> anyhow::Result<Vec<crate::indexing::change_manifest::ChangedSource>> {
            use crate::indexing::change_manifest::{ChangeKind, ChangedSource};
            let from = match from_version {
                Some(f) => f,
                None => {
                    return crate::adapter::default_changed_sources(source_path, None, to_version);
                }
            };
            if from == to_version {
                return Ok(vec![]);
            }
            let parse = |v: &str| {
                v.strip_prefix("git:")
                    .and_then(|s| git2::Oid::from_str(s).ok())
            };
            let (Some(fo), Some(to)) = (parse(from), parse(to_version)) else {
                return crate::adapter::default_changed_sources(
                    source_path,
                    Some(from),
                    to_version,
                );
            };
            let repo = git2::Repository::open(source_path)?;
            let ft = repo.find_commit(fo)?.tree()?;
            let tt = repo.find_commit(to)?.tree()?;
            let diff = repo.diff_tree_to_tree(Some(&ft), Some(&tt), None)?;
            let mut changed = Vec::new();
            diff.foreach(
                &mut |delta, _| {
                    let kind = match delta.status() {
                        git2::Delta::Added => ChangeKind::Added,
                        git2::Delta::Deleted => ChangeKind::Deleted,
                        _ => ChangeKind::Modified,
                    };
                    let file = if delta.status() == git2::Delta::Deleted {
                        delta.old_file()
                    } else {
                        delta.new_file()
                    };
                    if let Some(p) = file.path() {
                        let ps = p.to_string_lossy().to_string();
                        if ps.ends_with(".txt") {
                            changed.push(ChangedSource {
                                path: ps,
                                kind,
                                commit_meta: None,
                            });
                        }
                    }
                    true
                },
                None,
                None,
                None,
            )?;
            Ok(changed)
        }
    }

    // ── Test 4: forward walk does diff-based work ─────────────────────────────
    //
    // 3-commit linear repo:
    //   C0: adds a.txt
    //   C1: adds b.txt (a.txt unchanged)
    //   C2: modifies a.txt (b.txt unchanged)
    //
    // Assert: LLM is invoked for b.txt (not a.txt) at C1, and for a.txt (not b.txt) at C2.
    #[tokio::test]
    async fn forward_walk_does_diff_based_work() {
        // Build the 3-commit repo manually so we can control the content changes.
        let td = TempDir::new().expect("temp dir");
        let repo = Repository::init(td.path()).expect("git init");
        let sig = Signature::now("Test", "t@t.com").unwrap();

        // C0: add a.txt
        fs::write(td.path().join("a.txt"), "content-a-v1").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        idx.write().unwrap();
        let tree0_oid = idx.write_tree().unwrap();
        let tree0 = repo.find_tree(tree0_oid).unwrap();
        let c0 = repo
            .commit(Some("HEAD"), &sig, &sig, "C0: add a.txt", &tree0, &[])
            .unwrap();

        // C1: add b.txt (a.txt unchanged)
        fs::write(td.path().join("b.txt"), "content-b").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("b.txt")).unwrap();
        idx.write().unwrap();
        let tree1_oid = idx.write_tree().unwrap();
        let tree1 = repo.find_tree(tree1_oid).unwrap();
        let c0c = repo.find_commit(c0).unwrap();
        let c1 = repo
            .commit(Some("HEAD"), &sig, &sig, "C1: add b.txt", &tree1, &[&c0c])
            .unwrap();

        // C2: modify a.txt (b.txt unchanged)
        fs::write(td.path().join("a.txt"), "content-a-v2").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        idx.write().unwrap();
        let tree2_oid = idx.write_tree().unwrap();
        let tree2 = repo.find_tree(tree2_oid).unwrap();
        let c1c = repo.find_commit(c1).unwrap();
        let _c2 = repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                "C2: modify a.txt",
                &tree2,
                &[&c1c],
            )
            .unwrap();

        let repo_path = td.path().to_string_lossy().into_owned();
        let corpus_id = "diff-walk-test";

        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            corpus_id.to_string(),
            "Diff Walk Test".to_string(),
            "code".to_string(),
            repo_path.clone(),
        );
        db.corpus_insert(&corpus).unwrap();

        let (adapter, calls) = TrackingAdapter::new(corpus_id);
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(adapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        let walk_opts = WalkOptions {
            from_sha: None,
            skip_confirm: true,
        };
        let opts = IndexOptions {
            passes: vec![Pass::Chunk, Pass::Structure, Pass::Semantic],
            ..IndexOptions::default()
        };

        walk_history_forward(&pipeline, &corpus, opts, walk_opts)
            .await
            .expect("walk_history_forward failed");

        let recorded = calls.lock().unwrap().clone();

        // At C1 (index 1), only b.txt should be processed — a.txt is unchanged.
        // At C2 (index 2), only a.txt should be processed — b.txt is unchanged.
        //
        // C0 (first commit) always derives everything, so both we expect a.txt there.
        // We look for the key behavioral property:
        //   - b.txt appears in recorded calls (from C1)
        //   - a.txt appears in recorded calls (from C0 and C2)
        //   - Crucially, a.txt does NOT appear a second time at C1 (no duplicate for unchanged)
        //   - b.txt does NOT appear at C2 (not re-derived when unchanged)
        //
        // Since the calls vec is ordered by commit then by file, we count occurrences.
        let a_count = recorded.iter().filter(|s| s.as_str() == "a.txt").count();
        let b_count = recorded.iter().filter(|s| s.as_str() == "b.txt").count();

        // a.txt should appear exactly TWICE: once at C0 (first derive) + once at C2 (modified).
        // b.txt should appear exactly ONCE: once at C1 (first derive).
        assert_eq!(
            a_count, 2,
            "a.txt should be LLM-processed exactly 2 times (C0+C2), got {a_count}. All calls: {recorded:?}"
        );
        assert_eq!(
            b_count, 1,
            "b.txt should be LLM-processed exactly 1 time (C1 only), got {b_count}. All calls: {recorded:?}"
        );
    }

    // ── Test 6: backward walk does not fabricate history for unchanged files ──
    //
    // Repo: C0 adds a.txt + b.txt; C1 (HEAD) modifies a.txt (b unchanged).
    // Forward-ingest to HEAD, then walk_history_backward from C0.
    //
    // Under honest provenance the backward walk only re-derives files touched by
    // each commit's diff; it never copies unchanged substrate forward. So:
    //   * a.txt (changed C0→C1) IS superseded and has a chunks_history row at C0;
    //   * b.txt (unchanged) is NOT duplicated into history — its single head row
    //     already carries the honest provenance for every commit it spans;
    //   * head tables are untouched.
    #[tokio::test]
    async fn backward_walk_does_not_fabricate_history_for_unchanged() {
        let td = TempDir::new().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        let sig = Signature::now("Test", "t@t.com").unwrap();

        // C0: add a.txt + b.txt
        fs::write(td.path().join("a.txt"), "a-v1").unwrap();
        fs::write(td.path().join("b.txt"), "b-content").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        idx.add_path(Path::new("b.txt")).unwrap();
        idx.write().unwrap();
        let tree0_oid = idx.write_tree().unwrap();
        let tree0 = repo.find_tree(tree0_oid).unwrap();
        let c0 = repo
            .commit(Some("HEAD"), &sig, &sig, "C0: add a+b", &tree0, &[])
            .unwrap();

        // C1: modify a.txt (b.txt unchanged)
        fs::write(td.path().join("a.txt"), "a-v2").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        idx.write().unwrap();
        let tree1_oid = idx.write_tree().unwrap();
        let tree1 = repo.find_tree(tree1_oid).unwrap();
        let c0c = repo.find_commit(c0).unwrap();
        let _c1 = repo
            .commit(Some("HEAD"), &sig, &sig, "C1: modify a", &tree1, &[&c0c])
            .unwrap();

        let repo_path = td.path().to_string_lossy().into_owned();
        let corpus_id = "bwd-head-test";

        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            corpus_id.to_string(),
            "Bwd Head Test".to_string(),
            "code".to_string(),
            repo_path.clone(),
        );
        db.corpus_insert(&corpus).unwrap();

        let (adapter, _calls) = TrackingAdapter::new(corpus_id);
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(adapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        // Forward-walk to HEAD.
        walk_history_forward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: None,
                skip_confirm: true,
            },
        )
        .await
        .expect("forward walk failed");

        let head_chunk_count = db.chunk_count(corpus_id).unwrap();

        // Backward backfill from C0.
        walk_history_backward(
            &pipeline,
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
            WalkOptions {
                from_sha: Some(c0.to_string()),
                skip_confirm: true,
            },
        )
        .await
        .expect("backward walk failed");

        // Head tables must be untouched.
        let head_chunk_count_after = db.chunk_count(corpus_id).unwrap();
        assert_eq!(
            head_chunk_count, head_chunk_count_after,
            "head chunk count should not change after backward walk"
        );

        let v_c0 = format!("git:{c0}");
        let g = db.db_for_test();
        let conn = g.conn();

        // a.txt changed between C0 and C1, so its C0 content is superseded and
        // recorded in chunks_history at introduced_at_version = git:<c0>.
        let a_history: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks_history \
                 WHERE corpus_id=?1 AND location_uri LIKE '%a.txt%' AND introduced_at_version=?2",
                rusqlite::params![corpus_id, v_c0],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            a_history >= 1,
            "a.txt (changed) should have a chunks_history row at {v_c0}, got {a_history}"
        );

        // b.txt was never touched, so honest provenance leaves it as a single
        // head row — it must NOT be fabricated into chunks_history.
        let b_history: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks_history \
                 WHERE corpus_id=?1 AND location_uri LIKE '%b.txt%'",
                rusqlite::params![corpus_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            b_history, 0,
            "b.txt (unchanged) must not be copied into chunks_history, got {b_history}"
        );
    }

    // ── Test 7: rename across a commit (both directions) ─────────────────────
    //
    // Repo: C0 adds a.txt; C1 renames a.txt → b.txt (modelled as remove+add).
    // For BOTH forward and backward walks into separate DBs, assert:
    //   - At C0 SHA: chunks_history has a row for a.txt and NOT for b.txt.
    //   - At C1 SHA: chunks_history (or head) has a row for b.txt and NOT for a.txt.
    #[tokio::test]
    async fn rename_handled_in_both_walk_directions() {
        let td = TempDir::new().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        let sig = Signature::now("Test", "t@t.com").unwrap();

        // C0: add a.txt
        fs::write(td.path().join("a.txt"), "content-a").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new("a.txt")).unwrap();
        idx.write().unwrap();
        let tree0_oid = idx.write_tree().unwrap();
        let tree0 = repo.find_tree(tree0_oid).unwrap();
        let c0 = repo
            .commit(Some("HEAD"), &sig, &sig, "C0: add a.txt", &tree0, &[])
            .unwrap();

        // C1: rename a.txt → b.txt (remove a, add b).
        fs::write(td.path().join("b.txt"), "content-a").unwrap();
        // Also keep a.txt file on disk but remove from index.
        let mut idx = repo.index().unwrap();
        idx.read(true).unwrap();
        idx.remove_path(Path::new("a.txt")).unwrap();
        idx.add_path(Path::new("b.txt")).unwrap();
        idx.write().unwrap();
        let tree1_oid = idx.write_tree().unwrap();
        let tree1 = repo.find_tree(tree1_oid).unwrap();
        let c0c = repo.find_commit(c0).unwrap();
        let c1 = repo
            .commit(Some("HEAD"), &sig, &sig, "C1: rename a→b", &tree1, &[&c0c])
            .unwrap();

        let repo_path = td.path().to_string_lossy().into_owned();

        // Helper: run a forward + backward walk into a fresh DB and return the db.
        async fn run_walks(repo_path: &str, c0: git2::Oid) -> Arc<SqliteBackend> {
            let corpus_id = "rename-test";
            let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
            let corpus = Corpus::new(
                corpus_id.to_string(),
                "Rename Test".to_string(),
                "code".to_string(),
                repo_path.to_string(),
            );
            db.corpus_insert(&corpus).unwrap();

            let (adapter, _calls) = TrackingAdapter::new(corpus_id);
            let pipeline = IndexPipeline {
                db: db.clone(),
                adapter: Arc::new(adapter),
                llm: Arc::new(DryRunProvider::new()),
                embedder: None,
            };

            let opts = IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            };

            // Forward walk (root → HEAD).
            walk_history_forward(
                &pipeline,
                &corpus,
                opts.clone(),
                WalkOptions {
                    from_sha: None,
                    skip_confirm: true,
                },
            )
            .await
            .expect("forward walk failed");

            // Backward backfill from C0.
            walk_history_backward(
                &pipeline,
                &corpus,
                opts,
                WalkOptions {
                    from_sha: Some(c0.to_string()),
                    skip_confirm: true,
                },
            )
            .await
            .expect("backward walk failed");

            db
        }

        let db = run_walks(&repo_path, c0).await;

        let v_c0 = format!("git:{c0}");
        let v_c1 = format!("git:{c1}");
        let corpus_id = "rename-test";

        let g = db.db_for_test();
        let conn = g.conn();

        // At C0: a.txt must exist in history, b.txt must NOT.
        let a_at_c0: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks_history \
                 WHERE corpus_id=?1 AND location_uri LIKE '%a.txt%' AND introduced_at_version=?2",
                rusqlite::params![corpus_id, v_c0],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            a_at_c0 >= 1,
            "a.txt should have a history row at C0 ({v_c0}), got {a_at_c0}"
        );

        let b_at_c0: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks_history \
                 WHERE corpus_id=?1 AND location_uri LIKE '%b.txt%' AND introduced_at_version=?2",
                rusqlite::params![corpus_id, v_c0],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            b_at_c0, 0,
            "b.txt should NOT have a history row at C0 ({v_c0}), got {b_at_c0}"
        );

        // At C1 (HEAD): b.txt must exist (head or history), a.txt must NOT at C1 version.
        let b_at_c1: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE corpus_id=?1 AND location_uri LIKE '%b.txt%'
                 UNION ALL
                 SELECT COUNT(*) FROM chunks_history WHERE corpus_id=?1 AND location_uri LIKE '%b.txt%' AND introduced_at_version=?2",
                rusqlite::params![corpus_id, v_c1],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            b_at_c1 >= 1,
            "b.txt should exist at C1 ({v_c1}), got {b_at_c1}"
        );

        let a_at_c1_head: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE corpus_id=?1 AND location_uri LIKE '%a.txt%'",
                rusqlite::params![corpus_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            a_at_c1_head, 0,
            "a.txt should NOT be in head chunks after rename to b.txt"
        );
    }

    // ── Test 9: CONVERGENCE ────────────────────────────────────────────────────
    //
    // Build ONE richer fixture repo (~6 .txt files, ~8 commits) with ADDs,
    // EDITs, and one RENAME. Run THREE different backfills into THREE fresh DBs:
    //
    //   (a) forward-only:  walk_history_forward root→HEAD.
    //   (b) backward:      walk_history_forward root→HEAD first (to set HEAD
    //                      state + last_indexed_version), then
    //                      walk_history_backward from root.
    //   (c) middle-out:    walk_history_forward from mid-commit to HEAD, then
    //                      walk_history_backward from root to mid-1.
    //
    // The DryRunProvider + TrackingAdapter produce no entities/edges, so
    // convergence is proven over CHUNKS. Compare normalised sets of
    // (id, introduced_at_version, content) from chunks_history across (a),(b),(c).
    //
    // We EXCLUDE: superseded_at_version, superseded_at, history_id, created_at,
    // source_hash (may differ by order of writes).
    //
    // Note: (b) double-writes via forward+backward; the forward pass already
    // populates history; the backward pass copies from head into history for
    // older commits. Because copy is idempotent, the final content must match (a).
    #[tokio::test]
    async fn history_walk_convergence() {
        // ── Build the fixture repo ────────────────────────────────────────────
        //
        // Commits (oldest → newest):
        //  0: add a.txt, b.txt, c.txt
        //  1: edit a.txt
        //  2: add d.txt
        //  3: edit b.txt
        //  4: edit c.txt + add e.txt
        //  5: rename a.txt→f.txt (remove a, add f with same content)
        //  6: edit d.txt
        //  7: edit f.txt (= former a.txt)
        //
        // Mid commit for middle-out = commit 4 (0-indexed).

        let td = TempDir::new().unwrap();
        let repo = Repository::init(td.path()).unwrap();
        let sig = Signature::now("Convergence", "c@c.com").unwrap();
        let mut oids: Vec<Oid> = Vec::new();
        let mut parent: Option<Oid> = None;

        // Helper: commit current index state.
        let commit = |repo: &Repository, sig: &Signature, msg: &str, parent: Option<Oid>| -> Oid {
            let mut idx = repo.index().unwrap();
            idx.write().unwrap();
            let tree_oid = idx.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let parents: Vec<git2::Commit<'_>> = parent
                .iter()
                .map(|&p| repo.find_commit(p).unwrap())
                .collect();
            let prefs: Vec<&git2::Commit<'_>> = parents.iter().collect();
            repo.commit(Some("HEAD"), sig, sig, msg, &tree, &prefs)
                .unwrap()
        };

        // C0: add a, b, c
        fs::write(td.path().join("a.txt"), "alpha v1").unwrap();
        fs::write(td.path().join("b.txt"), "beta v1").unwrap();
        fs::write(td.path().join("c.txt"), "gamma v1").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("a.txt")).unwrap();
            idx.add_path(Path::new("b.txt")).unwrap();
            idx.add_path(Path::new("c.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C0: add a,b,c", parent);
        oids.push(o);
        parent = Some(o);

        // C1: edit a
        fs::write(td.path().join("a.txt"), "alpha v2").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("a.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C1: edit a", parent);
        oids.push(o);
        parent = Some(o);

        // C2: add d
        fs::write(td.path().join("d.txt"), "delta v1").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("d.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C2: add d", parent);
        oids.push(o);
        parent = Some(o);

        // C3: edit b
        fs::write(td.path().join("b.txt"), "beta v2").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("b.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C3: edit b", parent);
        oids.push(o);
        parent = Some(o);

        // C4: edit c + add e  (this is the mid-commit)
        fs::write(td.path().join("c.txt"), "gamma v2").unwrap();
        fs::write(td.path().join("e.txt"), "epsilon v1").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("c.txt")).unwrap();
            idx.add_path(Path::new("e.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C4: edit c + add e", parent);
        oids.push(o);
        parent = Some(o);

        // C5: rename a→f (remove a.txt, add f.txt with same content)
        fs::write(td.path().join("f.txt"), "alpha v2").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.read(true).unwrap();
            idx.remove_path(Path::new("a.txt")).unwrap();
            idx.add_path(Path::new("f.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C5: rename a→f", parent);
        oids.push(o);
        parent = Some(o);

        // C6: edit d
        fs::write(td.path().join("d.txt"), "delta v2").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("d.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C6: edit d", parent);
        oids.push(o);
        parent = Some(o);

        // C7 (HEAD): edit f
        fs::write(td.path().join("f.txt"), "alpha v3").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(Path::new("f.txt")).unwrap();
        }
        let o = commit(&repo, &sig, "C7: edit f", parent);
        oids.push(o);

        assert_eq!(oids.len(), 8, "expected 8 commits");

        let repo_path = td.path().to_string_lossy().into_owned();

        // Mid commit for middle-out: oids[4] (C4).
        // Forward half: C4 → HEAD (oids[4..=7]).
        // Backward half: root → C3 (oids[0..=3] walked backward).
        let mid_sha = oids[4].to_string();

        // ── Helper: create a fresh DB + pipeline with TrackingAdapter ─────────
        let make_pipeline = |corpus_id: &'static str| {
            let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
            let corpus = Corpus::new(
                corpus_id.to_string(),
                "Convergence Test".to_string(),
                "code".to_string(),
                repo_path.clone(),
            );
            db.corpus_insert(&corpus).unwrap();

            let (adapter, _calls) = TrackingAdapter::new(corpus_id);
            let pipeline = IndexPipeline {
                db: db.clone(),
                adapter: Arc::new(adapter),
                llm: Arc::new(DryRunProvider::new()),
                embedder: None,
            };
            (db, corpus, pipeline)
        };

        let base_opts = || IndexOptions {
            passes: vec![Pass::Chunk, Pass::Structure],
            ..IndexOptions::default()
        };

        // ── (a) Forward-only ──────────────────────────────────────────────────
        let (db_a, corpus_a, pipeline_a) = make_pipeline("conv-fwd");
        walk_history_forward(
            &pipeline_a,
            &corpus_a,
            base_opts(),
            WalkOptions {
                from_sha: None,
                skip_confirm: true,
            },
        )
        .await
        .expect("(a) forward walk failed");

        // ── (b) Backward: forward all, then backward from root ────────────────
        //
        // The forward walk already writes history for all commits. Then the
        // backward walk re-copies older commits from head state. The backward
        // pass's copy is idempotent so no extra rows are added — the per-SHA
        // chunk sets must match (a).
        let (db_b, corpus_b, pipeline_b) = make_pipeline("conv-bwd");
        walk_history_forward(
            &pipeline_b,
            &corpus_b,
            base_opts(),
            WalkOptions {
                from_sha: None,
                skip_confirm: true,
            },
        )
        .await
        .expect("(b) forward walk failed");
        walk_history_backward(
            &pipeline_b,
            &corpus_b,
            base_opts(),
            WalkOptions {
                from_sha: Some(oids[0].to_string()),
                skip_confirm: true,
            },
        )
        .await
        .expect("(b) backward walk failed");

        // ── (c) Middle-out ────────────────────────────────────────────────────
        //
        // Forward from C4 to HEAD, then backward from C3 to root.
        // This means commits C0..C3 are populated via backward walk only, and
        // C4..C7 via forward walk only.
        let (db_c, corpus_c, pipeline_c) = make_pipeline("conv-mid");
        walk_history_forward(
            &pipeline_c,
            &corpus_c,
            base_opts(),
            WalkOptions {
                from_sha: Some(mid_sha.clone()),
                skip_confirm: true,
            },
        )
        .await
        .expect("(c) forward walk from mid failed");
        walk_history_backward(
            &pipeline_c,
            &corpus_c,
            base_opts(),
            WalkOptions {
                from_sha: Some(oids[0].to_string()),
                skip_confirm: true,
            },
        )
        .await
        .expect("(c) backward walk to root failed");

        // ── Convergence under honest provenance ───────────────────────────────
        //
        // PR #34's copy-forward model materialised a complete chunk set at every
        // SHA (keyed on a re-stamped `introduced_at_version`), so the original
        // test compared per-SHA reconstructions. Honest provenance deletes that
        // copy-forward: a chunk untouched across many commits is a *single* row
        // stamped at its real introduction point, and the introduction point a
        // walk records depends on where that walk started (forward-from-root
        // sees `alpha v2` introduced at C1; middle-out-from-C4 sees it introduced
        // at C4). So per-SHA reconstruction keyed on `introduced_at_version` no
        // longer converges by construction — the honest per-SHA query is the
        // tagged-union + tombstone read layer landing in PR 4.
        //
        // What MUST converge regardless of walk order is the *lineage*: the full
        // set of distinct (chunk_id, content) every walk discovers across head +
        // history. All three strategies eventually process every commit's diff,
        // so they must agree on the complete content lineage and on the final
        // HEAD state. (The toy DryRunProvider + TrackingAdapter emit no
        // entities/edges, so chunks carry the convergence proof.)

        type ChunkRow = (String, String); // (chunk_id, content)

        let head_chunks =
            |db: &Arc<SqliteBackend>, corpus_id: &str| -> std::collections::BTreeSet<ChunkRow> {
                let g = db.db_for_test();
                let conn = g.conn();
                let mut stmt = conn
                    .prepare("SELECT id, content FROM chunks WHERE corpus_id=?1")
                    .unwrap();
                stmt.query_map(rusqlite::params![corpus_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
            };

        let lineage =
            |db: &Arc<SqliteBackend>, corpus_id: &str| -> std::collections::BTreeSet<ChunkRow> {
                let g = db.db_for_test();
                let conn = g.conn();
                let mut stmt = conn
                    .prepare(
                        "SELECT id, content FROM chunks_history WHERE corpus_id=?1
                     UNION
                     SELECT id, content FROM chunks WHERE corpus_id=?1",
                    )
                    .unwrap();
                stmt.query_map(rusqlite::params![corpus_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
            };

        // 1. All three strategies converge on the same final HEAD state.
        let head_a = head_chunks(&db_a, "conv-fwd");
        let head_b = head_chunks(&db_b, "conv-bwd");
        let head_c = head_chunks(&db_c, "conv-mid");
        assert_eq!(head_a, head_b, "HEAD chunk state differs: (a) vs (b)");
        assert_eq!(head_a, head_c, "HEAD chunk state differs: (a) vs (c)");
        assert!(!head_a.is_empty(), "expected a non-empty HEAD chunk set");

        // 2. All three strategies discover the identical full chunk lineage.
        let lin_a = lineage(&db_a, "conv-fwd");
        let lin_b = lineage(&db_b, "conv-bwd");
        let lin_c = lineage(&db_c, "conv-mid");
        assert_eq!(
            lin_a, lin_b,
            "chunk lineage differs between (a) forward-only and (b) forward+backward"
        );
        assert_eq!(
            lin_a, lin_c,
            "chunk lineage differs between (a) forward-only and (c) middle-out"
        );

        // 3. No walk fabricates a duplicate history row for the same identity at
        //    the same provenance SHA (the middle-out duplicate-row regression).
        for (db, cid) in [
            (&db_a, "conv-fwd"),
            (&db_b, "conv-bwd"),
            (&db_c, "conv-mid"),
        ] {
            let g = db.db_for_test();
            let conn = g.conn();
            let dupes: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM (
                       SELECT id, derived_at_kind, derived_at_sha, COUNT(*) c
                       FROM chunks_history WHERE corpus_id=?1
                       GROUP BY id, derived_at_kind, derived_at_sha
                       HAVING c > 1
                     )",
                    rusqlite::params![cid],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                dupes, 0,
                "{cid}: chunks_history has duplicate (id, kind, sha) rows"
            );
        }
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

    // ── Test: no duplicate history rows after middle-out walk ─────────────────
    //
    // After a middle-out walk (forward from mid → HEAD, then backward to root),
    // every *_history table must have zero groups where the same
    // (corpus_id, id/entity_id, derived_at_kind, derived_at_sha) tuple appears
    // more than once.  The uniqueness indexes enforce this at the DB level, but
    // this test makes the invariant observable as a behavioral assertion.
    #[tokio::test]
    async fn walk_produces_no_duplicate_history_rows() {
        let corpus_id = "no-dup-test";

        // 3 commits on one file: v1, v2, v3.  HEAD = oids[2].
        let (_td, _repo, oids) =
            build_linear_repo(&[("greet.py", "v1"), ("greet.py", "v2"), ("greet.py", "v3")]);
        let repo_path = _td.path().to_string_lossy().into_owned();

        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            corpus_id.to_string(),
            "No Dup Test".to_string(),
            "code".to_string(),
            repo_path.clone(),
        );
        db.corpus_insert(&corpus).unwrap();

        let (adapter, _calls) = TrackingAdapter::new(corpus_id);
        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(adapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

        let opts = || IndexOptions {
            passes: vec![Pass::Chunk, Pass::Structure],
            ..IndexOptions::default()
        };

        // Middle-out: forward from oids[1] (mid) to HEAD, then backward to root.
        walk_history_forward(
            &pipeline,
            &corpus,
            opts(),
            WalkOptions {
                from_sha: Some(oids[1].to_string()),
                skip_confirm: true,
            },
        )
        .await
        .expect("middle-out forward walk failed");

        walk_history_backward(
            &pipeline,
            &corpus,
            opts(),
            WalkOptions {
                from_sha: Some(oids[0].to_string()),
                skip_confirm: true,
            },
        )
        .await
        .expect("middle-out backward walk failed");

        let g = db.db_for_test();
        let conn = g.conn();

        // For each *_history table assert zero duplicate (id, kind, sha) groups.
        // entity_purposes_history and entity_contracts_history key on entity_id
        // rather than id.
        let checks: &[(&str, &str)] = &[
            (
                "chunks_history",
                "SELECT COUNT(*) FROM (
                   SELECT id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM chunks_history WHERE corpus_id=?1
                   GROUP BY id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
            (
                "entities_history",
                "SELECT COUNT(*) FROM (
                   SELECT id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM entities_history WHERE corpus_id=?1
                   GROUP BY id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
            (
                "edges_history",
                "SELECT COUNT(*) FROM (
                   SELECT id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM edges_history WHERE corpus_id=?1
                   GROUP BY id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
            (
                "entity_purposes_history",
                "SELECT COUNT(*) FROM (
                   SELECT entity_id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM entity_purposes_history WHERE corpus_id=?1
                   GROUP BY entity_id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
            (
                "entity_contracts_history",
                "SELECT COUNT(*) FROM (
                   SELECT entity_id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM entity_contracts_history WHERE corpus_id=?1
                   GROUP BY entity_id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
            (
                "entity_blocks_history",
                "SELECT COUNT(*) FROM (
                   SELECT id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM entity_blocks_history WHERE corpus_id=?1
                   GROUP BY id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
            (
                "summaries_history",
                "SELECT COUNT(*) FROM (
                   SELECT id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM summaries_history WHERE corpus_id=?1
                   GROUP BY id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
            (
                "themes_history",
                "SELECT COUNT(*) FROM (
                   SELECT id, derived_at_kind, derived_at_sha, COUNT(*) c
                   FROM themes_history WHERE corpus_id=?1
                   GROUP BY id, derived_at_kind, derived_at_sha HAVING c > 1
                 )",
            ),
        ];

        for (table, sql) in checks {
            let dupes: i64 = conn
                .query_row(sql, rusqlite::params![corpus_id], |r| r.get(0))
                .unwrap_or_else(|e| panic!("query failed on {table}: {e}"));
            assert_eq!(
                dupes, 0,
                "{corpus_id}: {table} has duplicate (id/entity_id, kind, sha) rows after middle-out walk"
            );
        }
    }

    // ── Test: backward backfill resumes past a partial-SHA cursor ─────────────
    //
    // A backward backfill stores a per-corpus cursor after each completed
    // iteration so a subsequent run can skip already-processed targets.
    //
    // This test verifies the cursor-resume contract:
    //   (a) A cursor planted mid-way through the target list is honoured on
    //       the next walk_history_backward call — only the remaining (older)
    //       targets are processed.
    //   (b) The cursor is cleared when the walk completes to root.
    //   (c) The root commit's dirty chunks appear in chunks_history.
    //   (d) HEAD tables are untouched throughout.
    //
    // We simulate "an interrupted prior run" by planting the cursor directly
    // via corpus_set_backfill_cursor — exactly what the real walker writes
    // after each completed iteration.  setup_ingested_corpus (shared fixture
    // helper) provides the HEAD-ingested state.
    #[tokio::test]
    async fn walk_resumes_past_partial_sha() {
        // 5-commit fixture. setup_ingested_corpus does a full forward walk
        // (root→HEAD) and returns the DB + corpus + pipeline already ingested.
        let (db, corpus, pipeline, _td, oids) = setup_ingested_corpus(&[
            ("a.txt", "a1"),
            ("a.txt", "a2"),
            ("a.txt", "a3"),
            ("a.txt", "a4"),
            ("a.txt", "a5"), // HEAD = oids[4]
        ])
        .await;

        let corpus_id = &corpus.id;
        let head_chunk_count = db.chunk_count(corpus_id).unwrap();

        // Step 1: plant a cursor at oids[2] — simulating a prior backfill that
        // completed oids[3] (wrote its history row, advanced cursor) and then died.
        // The cursor value is the NEXT target the walker should process = oids[2].
        //
        // Backfill target order (newest-older first): oids[3], oids[2], oids[1], oids[0].
        // Cursor at oids[2] means start_index=1 — skip oids[3], resume from oids[2].
        let cursor_sha = format!("git:{}", oids[2]);
        db.corpus_set_backfill_cursor(corpus_id, Some(&cursor_sha))
            .unwrap();
        assert_eq!(
            db.corpus_get_backfill_cursor(corpus_id).unwrap(),
            Some(cursor_sha.clone()),
            "pre-condition: cursor must be planted before resume walk"
        );

        // Step 2: resume walk from root — walker reads the cursor, starts at
        // oids[2], processes oids[2], oids[1], oids[0], then clears the cursor.
        let chunk_opts = IndexOptions {
            passes: vec![Pass::Chunk, Pass::Structure],
            ..IndexOptions::default()
        };
        let stats = walk_history_backward(
            &pipeline,
            &corpus,
            chunk_opts,
            WalkOptions {
                from_sha: Some(oids[0].to_string()),
                skip_confirm: true,
            },
        )
        .await
        .expect("step 2: resumed backfill failed");

        // Cursor at oids[2] → start_index=1 in [oids[3], oids[2], oids[1], oids[0]].
        // Processed: oids[2], oids[1], oids[0] = 3 commits.
        assert_eq!(
            stats.commits_processed, 3,
            "step 2: expected 3 commits processed (oids[2]..oids[0]), got {}",
            stats.commits_processed
        );

        // Root commit's dirty chunks must appear in chunks_history.
        // Use a scoped block so the MutexGuard is dropped before calling
        // corpus_get_backfill_cursor (which also acquires the same mutex).
        let root_version = format!("git:{}", oids[0]);
        {
            let g = db.db_for_test();
            let conn = g.conn();
            let root_rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM chunks_history \
                     WHERE corpus_id=?1 AND introduced_at_version=?2",
                    rusqlite::params![corpus_id.as_str(), root_version],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                root_rows >= 1,
                "step 2: expected chunks_history rows at root version {root_version}, got 0"
            );
        } // guard dropped here

        // Cursor must be cleared after a complete run.
        let cursor_after = db.corpus_get_backfill_cursor(corpus_id).unwrap();
        assert_eq!(
            cursor_after, None,
            "step 2: backfill cursor should be cleared after full completion, got {cursor_after:?}"
        );

        // HEAD chunk count must be unchanged throughout.
        let head_chunk_count_after = db.chunk_count(corpus_id).unwrap();
        assert_eq!(
            head_chunk_count, head_chunk_count_after,
            "head chunk count changed after backfill"
        );
    }

    // ── Test: head-mode incremental archives prior themes ─────────────────────
    //
    // Regression for head-mode-theme-archival-missing: after a HEAD-mode
    // incremental at a commit that changes a source file, the prior theme(s)
    // must appear in themes_history with superseded_at_sha = <new version>.
    #[tokio::test]
    async fn head_mode_incremental_archives_prior_themes() {
        use crate::indexing::cascade;
        use crate::indexing::change_manifest::{ChangeKind, ChangeManifest, ChangedSource};
        use crate::types::Theme;

        let corpus_id = "theme-archival-test";

        // 2-commit repo: C0 adds a.txt, C1 modifies it.
        let (_td, _repo, oids) = build_linear_repo(&[("a.txt", "a1"), ("a.txt", "a2")]);
        let repo_path = _td.path().to_string_lossy().into_owned();

        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            corpus_id.to_string(),
            "Theme Archival Test".to_string(),
            "code".to_string(),
            repo_path.clone(),
        );
        db.corpus_insert(&corpus).unwrap();

        let v_c0 = format!("git:{}", oids[0]);
        let v_c1 = format!("git:{}", oids[1]);

        // Simulate "indexed at C0": set last_indexed_version to C0.
        db.corpus_set_last_indexed_version(corpus_id, &v_c0)
            .unwrap();

        // Insert a chunk for a.txt at C0 so the cascade has something to sweep.
        let mut chunk = crate::types::Chunk::new(
            corpus_id.to_string(),
            None,
            "file".to_string(),
            crate::types::Location::new(corpus_id, "a.txt"),
            "a1".to_string(),
        );
        chunk.introduced_at_version = Some(v_c0.clone());
        db.chunk_upsert(&chunk).unwrap();

        // Insert a theme at C0 so there is something to archive.
        let theme = Theme {
            id: "theme-1".to_string(),
            corpus_id: corpus_id.to_string(),
            title: "Test Theme".to_string(),
            statement: "A recurring pattern".to_string(),
            confidence: 0.9,
            model: "dry-run".to_string(),
            model_tier: "unknown".to_string(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            derived_at_version: Some(v_c0.clone()),
        };
        db.theme_upsert(&theme).unwrap();

        // Run a HEAD-mode incremental at C1 by calling cascade::run directly
        // with a manifest that marks a.txt dirty at v_c1.
        let manifest = ChangeManifest::from_changed(
            v_c1.clone(),
            vec![ChangedSource {
                path: "a.txt".to_string(),
                kind: ChangeKind::Modified,
                commit_meta: None,
            }],
        );
        cascade::run(
            Arc::clone(&db) as Arc<dyn crate::storage::StorageBackend>,
            &corpus,
            &manifest,
        )
        .await
        .expect("cascade::run failed");

        // The prior theme must now appear in themes_history with
        // superseded_at_sha = v_c1 (same value as superseded_at_version).
        let g = db.db_for_test();
        let conn = g.conn();
        let archived: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM themes_history \
                 WHERE corpus_id=?1 AND id='theme-1' AND superseded_at_sha=?2",
                rusqlite::params![corpus_id, v_c1],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            archived >= 1,
            "themes_history must contain a row for theme-1 with superseded_at_sha={v_c1}, got {archived}"
        );
    }

    /// A forward walk tombstones an entity that exists at one commit but is gone
    /// at the next. Fixture: `a.txt` holds entity "Alpha" at C1, then is rewritten
    /// to "Beta" at C2 — Alpha's identity disappears and must be tombstoned at C2.
    #[tokio::test]
    async fn forward_walk_tombstones_removed_entity() {
        use crate::adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        };
        use crate::types::{Chunk, Entity, Location};

        // One chunk + one structural entity per .txt file. The entity's identity
        // is its trimmed content, so rewriting the file "renames" the entity.
        struct EntityAdapter;

        #[async_trait::async_trait]
        impl SourceAdapter for EntityAdapter {
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
                let rel = std::path::Path::new(&source.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                Ok(vec![Chunk::new(
                    "tomb-test".to_string(),
                    None,
                    "file".to_string(),
                    Location::new("tomb-test", &rel),
                    std::fs::read_to_string(&source.path).unwrap_or_default(),
                )])
            }
            async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
                let name = chunk.content.trim().to_string();
                let mut entity = Entity::new(
                    format!("ent-{name}"),
                    "tomb-test".to_string(),
                    name,
                    "function".to_string(),
                );
                entity.first_location = Some(chunk.location.clone());
                Ok(ExtractedStructure {
                    parent_path: None,
                    child_paths: vec![],
                    structural_entities: vec![entity],
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
                _e: &[Entity],
                _llm: &dyn callimachus_llm::LlmProvider,
            ) -> anyhow::Result<Vec<EntityMerge>> {
                Ok(vec![])
            }
            fn format_location(&self, chunk: &Chunk) -> String {
                chunk.location.path.clone()
            }
            fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
                Ok(LocationRef {
                    corpus_id: "tomb-test".to_string(),
                    path: uri.to_string(),
                })
            }
        }

        // C1: a.txt = "Alpha"; C2: a.txt rewritten to "Beta".
        let (_td, _repo, oids) = build_linear_repo(&[("a.txt", "Alpha"), ("a.txt", "Beta")]);
        let repo_path = _td.path().to_string_lossy().into_owned();

        let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "tomb-test".to_string(),
            "Tomb Test".to_string(),
            "code".to_string(),
            repo_path,
        );
        db.corpus_insert(&corpus).unwrap();

        let pipeline = IndexPipeline {
            db: db.clone(),
            adapter: Arc::new(EntityAdapter),
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };

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

        // Alpha (present at C1, gone at C2) must be tombstoned at git:<C2>.
        let g = db.db_for_test();
        let died: String = g
            .conn()
            .query_row(
                "SELECT derived_at_sha FROM artifact_tombstones
                 WHERE corpus_id = 'tomb-test'
                   AND artifact_kind = 'entity'
                   AND artifact_id = 'ent-Alpha'",
                [],
                |r| r.get(0),
            )
            .expect("entity ent-Alpha must have a tombstone after it is removed");
        assert_eq!(
            died,
            format!("git:{}", oids[1]),
            "tombstone died_at_sha must be the C2 commit"
        );
    }
}
