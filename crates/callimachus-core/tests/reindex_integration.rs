//! End-to-end integration test for incremental reindex.
//!
//! Verifies:
//!   - After full index, modifying one chapter and reindexing:
//!       * Unchanged chapter chunks keep their IDs.
//!       * The modified chapter chunk gets a new ID.
//!   - Corrections recorded before reindex survive intact.
//!   - `corpus.last_indexed_at` is updated after reindex.

use std::sync::Arc;

use callimachus_core::adapter::{
    DiscoveredSource, EntityMerge, ExtractedSemantic, ExtractedStructure, LocationRef,
    SourceAdapter,
};
use callimachus_core::{
    corrections::types::CorrectionKind,
    indexing::{
        change_detector::ChangeSet,
        pipeline::{IndexOptions, IndexPipeline},
        reindex_pass,
    },
    storage::{SqliteBackend, StorageBackend},
    types::{Chunk, Corpus, Entity, Location},
};
use callimachus_llm::DryRunProvider;

// ---------------------------------------------------------------------------
// Plain-text adapter that splits content on blank lines into chapters.
// ---------------------------------------------------------------------------

struct PlainTextAdapter {
    /// Current file content, keyed by path.
    content: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl PlainTextAdapter {
    fn new() -> Self {
        Self {
            content: Default::default(),
        }
    }

    fn set(&self, path: &str, text: &str) {
        self.content
            .lock()
            .unwrap()
            .insert(path.to_string(), text.to_string());
    }
}

#[async_trait::async_trait]
impl SourceAdapter for PlainTextAdapter {
    fn kind(&self) -> &str {
        "plain"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }

    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>> {
        Ok(vec![DiscoveredSource {
            path: source.to_string(),
            kind: "text".to_string(),
            meta: serde_json::json!({ "corpus_id": "integ" }),
        }])
    }

    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>> {
        let map = self.content.lock().unwrap();
        let text = map.get(&source.path).cloned().unwrap_or_default();
        drop(map);

        // Split on blank lines → chapters.
        let chapters: Vec<&str> = text
            .split("\n\n")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        let chunks = chapters
            .iter()
            .enumerate()
            .map(|(i, content)| {
                Chunk::new(
                    "integ".to_string(),
                    None,
                    "chapter".to_string(),
                    Location::new("integ", format!("ch/{}", i + 1)),
                    content.to_string(),
                )
            })
            .collect();
        Ok(chunks)
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
        _l: &dyn callimachus_llm::LlmProvider,
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
        _l: &dyn callimachus_llm::LlmProvider,
        _d: &str,
    ) -> anyhow::Result<Option<String>> {
        Ok(Some("[summary]".to_string()))
    }

    async fn resolve_aliases(
        &self,
        _e: &[Entity],
        _l: &dyn callimachus_llm::LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>> {
        Ok(vec![])
    }

    fn format_location(&self, c: &Chunk) -> String {
        c.location.path.clone()
    }
    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef> {
        Ok(LocationRef {
            corpus_id: "integ".to_string(),
            path: uri.to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn incremental_reindex_preserves_unchanged_chunks_and_corrections() {
    const SOURCE: &str = "/virtual/corpus.txt";
    const CHAPTER_1: &str = "Chapter one content that stays the same.";
    const CHAPTER_2_V1: &str = "Chapter two: original sentence.";
    const CHAPTER_2_V2: &str = "Chapter two: modified sentence!";

    // ── Setup ────────────────────────────────────────────────────────────────
    let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        "integ".to_string(),
        "Integration Test".to_string(),
        "plain".to_string(),
        SOURCE.to_string(),
    );
    db.corpus_insert(&corpus).unwrap();

    let adapter = Arc::new(PlainTextAdapter::new());
    adapter.set(SOURCE, &format!("{CHAPTER_1}\n\n{CHAPTER_2_V1}"));

    // ── Step 1: Full index ───────────────────────────────────────────────────
    let pipeline = IndexPipeline {
        db: Arc::clone(&db),
        adapter: Arc::clone(&adapter) as Arc<dyn SourceAdapter>,
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };
    pipeline
        .run(&corpus, IndexOptions::default())
        .await
        .unwrap();

    let mut initial_chunks = db.chunk_list("integ").unwrap();
    initial_chunks.sort_by(|a, b| a.location.uri.cmp(&b.location.uri));
    assert_eq!(
        initial_chunks.len(),
        2,
        "should have 2 chapters after full index"
    );

    let ch1_id = initial_chunks[0].id.clone(); // ch/1
    let ch2_id = initial_chunks[1].id.clone(); // ch/2

    // ── Step 2: Record a correction ──────────────────────────────────────────
    db.correction_insert(
        Some("integ"),
        None,
        &CorrectionKind::Rename {
            entity_id: "entity-x".to_string(),
            new_name: "Renamed Entity".to_string(),
        },
    )
    .unwrap();

    // ── Step 3: Modify chapter 2 ─────────────────────────────────────────────
    adapter.set(SOURCE, &format!("{CHAPTER_1}\n\n{CHAPTER_2_V2}"));

    // ── Step 4: change_detector → reindex_pass ───────────────────────────────
    let change_set = ChangeSet {
        changed_paths: vec![SOURCE.to_string()],
        ..Default::default()
    };

    let corpus_refreshed = db.corpus_require("integ").unwrap();

    reindex_pass::run(
        &db,
        &corpus_refreshed,
        &(Arc::clone(&adapter) as Arc<dyn SourceAdapter>),
        &(Arc::new(DryRunProvider::new()) as Arc<dyn callimachus_llm::LlmProvider>),
        &change_set,
        &IndexOptions::default(),
    )
    .await
    .unwrap();

    // ── Step 5: Assertions ───────────────────────────────────────────────────
    let mut after_chunks = db.chunk_list("integ").unwrap();
    after_chunks.sort_by(|a, b| a.location.uri.cmp(&b.location.uri));

    assert_eq!(after_chunks.len(), 2, "still 2 chunks after reindex");

    // Chapter 1 should have the SAME id (content unchanged).
    let new_ch1 = after_chunks
        .iter()
        .find(|c| c.location.path == "ch/1")
        .unwrap();
    assert_eq!(
        new_ch1.id, ch1_id,
        "chapter 1 id should be unchanged (same content)"
    );

    // Chapter 2 should have a NEW id (content changed).
    let new_ch2 = after_chunks
        .iter()
        .find(|c| c.location.path == "ch/2")
        .unwrap();
    assert_ne!(
        new_ch2.id, ch2_id,
        "chapter 2 id should change after content modification"
    );
    assert!(
        new_ch2.content.contains("modified sentence"),
        "chapter 2 content should reflect the new text"
    );

    // Correction should still exist.
    let corrections = db.correction_list("integ").unwrap();
    assert_eq!(corrections.len(), 1, "correction must survive reindex");

    // corpus.last_indexed_at should be set.
    let corpus_final = db.corpus_require("integ").unwrap();
    assert!(
        corpus_final.last_indexed_at.is_some(),
        "last_indexed_at should be updated after reindex"
    );
}
