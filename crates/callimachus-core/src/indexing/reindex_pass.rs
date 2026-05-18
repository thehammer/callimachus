use std::{collections::HashSet, sync::Arc};

use callimachus_llm::LlmProvider;

use crate::{
    adapter::SourceAdapter,
    indexing::{
        change_detector::ChangeSet, pipeline::IndexOptions, semantic_pass, structure_pass,
        summarize_pass,
    },
    storage::StorageBackend,
    types::Corpus,
};

/// Statistics for an incremental reindex run.
#[derive(Debug, Default)]
pub struct ReindexStats {
    pub added: u64,
    pub modified: u64,
    pub deleted: u64,
}

/// Run an incremental reindex over a `ChangeSet`.
///
/// Steps for each changed path:
///   1. Re-discover and re-chunk sources scoped to that path.
///   2. Upsert new content-addressed chunks.
///   3. Find orphaned chunk IDs (old IDs not present in new set) and delete them.
///
/// Then:
///   4. Delete explicitly deleted chunk IDs.
///   5. Re-run structure, semantic, and summarize passes (all idempotent).
///   6. Run alias resolution.
///   7. Update `corpus.last_indexed_at`.
pub async fn run(
    db: &Arc<dyn StorageBackend>,
    corpus: &Corpus,
    adapter: &Arc<dyn SourceAdapter>,
    llm: &Arc<dyn LlmProvider>,
    change_set: &ChangeSet,
    opts: &IndexOptions,
) -> anyhow::Result<ReindexStats> {
    let mut stats = ReindexStats::default();

    // ── 1. Re-chunk changed paths and compute orphans ─────────────────────────
    if !change_set.changed_paths.is_empty() {
        // Snapshot current chunk IDs for the corpus.
        let old_ids: HashSet<String> = db.chunk_list_ids(&corpus.id)?.into_iter().collect();

        // Collect new chunk IDs by re-chunking every changed path.
        let mut new_ids: HashSet<String> = HashSet::new();

        for path in &change_set.changed_paths {
            // Discover sources at this path.
            let sources = adapter.discover(path).await?;

            for source in &sources {
                let chunks = adapter.chunk(source).await?;
                for chunk in chunks {
                    new_ids.insert(chunk.id.clone());
                    if opts.dry_run {
                        continue;
                    }
                    // Upsert is idempotent: INSERT OR IGNORE skips existing same-content chunks.
                    if !db.chunk_has(&chunk.id)? {
                        db.chunk_upsert(&chunk)?;
                        stats.added += 1;
                    }
                }
            }
        }

        // Orphans = chunks that existed before but are no longer produced.
        let orphan_ids: Vec<String> = old_ids.difference(&new_ids).cloned().collect();

        for orphan_id in &orphan_ids {
            if opts.dry_run {
                stats.deleted += 1;
                continue;
            }
            db.summary_delete_for_target(&corpus.id, orphan_id)?;
            db.chunk_delete_by_id(orphan_id)?;
            stats.deleted += 1;
        }
    }

    // ── 2. Delete explicitly deleted chunks ───────────────────────────────────
    for chunk_id in &change_set.deleted_chunk_ids {
        if opts.dry_run {
            stats.deleted += 1;
            continue;
        }
        db.summary_delete_for_target(&corpus.id, chunk_id)?;
        db.chunk_delete_by_id(chunk_id)?;
        stats.deleted += 1;
    }

    if opts.dry_run {
        return Ok(stats);
    }

    // ── 3. Re-run downstream passes (all idempotent) ──────────────────────────
    structure_pass::run(Arc::clone(db), corpus, Arc::clone(adapter), opts).await?;
    semantic_pass::run(
        Arc::clone(db),
        corpus,
        Arc::clone(adapter),
        Arc::clone(llm),
        opts,
    )
    .await?;
    summarize_pass::run(
        Arc::clone(db),
        corpus,
        Arc::clone(adapter),
        Arc::clone(llm),
        opts,
    )
    .await?;

    // ── 4. Alias resolution ───────────────────────────────────────────────────
    let all_entities = db.entity_list(&corpus.id)?;
    if !all_entities.is_empty() {
        match adapter.resolve_aliases(&all_entities, llm.as_ref()).await {
            Ok(merges) => {
                for merge in merges {
                    if let Err(e) = db.entity_merge(&merge.keep_id, &merge.absorb_id) {
                        tracing::warn!("entity merge failed during reindex: {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("alias resolution failed during reindex: {e}"),
        }
    }

    // ── 5. Update corpus last_indexed_at ─────────────────────────────────────
    let now = chrono::Utc::now().to_rfc3339();
    db.corpus_set_last_indexed(&corpus.id, &now)?;

    Ok(stats)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use callimachus_llm::{DryRunProvider, LlmProvider};

    use crate::{
        adapter::{
            DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
            SourceAdapter,
        },
        corrections::types::CorrectionKind,
        indexing::{IndexPipeline, change_detector::ChangeSet, pipeline::IndexOptions},
        storage::{SqliteBackend, StorageBackend},
        types::{Chunk, Corpus, Entity, Location},
    };

    use super::run;

    // ── Minimal fake adapter (mirrors pipeline tests) ────────────────────────

    struct FakeAdapter {
        /// Content returned per call (rotates through the vec by path).
        content: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    impl FakeAdapter {
        fn new() -> Self {
            Self {
                content: std::sync::Mutex::new(Default::default()),
            }
        }
        fn set_content(&self, path: &str, content: &str) {
            self.content
                .lock()
                .unwrap()
                .insert(path.to_string(), content.to_string());
        }
    }

    #[async_trait::async_trait]
    impl SourceAdapter for FakeAdapter {
        fn kind(&self) -> &str {
            "fake"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }

        async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
            Ok(vec![DiscoveredSource {
                path: source.to_string(),
                kind: "text".to_string(),
                meta: serde_json::json!({ "corpus_id": "test-corpus" }),
            }])
        }

        async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
            let corpus_id = "test-corpus";
            let content = {
                let map = self.content.lock().unwrap();
                map.get(&source.path)
                    .cloned()
                    .unwrap_or_else(|| "default content".to_string())
            };
            Ok(vec![Chunk::new(
                corpus_id.to_string(),
                None,
                "chapter".to_string(),
                Location::new(corpus_id, "ch/1"),
                format!("{content} | path={}", source.path),
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
            Ok(Some("[test summary]".to_string()))
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
                corpus_id: "test-corpus".to_string(),
                path: uri.to_string(),
            })
        }
    }

    fn setup() -> (Arc<dyn StorageBackend>, Corpus, Arc<FakeAdapter>) {
        let db = SqliteBackend::open_in_memory().unwrap();
        let corpus = Corpus::new(
            "test-corpus".to_string(),
            "Test".to_string(),
            "fake".to_string(),
            "/tmp/test-source".to_string(),
        );
        db.corpus_insert(&corpus).unwrap();
        (Arc::new(db), corpus, Arc::new(FakeAdapter::new()))
    }

    async fn full_index(db: Arc<dyn StorageBackend>, corpus: &Corpus, adapter: Arc<FakeAdapter>) {
        let pipeline = IndexPipeline {
            db: Arc::clone(&db),
            adapter: adapter as Arc<dyn SourceAdapter>,
            llm: Arc::new(DryRunProvider::new()),
            embedder: None,
        };
        pipeline.run(corpus, IndexOptions::default()).await.unwrap();
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn unchanged_corpus_produces_zero_deletions() {
        let (db, corpus, adapter) = setup();
        adapter.set_content("/tmp/test-source", "chapter one content");
        full_index(Arc::clone(&db), &corpus, Arc::clone(&adapter)).await;

        let count_before = db.chunk_count(&corpus.id).unwrap();

        // Re-index with the same content → chunk IDs unchanged → no orphans.
        let cs = ChangeSet {
            changed_paths: vec!["/tmp/test-source".to_string()],
            ..Default::default()
        };
        let stats = run(
            &db,
            &corpus,
            &(Arc::clone(&adapter) as Arc<dyn SourceAdapter>),
            &(Arc::new(DryRunProvider::new()) as Arc<dyn LlmProvider>),
            &cs,
            &IndexOptions::default(),
        )
        .await
        .unwrap();

        let count_after = db.chunk_count(&corpus.id).unwrap();

        assert_eq!(count_before, count_after, "chunk count should be unchanged");
        assert_eq!(stats.deleted, 0);
    }

    #[tokio::test]
    async fn explicit_delete_removes_chunk() {
        let (db, corpus, adapter) = setup();
        adapter.set_content("/tmp/test-source", "chapter one content");
        full_index(Arc::clone(&db), &corpus, Arc::clone(&adapter)).await;

        let chunk_ids = db.chunk_list_ids(&corpus.id).unwrap();
        assert_eq!(chunk_ids.len(), 1);
        let victim = chunk_ids[0].clone();

        let cs = ChangeSet {
            deleted_chunk_ids: vec![victim],
            ..Default::default()
        };
        run(
            &db,
            &corpus,
            &(Arc::clone(&adapter) as Arc<dyn SourceAdapter>),
            &(Arc::new(DryRunProvider::new()) as Arc<dyn LlmProvider>),
            &cs,
            &IndexOptions::default(),
        )
        .await
        .unwrap();

        let count = db.chunk_count(&corpus.id).unwrap();
        assert_eq!(count, 0, "deleted chunk should be gone");
    }

    #[tokio::test]
    async fn idempotent_reindex_adds_zero_chunks() {
        let (db, corpus, adapter) = setup();
        adapter.set_content("/tmp/test-source", "stable content");
        full_index(Arc::clone(&db), &corpus, Arc::clone(&adapter)).await;

        let before = db.chunk_count(&corpus.id).unwrap();

        // Run twice.
        for _ in 0..2 {
            let cs = ChangeSet {
                changed_paths: vec!["/tmp/test-source".to_string()],
                ..Default::default()
            };
            run(
                &db,
                &corpus,
                &(Arc::clone(&adapter) as Arc<dyn SourceAdapter>),
                &(Arc::new(DryRunProvider::new()) as Arc<dyn LlmProvider>),
                &cs,
                &IndexOptions::default(),
            )
            .await
            .unwrap();
        }

        let after = db.chunk_count(&corpus.id).unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn corrections_survive_reindex() {
        let (db, corpus, adapter) = setup();
        adapter.set_content("/tmp/test-source", "some content");
        full_index(Arc::clone(&db), &corpus, Arc::clone(&adapter)).await;

        // Record a correction via the store directly (backend wraps the same DB).
        // We need to downcast to SqliteBackend to access the raw store; instead use the trait.
        db.correction_insert(
            Some(&corpus.id),
            None,
            &CorrectionKind::Rename {
                entity_id: "e1".to_string(),
                new_name: "New Name".to_string(),
            },
        )
        .unwrap();

        // Reindex.
        let cs = ChangeSet {
            changed_paths: vec!["/tmp/test-source".to_string()],
            ..Default::default()
        };
        run(
            &db,
            &corpus,
            &(Arc::clone(&adapter) as Arc<dyn SourceAdapter>),
            &(Arc::new(DryRunProvider::new()) as Arc<dyn LlmProvider>),
            &cs,
            &IndexOptions::default(),
        )
        .await
        .unwrap();

        // Correction should still be in the DB.
        let corrections = db.correction_list(&corpus.id).unwrap();
        assert_eq!(corrections.len(), 1, "correction should survive reindex");
    }
}
