use callimachus_adapter_capture::CaptureAdapter;
use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};
use callimachus_llm::DryRunProvider;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample-capture")
}

fn fixture_source(corpus_id: &str) -> DiscoveredSource {
    let dir = fixture_dir();
    let meta_json_path = dir.join("meta.json");
    let meta_json: serde_json::Value = std::fs::read_to_string(&meta_json_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);

    DiscoveredSource {
        path: dir.join("events.jsonl").to_string_lossy().to_string(),
        kind: "capture".to_string(),
        meta: serde_json::json!({
            "events_path": dir.join("events.jsonl").to_string_lossy(),
            "capture_dir": dir.to_string_lossy(),
            "meta_json": meta_json,
            "corpus_id": corpus_id,
        }),
    }
}

// ── discover ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn discover_directory_returns_one_source() {
    let adapter = CaptureAdapter::new();
    let dir = fixture_dir().to_string_lossy().to_string();
    let sources = adapter.discover(&dir).await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].kind, "capture");
    assert!(sources[0].path.ends_with("events.jsonl"));
}

#[tokio::test]
async fn discover_events_file_directly() {
    let adapter = CaptureAdapter::new();
    let events_path = fixture_dir()
        .join("events.jsonl")
        .to_string_lossy()
        .to_string();
    let sources = adapter.discover(&events_path).await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].kind, "capture");
}

#[tokio::test]
async fn discover_nonexistent_path_errors() {
    let adapter = CaptureAdapter::new();
    let result = adapter.discover("/nonexistent/path/to/capture").await;
    assert!(result.is_err());
}

// ── chunk ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chunk_produces_five_endpoint_chunks() {
    let adapter = CaptureAdapter::new();
    let source = fixture_source("curaspan-test");
    let chunks = adapter.chunk(&source).await.unwrap();

    // 5 distinct curaspan endpoints after filtering telemetry and grouping duplicates.
    assert_eq!(
        chunks.len(),
        5,
        "expected 5 endpoint chunks; got {}: {:?}",
        chunks.len(),
        chunks.iter().map(|c| c.location.path.clone()).collect::<Vec<_>>()
    );
    assert!(chunks.iter().all(|c| c.kind == "endpoint"));
    assert!(chunks.iter().all(|c| c.corpus_id == "curaspan-test"));
}

#[tokio::test]
async fn chunk_location_paths_are_unique() {
    let adapter = CaptureAdapter::new();
    let source = fixture_source("curaspan-test");
    let chunks = adapter.chunk(&source).await.unwrap();

    let mut paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for chunk in &chunks {
        assert!(
            paths.insert(&chunk.location.path),
            "duplicate location path: {}",
            chunk.location.path
        );
    }
}

// ── extract_structure ─────────────────────────────────────────────────────────

#[tokio::test]
async fn extract_structure_one_entity_per_chunk() {
    let adapter = CaptureAdapter::new();
    let source = fixture_source("curaspan-test");
    let chunks = adapter.chunk(&source).await.unwrap();

    for chunk in &chunks {
        let extracted = adapter.extract_structure(chunk).await.unwrap();
        assert_eq!(
            extracted.structural_entities.len(),
            1,
            "each chunk should yield exactly one entity; chunk: {}",
            chunk.location.path
        );
        let entity = &extracted.structural_entities[0];
        assert_eq!(entity.kind, "endpoint");
        assert_eq!(entity.corpus_id, "curaspan-test");
        assert!(entity.id.starts_with("ep:"), "entity id should start with 'ep:': {}", entity.id);
    }
}

#[tokio::test]
async fn extract_structure_produces_precedes_edges() {
    let adapter = CaptureAdapter::new();
    let source = fixture_source("curaspan-test");
    let chunks = adapter.chunk(&source).await.unwrap();

    let total_edges: usize = {
        let mut count = 0;
        for chunk in &chunks {
            let extracted = adapter.extract_structure(chunk).await.unwrap();
            count += extracted.structural_edges.len();
        }
        count
    };

    assert!(
        total_edges >= 1,
        "should produce at least one 'precedes' edge across all chunks; got 0"
    );
}

#[tokio::test]
async fn no_telemetry_entity_in_extracted_structure() {
    let adapter = CaptureAdapter::new();
    let source = fixture_source("curaspan-test");
    let chunks = adapter.chunk(&source).await.unwrap();

    for chunk in &chunks {
        let extracted = adapter.extract_structure(chunk).await.unwrap();
        for entity in &extracted.structural_entities {
            assert!(
                !entity.id.contains("nr-data.net") && !entity.id.contains("bam."),
                "telemetry endpoint should not appear in entities: {}",
                entity.id
            );
        }
    }
}

// ── extract_with_llm (dry run) ────────────────────────────────────────────────

#[tokio::test]
async fn extract_with_llm_returns_entity_with_description() {
    let adapter = CaptureAdapter::new();
    let source = fixture_source("curaspan-test");
    let chunks = adapter.chunk(&source).await.unwrap();
    let llm = DryRunProvider::new();

    let first_chunk = chunks.first().expect("at least one chunk");
    let semantic = adapter.extract_with_llm(first_chunk, &llm).await.unwrap();

    assert!(semantic.is_some(), "extract_with_llm should return Some");
    let semantic = semantic.unwrap();
    assert_eq!(semantic.entities.len(), 1);
    assert!(
        semantic.entities[0].description.is_some(),
        "entity description should be set"
    );
}

// ── full discover → chunk → extract_structure pipeline ───────────────────────

#[tokio::test]
async fn full_pipeline_dry_run_completes() {
    let adapter = CaptureAdapter::new();
    let dir = fixture_dir().to_string_lossy().to_string();

    // discover
    let sources = adapter.discover(&dir).await.unwrap();
    assert!(!sources.is_empty());

    // inject corpus_id as the pipeline would
    let mut source = sources.into_iter().next().unwrap();
    source.meta["corpus_id"] = serde_json::json!("curaspan-test");

    // chunk
    let chunks = adapter.chunk(&source).await.unwrap();
    assert!(!chunks.is_empty());

    // extract_structure for all chunks
    let mut all_entities = Vec::new();
    let mut all_edges = Vec::new();
    for chunk in &chunks {
        let extracted = adapter.extract_structure(chunk).await.unwrap();
        all_entities.extend(extracted.structural_entities);
        all_edges.extend(extracted.structural_edges);
    }

    // Assertions
    assert_eq!(all_entities.len(), chunks.len(), "one entity per chunk");
    assert!(
        all_edges.iter().all(|e| e.kind == "precedes"),
        "all edges should be 'precedes'"
    );

    // No telemetry
    assert!(
        all_entities
            .iter()
            .all(|e| !e.id.contains("nr-data.net")),
        "no telemetry entities"
    );
}
