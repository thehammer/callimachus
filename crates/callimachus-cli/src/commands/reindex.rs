use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use callimachus_core::{
    indexing::{
        ChangeStrategy, change_detector, change_manifest::ChangeManifest, pipeline::IndexOptions,
        reindex_pass,
    },
    storage::StorageBackend,
};
use callimachus_llm::{build_embedding_provider, build_provider};

use crate::{
    commands::index::{
        build_embedding_provider_config, default_registry, resolve_adapter, resolve_provider,
    },
    config::GlobalConfig,
};

#[allow(clippy::too_many_arguments)]
pub async fn run(
    corpus_id: &str,
    since: Option<String>,
    dry_run: bool,
    provider_override: Option<String>,
    stable_sampling: bool,
    source_override: Option<String>,
    db: Arc<dyn StorageBackend>,
    config: &GlobalConfig,
) -> Result<()> {
    let corpus = db
        .corpus_require(corpus_id)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .map_err(|e| e.context(format!("corpus '{corpus_id}' not found")))?;

    // Apply --source override in-memory only.  The stored corpus record is never mutated.
    let corpus = if let Some(ref override_path) = source_override {
        if !std::path::Path::new(override_path).exists() {
            bail!(
                "--source path does not exist: {override_path}\n\
                 Refusing to proceed — an absent source path would cause all \
                 existing chunks to be treated as orphaned and deleted."
            );
        }
        eprintln!(
            "note: using --source override '{}' (stored path: '{}')",
            override_path, corpus.source
        );
        callimachus_adapter_contract::types::Corpus {
            source: override_path.clone(),
            ..corpus
        }
    } else {
        corpus
    };

    let provider_config = resolve_provider(provider_override, config)?;
    let llm = build_provider(provider_config)
        .map_err(|e| anyhow::anyhow!("failed to build LLM provider: {e}"))?;

    let registry = default_registry();
    let adapter = resolve_adapter(&corpus, &registry)?;

    // Detect changes.
    let change_set = change_detector::detect(&corpus, db.as_ref(), since.as_deref())
        .context("change detection failed")?;

    // Summarise strategy for the user.
    let strategy_label = match &change_set.strategy {
        ChangeStrategy::Mtime { since: ts } => format!("mtime since {ts}"),
        ChangeStrategy::Git { since_ref } => format!("git diff since {since_ref}"),
        ChangeStrategy::Full => {
            eprintln!(
                "warning: no change baseline found; running full reindex. \
                 Use `calli index` for initial indexing."
            );
            "full".to_string()
        }
    };

    eprintln!(
        "Reindexing corpus '{}' — strategy: {strategy_label}",
        corpus.id
    );
    eprintln!(
        "  Detected {} changed path(s), {} pre-deleted chunk(s).",
        change_set.changed_paths.len(),
        change_set.deleted_chunk_ids.len()
    );

    if dry_run {
        println!("Dry run — no changes written.");
        for p in &change_set.changed_paths {
            println!("  changed: {p}");
        }
        for id in &change_set.deleted_chunk_ids {
            println!("  delete chunk: {id}");
        }
        return Ok(());
    }

    // Build embedder if embeddings are enabled. Fail loudly if enabled-but-broken.
    let embedder = {
        let embedding_enabled = config.embedding.as_ref().is_some_and(|e| e.enabled);
        if embedding_enabled {
            let embed_cfg = build_embedding_provider_config(config);
            match build_embedding_provider(embed_cfg) {
                Ok(Some(p)) => Some(p),
                Ok(None) => None,
                Err(e) => {
                    bail!("embeddings enabled in config but not usable: {e}");
                }
            }
        } else {
            None
        }
    };

    // Resolve the corpus's current HEAD as a `git:<oid>` version so the Layer-2
    // passes (purpose/contract/theme) stamp `derived_at_version` and the version
    // anchor advances. Without this, reindex wrote NULL-provenance artifacts and
    // never bumped `last_indexed_version`. `all_dirty` keeps every entity a
    // candidate; the per-entity cache still skips unchanged ones, so only changed
    // entities re-derive — just now with a version stamp. Non-git corpora (rev-parse
    // fails) get no manifest, preserving prior behaviour.
    let change_manifest = std::process::Command::new("git")
        .args(["-C", &corpus.source, "rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| ChangeManifest::all_dirty(format!("git:{}", s.trim())));

    let start = Instant::now();
    let llm_arc = Arc::new(llm);

    let stats = reindex_pass::run(
        &db,
        &corpus,
        &(adapter as Arc<dyn callimachus_core::SourceAdapter>),
        &(llm_arc as Arc<dyn callimachus_llm::LlmProvider>),
        embedder,
        &change_set,
        &IndexOptions {
            dry_run: false,
            stable_sampling,
            change_manifest,
            ..Default::default()
        },
    )
    .await?;

    let elapsed = start.elapsed();
    println!("Done ({:.1}s).", elapsed.as_secs_f32());
    println!(
        "  +{} added  ~{} modified  -{} deleted chunks",
        stats.added, stats.modified, stats.deleted
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use callimachus_core::storage::{SqliteBackend, StorageBackend};
    use callimachus_core::types::Corpus;

    use crate::config::GlobalConfig;

    /// Regression: `reindex` previously hardcoded the book adapter and bailed
    /// with "supports 'book' only" for any other corpus kind. It must now select
    /// the code adapter via the registry (`resolve_adapter`) and complete a
    /// dry-run without error.
    #[tokio::test]
    async fn code_corpus_reindex_selects_code_adapter() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "code-test".to_string(),
            "Code Test".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        super::run(
            "code-test",
            None,                        // since
            true,                        // dry_run
            Some("dry-run".to_string()), // provider_override (no API key needed)
            false,                       // stable_sampling
            None,                        // source_override
            db,
            &GlobalConfig::default(),
        )
        .await
        .unwrap();
    }

    /// A dry-run with `--source` pointing at an existing temp dir must succeed
    /// and use the override path (not the stored source).
    #[tokio::test]
    async fn source_override_valid_path_dry_run_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let override_path = dir.path().to_str().unwrap().to_string();

        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        // Seed the corpus with a *different* (non-existent) stored source to
        // prove the override replaces it.
        let corpus = Corpus::new(
            "src-override-test".to_string(),
            "Source Override Test".to_string(),
            "code".to_string(),
            "/nonexistent/stored/path".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        super::run(
            "src-override-test",
            None,                           // since
            true,                           // dry_run
            Some("dry-run".to_string()),    // provider_override
            false,                          // stable_sampling
            Some(override_path.clone()),    // source_override — valid temp dir
            db,
            &GlobalConfig::default(),
        )
        .await
        .expect("dry-run with valid --source override should succeed");
    }

    /// A `--source` pointing at a non-existent path must fail loudly and must
    /// NOT proceed to change detection (which would wipe all chunks).
    #[tokio::test]
    async fn source_override_nonexistent_path_fails_loudly() {
        let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
        let corpus = Corpus::new(
            "src-bad-override-test".to_string(),
            "Bad Source Override Test".to_string(),
            "code".to_string(),
            env!("CARGO_MANIFEST_DIR").to_string(),
        );
        db.corpus_insert(&corpus).unwrap();

        let err = super::run(
            "src-bad-override-test",
            None,                                         // since
            true,                                         // dry_run
            Some("dry-run".to_string()),                  // provider_override
            false,                                        // stable_sampling
            Some("/nonexistent/path/12345".to_string()),  // source_override — bad path
            db,
            &GlobalConfig::default(),
        )
        .await
        .expect_err("bad --source override must return an error");

        let msg = err.to_string();
        assert!(
            msg.contains("does not exist") || msg.contains("/nonexistent/path/12345"),
            "error message should mention the bad path; got: {msg}"
        );
    }
}
