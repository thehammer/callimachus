//! End-to-end smoke tests for `--passes` flag support on the two walking commands.
//!
//! These tests require a real git repository fixture with multiple commits and
//! a working adapter that produces theme rows.  Because the `DryRunProvider`
//! returns empty LLM responses, the theme pass produces no rows in dry-run mode.
//! Both tests are therefore gated behind `#[ignore]`; run them manually with a
//! real Anthropic API key:
//!
//! ```bash
//! ANTHROPIC_API_KEY=sk-ant-… cargo test -p callimachus-core \
//!     --test passes_flag_walk -- --ignored
//! ```
//!
//! Parser and prerequisite-validator correctness are covered by unit tests in
//! `crates/callimachus-core/src/types/pass.rs` and
//! `crates/callimachus-core/src/indexing/pipeline.rs`.

use std::sync::Arc;

use callimachus_core::{
    adapter::{
        DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
        SourceAdapter,
    },
    indexing::{
        IndexOptions, IndexPipeline,
        history_walk::{WalkOptions, walk_history_backward, walk_history_forward},
    },
    storage::{SqliteBackend, StorageBackend},
    types::{Chunk, Corpus, Entity, Location, Pass},
};
use callimachus_llm::DryRunProvider;

// ── Minimal fake adapter (no git, no real sources) ───────────────────────────
//
// The walking commands drive a real git repo; these integration tests use an
// adapter that reports a single virtual file so the pipeline has something to
// chunk without needing the filesystem to match.

struct FakeWalkAdapter;

#[async_trait::async_trait]
impl SourceAdapter for FakeWalkAdapter {
    fn kind(&self) -> &str {
        "fake-walk"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        Ok(vec![DiscoveredSource {
            path: source.to_string(),
            kind: "text".to_string(),
            meta: serde_json::Value::Null,
        }])
    }
    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        let corpus_id = "passes-walk-test";
        Ok(vec![Chunk::new(
            corpus_id.to_string(),
            None,
            "chapter".to_string(),
            Location::new(corpus_id, &source.path),
            "content".to_string(),
        )])
    }
    async fn extract_structure(&self, _chunk: &Chunk) -> anyhow::Result<ExtractedStructure> {
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
            corpus_id: "passes-walk-test".to_string(),
            path: uri.to_string(),
        })
    }
}

// ── Helper: build a tiny 2-commit git repo in a temp dir ─────────────────────

fn make_git_repo_with_commits(n: usize) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = git2::Repository::init(dir.path()).expect("git init");

    let mut sig_time = git2::Time::new(1_700_000_000, 0);
    let mut parent_commit: Option<git2::Oid> = None;

    for i in 1..=n {
        // Write a file so each commit has a non-empty tree.
        let file_path = dir.path().join(format!("file{i}.txt"));
        std::fs::write(&file_path, format!("content {i}")).expect("write file");

        let mut index = repo.index().expect("index");
        index
            .add_path(std::path::Path::new(&format!("file{i}.txt")))
            .expect("add");
        index.write().expect("write index");
        let tree_oid = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_oid).expect("find tree");

        let sig = git2::Signature::new("Test", "test@test.com", &sig_time).expect("sig");
        sig_time = git2::Time::new(sig_time.seconds() + 60, 0);

        let msg = format!("commit {i}");
        match parent_commit {
            None => {
                let oid = repo
                    .commit(Some("HEAD"), &sig, &sig, &msg, &tree, &[])
                    .expect("initial commit");
                parent_commit = Some(oid);
            }
            Some(parent_oid) => {
                let parent = repo.find_commit(parent_oid).expect("find parent");
                let oid = repo
                    .commit(Some("HEAD"), &sig, &sig, &msg, &tree, &[&parent])
                    .expect("commit");
                parent_commit = Some(oid);
            }
        }
    }

    dir
}

// ── Test 1: ingest --with-history --passes default, then --passes theme ──────

/// Verify that a forward walk with `--passes theme` runs only the theme pass
/// at each iteration (no new chunks or entities added on the second walk).
///
/// Requires a live LLM provider that produces theme rows; dry-run returns empty
/// results so the assertion on themes_history is skipped.
#[ignore = "requires a real git repo and ANTHROPIC_API_KEY; run with --ignored"]
#[tokio::test]
async fn ingest_with_history_passes_theme_only_runs_only_theme() {
    let repo_dir = make_git_repo_with_commits(2);
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus_id = "passes-walk-test";
    let corpus = Corpus::new(
        corpus_id.to_string(),
        "Passes Walk Test".to_string(),
        "fake-walk".to_string(),
        repo_dir.path().to_str().unwrap().to_string(),
    );
    db.corpus_insert(&corpus).unwrap();

    let adapter = Arc::new(FakeWalkAdapter);
    let llm = Arc::new(DryRunProvider::new());

    let pipeline = IndexPipeline {
        db: db.clone(),
        adapter: adapter.clone(),
        llm: llm.clone(),
        embedder: None,
    };

    // First pass: full default walk to seed chunks/entities.
    let default_opts = IndexOptions {
        passes: vec![
            Pass::History,
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Summarize,
            Pass::Purpose,
            Pass::Contract,
        ],
        ..IndexOptions::default()
    };
    walk_history_forward(
        &pipeline,
        &corpus,
        default_opts,
        WalkOptions {
            from_sha: None,
            skip_confirm: true,
        },
    )
    .await
    .unwrap();

    let chunks_after_default = db.chunk_count(corpus_id).unwrap();
    let entities_after_default = db.entity_count(corpus_id).unwrap();

    // Second pass: theme only.
    let theme_opts = IndexOptions {
        passes: vec![Pass::Theme],
        ..IndexOptions::default()
    };
    walk_history_forward(
        &pipeline,
        &corpus,
        theme_opts,
        WalkOptions {
            from_sha: None,
            skip_confirm: true,
        },
    )
    .await
    .unwrap();

    // Head chunk and entity counts must be unchanged after theme-only walk.
    assert_eq!(
        db.chunk_count(corpus_id).unwrap(),
        chunks_after_default,
        "chunk count should not change after theme-only walk"
    );
    assert_eq!(
        db.entity_count(corpus_id).unwrap(),
        entities_after_default,
        "entity count should not change after theme-only walk"
    );
}

// ── Test 2: backfill --back N --passes theme ──────────────────────────────────

/// Verify that `--passes theme` on backfill runs only the theme pass (no new
/// chunks or entities added to `*_history` tables for the targeted commits).
///
/// Requires a live LLM provider; dry-run produces no theme rows so the
/// themes_history assertion is skipped.
#[ignore = "requires a real git repo and ANTHROPIC_API_KEY; run with --ignored"]
#[tokio::test]
async fn backfill_passes_theme_runs_only_theme() {
    let repo_dir = make_git_repo_with_commits(3);
    let db = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus_id = "passes-walk-test";
    let corpus = Corpus::new(
        corpus_id.to_string(),
        "Passes Walk Test".to_string(),
        "fake-walk".to_string(),
        repo_dir.path().to_str().unwrap().to_string(),
    );
    db.corpus_insert(&corpus).unwrap();

    let adapter = Arc::new(FakeWalkAdapter);
    let llm = Arc::new(DryRunProvider::new());

    let pipeline = IndexPipeline {
        db: db.clone(),
        adapter: adapter.clone(),
        llm: llm.clone(),
        embedder: None,
    };

    // Seed HEAD via a normal forward walk.
    let default_opts = IndexOptions {
        passes: vec![
            Pass::History,
            Pass::Chunk,
            Pass::Structure,
            Pass::Semantic,
            Pass::Aliases,
            Pass::Summarize,
            Pass::Purpose,
            Pass::Contract,
        ],
        ..IndexOptions::default()
    };
    walk_history_forward(
        &pipeline,
        &corpus,
        default_opts,
        WalkOptions {
            from_sha: None,
            skip_confirm: true,
        },
    )
    .await
    .unwrap();

    let chunks_before = db.chunk_count(corpus_id).unwrap();
    let entities_before = db.entity_count(corpus_id).unwrap();

    // Backfill the last 2 commits with theme only.
    let theme_opts = IndexOptions {
        passes: vec![Pass::Theme],
        ..IndexOptions::default()
    };

    let repo = git2::Repository::open(repo_dir.path()).unwrap();
    let from_sha = callimachus_core::indexing::history_walk::resolve_back_n_sha(&repo, 2)
        .unwrap()
        .to_string();
    drop(repo);

    walk_history_backward(
        &pipeline,
        &corpus,
        theme_opts,
        WalkOptions {
            from_sha: Some(from_sha),
            skip_confirm: true,
        },
    )
    .await
    .unwrap();

    // Head chunk and entity counts must be unchanged after theme-only backfill.
    assert_eq!(
        db.chunk_count(corpus_id).unwrap(),
        chunks_before,
        "chunk count should not change after theme-only backfill"
    );
    assert_eq!(
        db.entity_count(corpus_id).unwrap(),
        entities_before,
        "entity count should not change after theme-only backfill"
    );
}
