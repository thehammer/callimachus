use callimachus_adapter_book::BookAdapter;
use callimachus_core::{adapter::SourceAdapter, types::Entity};
use callimachus_llm::DryRunProvider;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/tale_excerpt.txt"
);

const CORPUS_ID: &str = "tale";

fn make_source_meta() -> serde_json::Value {
    serde_json::json!({ "corpus_id": CORPUS_ID })
}

#[tokio::test]
async fn discover_returns_one_text_source() {
    let adapter = BookAdapter::new();
    let sources = adapter.discover(FIXTURE).await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].kind, "text");
    assert_eq!(sources[0].path, FIXTURE);
}

#[tokio::test]
async fn chunk_text_returns_chunks_with_valid_locations() {
    let adapter = BookAdapter::new();

    let mut sources = adapter.discover(FIXTURE).await.unwrap();
    // Inject corpus_id so chunk IDs are scoped correctly.
    sources[0].meta = make_source_meta();

    let chunks = adapter.chunk(&sources[0]).await.unwrap();

    assert!(!chunks.is_empty(), "expected at least one chunk");

    for chunk in &chunks {
        // Every chunk must have a non-empty location URI.
        assert!(
            chunk.location.uri.starts_with("calli://"),
            "bad location URI: {}",
            chunk.location.uri
        );
        // Every chunk must have non-empty content.
        assert!(!chunk.content.trim().is_empty(), "chunk has empty content");
    }
}

#[tokio::test]
async fn chunk_text_produces_chapters_and_scenes() {
    let adapter = BookAdapter::new();
    let mut sources = adapter.discover(FIXTURE).await.unwrap();
    sources[0].meta = make_source_meta();

    let chunks = adapter.chunk(&sources[0]).await.unwrap();

    let chapters: Vec<_> = chunks.iter().filter(|c| c.kind == "chapter").collect();
    let scenes: Vec<_> = chunks.iter().filter(|c| c.kind == "scene").collect();

    assert!(!chapters.is_empty(), "expected chapter chunks");
    assert!(!scenes.is_empty(), "expected scene chunks");

    // Scenes should have parent_path set.
    for scene in &scenes {
        assert!(
            scene.parent_path.is_some(),
            "scene {} should have parent_path",
            scene.id
        );
    }
}

#[tokio::test]
async fn extract_structure_returns_parent_path_for_scenes() {
    let adapter = BookAdapter::new();
    let mut sources = adapter.discover(FIXTURE).await.unwrap();
    sources[0].meta = make_source_meta();

    let chunks = adapter.chunk(&sources[0]).await.unwrap();
    let scene = chunks.iter().find(|c| c.kind == "scene").unwrap();

    let structure = adapter.extract_structure(scene).await.unwrap();
    // For book scenes, extract_structure mirrors the parent already set in the chunk.
    assert_eq!(structure.parent_path, scene.parent_path);
}

#[tokio::test]
async fn extract_with_llm_dry_run_returns_valid_semantic() {
    let adapter = BookAdapter::new();
    let llm = DryRunProvider::new();

    let mut sources = adapter.discover(FIXTURE).await.unwrap();
    sources[0].meta = make_source_meta();

    let chunks = adapter.chunk(&sources[0]).await.unwrap();
    let scene = chunks.iter().find(|c| c.kind == "scene").unwrap();

    let result = adapter.extract_with_llm(scene, &llm).await.unwrap();

    // Scene chunks should return Some.
    assert!(result.is_some(), "expected Some for scene chunk");
    let sem = result.unwrap();
    // entities and edges are valid (may be empty with DryRunProvider).
    let _ = sem.entities;
    let _ = sem.edges;
}

#[tokio::test]
async fn extract_with_llm_skips_chapter_chunks() {
    let adapter = BookAdapter::new();
    let llm = DryRunProvider::new();

    let mut sources = adapter.discover(FIXTURE).await.unwrap();
    sources[0].meta = make_source_meta();

    let chunks = adapter.chunk(&sources[0]).await.unwrap();
    let chapter = chunks.iter().find(|c| c.kind == "chapter").unwrap();

    let result = adapter.extract_with_llm(chapter, &llm).await.unwrap();
    assert!(
        result.is_none(),
        "chapter chunks should be skipped by extract_with_llm"
    );
}

#[tokio::test]
async fn summarize_dry_run_returns_non_empty_string() {
    let adapter = BookAdapter::new();
    let llm = DryRunProvider::new();

    let mut sources = adapter.discover(FIXTURE).await.unwrap();
    sources[0].meta = make_source_meta();

    let chunks = adapter.chunk(&sources[0]).await.unwrap();
    let scene = chunks.iter().find(|c| c.kind == "scene").unwrap();

    let summary = adapter.summarize(scene, &llm, "scene").await.unwrap();
    assert!(summary.is_some(), "expected a summary");
    assert!(!summary.unwrap().is_empty());
}

#[tokio::test]
async fn resolve_aliases_dry_run_returns_without_error() {
    let adapter = BookAdapter::new();
    let llm = DryRunProvider::new();

    let entities: Vec<Entity> = vec![
        Entity {
            id: "tale:carton".to_string(),
            corpus_id: CORPUS_ID.to_string(),
            canonical_name: "Sydney Carton".to_string(),
            kind: "character".to_string(),
            abstract_kind: String::new(),
            aliases: vec!["Carton".to_string()],
            description: Some("A lawyer".to_string()),
            first_location: None,
            last_location: None,
            appearance_count: 5,
            confidence: 0.9,
        },
        Entity {
            id: "tale:darnay".to_string(),
            corpus_id: CORPUS_ID.to_string(),
            canonical_name: "Charles Darnay".to_string(),
            kind: "character".to_string(),
            abstract_kind: String::new(),
            aliases: vec!["Darnay".to_string()],
            description: Some("A French aristocrat".to_string()),
            first_location: None,
            last_location: None,
            appearance_count: 3,
            confidence: 0.9,
        },
    ];

    let merges = adapter.resolve_aliases(&entities, &llm).await.unwrap();
    // DryRunProvider returns "[dry-run]" which is treated as no merges.
    let _ = merges;
}

#[tokio::test]
async fn format_and_parse_location() {
    let adapter = BookAdapter::new();
    let mut sources = adapter.discover(FIXTURE).await.unwrap();
    sources[0].meta = make_source_meta();

    let chunks = adapter.chunk(&sources[0]).await.unwrap();
    let chunk = &chunks[0];

    let formatted = adapter.format_location(chunk);
    assert!(!formatted.is_empty());

    // parse_location should round-trip through the full URI.
    let loc_ref = adapter.parse_location(&chunk.location.uri).unwrap();
    assert!(!loc_ref.path.is_empty());
}

#[tokio::test]
async fn full_pipeline_dry_run() {
    use callimachus_core::{
        indexing::{IndexOptions, IndexPipeline},
        storage::{SqliteBackend, StorageBackend},
        types::Corpus,
    };
    use std::sync::Arc;

    let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        CORPUS_ID.to_string(),
        "A Tale of Two Cities (excerpt)".to_string(),
        "book".to_string(),
        FIXTURE.to_string(),
    );
    db.corpus_insert(&corpus).unwrap();

    let pipeline = IndexPipeline {
        db,
        adapter: Arc::new(BookAdapter::new()),
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };

    // Dry-run should complete without error.
    let opts = IndexOptions {
        dry_run: true,
        ..Default::default()
    };
    let result = pipeline.run(&corpus, opts).await.unwrap();

    // In dry-run mode, nothing is written.
    assert_eq!(result.total_chunks, 0);
    assert_eq!(result.total_entities, 0);
}

#[tokio::test]
async fn full_pipeline_writes_chunks() {
    use callimachus_core::{
        indexing::{IndexOptions, IndexPipeline},
        storage::{SqliteBackend, StorageBackend},
        types::{Corpus, Pass},
    };
    use std::sync::Arc;

    let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        CORPUS_ID.to_string(),
        "A Tale of Two Cities (excerpt)".to_string(),
        "book".to_string(),
        FIXTURE.to_string(),
    );
    db.corpus_insert(&corpus).unwrap();

    let pipeline = IndexPipeline {
        db,
        adapter: Arc::new(BookAdapter::new()),
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };

    // Run only the chunk pass so we can assert chunk count.
    let opts = IndexOptions {
        passes: vec![Pass::Chunk],
        dry_run: false,
        ..Default::default()
    };
    let result = pipeline.run(&corpus, opts).await.unwrap();
    assert!(result.total_chunks > 0, "expected chunks after indexing");
}
