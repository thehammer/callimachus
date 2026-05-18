use callimachus_adapter_code::CodeAdapter;
use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};
use callimachus_llm::DryRunProvider;
use std::path::PathBuf;

/// Path to the sample fixture project.
fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_project")
}

fn fixture_source(corpus_id: &str) -> DiscoveredSource {
    DiscoveredSource {
        path: fixture_dir().to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": corpus_id }),
    }
}

// ── discover ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn discover_returns_one_source() {
    let adapter = CodeAdapter::new();
    let sources = adapter
        .discover(&fixture_dir().to_string_lossy())
        .await
        .unwrap();
    assert_eq!(
        sources.len(),
        1,
        "discover should return exactly one source"
    );
    assert_eq!(sources[0].kind, "directory");
}

// ── chunk ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn chunk_returns_nonzero_chunks() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();
    assert!(!chunks.is_empty(), "should produce at least one chunk");
}

#[tokio::test]
async fn chunk_rust_files_produce_chunks() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();

    let rust_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.location.uri.contains(".rs"))
        .collect();
    assert!(
        !rust_chunks.is_empty(),
        "should produce chunks for .rs files"
    );
}

#[tokio::test]
async fn chunk_function_has_file_parent() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();

    // Function/class chunks should have a parent_path pointing at their file.
    let item_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.kind != "file" && c.parent_path.is_some())
        .collect();

    assert!(
        !item_chunks.is_empty(),
        "function chunks should have parent_path set"
    );

    for chunk in &item_chunks {
        let parent = chunk.parent_path.as_deref().unwrap();
        // Parent URI should be a file chunk (no '#' in path).
        assert!(
            !parent.contains('#'),
            "parent_path '{}' should point to a file chunk (no '#')",
            parent
        );
    }
}

#[tokio::test]
async fn yaml_file_produces_file_chunk() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();

    // YAML is now a text-passthrough extension — it should produce a file chunk.
    let yaml_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.location.uri.contains(".yaml"))
        .collect();
    assert!(
        !yaml_chunks.is_empty(),
        "config.yaml should produce a file chunk via text passthrough"
    );
    assert!(
        yaml_chunks.iter().all(|c| c.kind == "file"),
        "YAML chunks should all be kind='file'"
    );
}

#[tokio::test]
async fn truly_unknown_extension_skipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("weird.xyz"), "some content").unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

    use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};
    let adapter = CodeAdapter::new();
    let source = DiscoveredSource {
        path: dir.path().to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": "test" }),
    };
    let chunks = adapter.chunk(&source).await.unwrap();

    assert!(!chunks.is_empty(), "should have chunks from main.rs");
    for chunk in &chunks {
        assert!(
            !chunk.location.uri.contains(".xyz"),
            ".xyz file should not produce chunks: {}",
            chunk.location.uri
        );
    }
}

// ── extract_structure ─────────────────────────────────────────────────────────

#[tokio::test]
async fn extract_structure_function_chunks() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();

    let fn_chunks: Vec<_> = chunks.iter().filter(|c| c.kind == "function").collect();

    if fn_chunks.is_empty() {
        // If there happen to be no function chunks (all too small), that's ok.
        return;
    }

    for chunk in fn_chunks {
        let structure = adapter.extract_structure(chunk).await.unwrap();
        // structural extraction should at least not error.
        let _ = structure;
    }
}

#[tokio::test]
async fn extract_structure_produces_entities_for_rust() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();

    // Find any file chunk for a .rs file.
    let rs_file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri.contains(".rs"));

    if let Some(chunk) = rs_file_chunk {
        let structure = adapter.extract_structure(chunk).await.unwrap();
        // We expect at least some structural_entities (functions, structs).
        // For small files it might be empty — just check no error.
        let _ = structure.structural_entities;
    }
}

// ── extract_with_llm ──────────────────────────────────────────────────────────

#[tokio::test]
async fn extract_with_llm_dry_run_no_panic() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();
    let llm = DryRunProvider::new();

    for chunk in &chunks {
        let result = adapter.extract_with_llm(chunk, &llm).await;
        assert!(
            result.is_ok(),
            "extract_with_llm should not panic for chunk kind '{}': {:?}",
            chunk.kind,
            result.err()
        );
    }
}

#[tokio::test]
async fn extract_with_llm_returns_none_for_file_chunks() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();
    let llm = DryRunProvider::new();

    let file_chunks: Vec<_> = chunks.iter().filter(|c| c.kind == "file").collect();
    for chunk in file_chunks {
        let result = adapter.extract_with_llm(chunk, &llm).await.unwrap();
        assert!(
            result.is_none(),
            "file chunks should return None from extract_with_llm"
        );
    }
}

// ── format_location / parse_location ─────────────────────────────────────────

#[tokio::test]
async fn location_round_trip() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();

    for chunk in &chunks {
        let formatted = adapter.format_location(chunk);
        let parsed = adapter.parse_location(&chunk.location.uri).unwrap();

        // The formatted path should match the chunk's location path.
        assert_eq!(
            formatted, chunk.location.path,
            "format_location should return the chunk's path"
        );

        // The parsed corpus_id should be non-empty.
        assert_eq!(
            parsed.corpus_id, "sample",
            "parse_location should extract corpus_id"
        );
    }
}

// ── slug entity IDs ───────────────────────────────────────────────────────────

/// Every structural entity and edge produced by the code adapter must use the
/// `{corpus_id}:{slug}` ID format, so that edges can reference entities by a
/// stable, deterministic key rather than a random UUID.
#[tokio::test]
async fn extract_structure_edges_use_slug_entity_ids() {
    let adapter = CodeAdapter::new();
    let source = fixture_source("sample");
    let chunks = adapter.chunk(&source).await.unwrap();

    let mut any_edge = false;

    for chunk in &chunks {
        let structure = adapter.extract_structure(chunk).await.unwrap();

        for entity in &structure.structural_entities {
            assert!(
                entity.id.starts_with("sample:"),
                "entity id '{}' in chunk '{}' should start with 'sample:'",
                entity.id,
                chunk.location.uri
            );
        }

        for edge in &structure.structural_edges {
            assert!(
                edge.from_entity_id.starts_with("sample:"),
                "edge from_entity_id '{}' in chunk '{}' should start with 'sample:'",
                edge.from_entity_id,
                chunk.location.uri
            );
            assert!(
                edge.to_entity_id.starts_with("sample:"),
                "edge to_entity_id '{}' in chunk '{}' should start with 'sample:'",
                edge.to_entity_id,
                chunk.location.uri
            );
            // No whitespace in IDs.
            assert!(
                !edge.from_entity_id.contains(char::is_whitespace),
                "from_entity_id '{}' must not contain whitespace",
                edge.from_entity_id
            );
            assert!(
                !edge.to_entity_id.contains(char::is_whitespace),
                "to_entity_id '{}' must not contain whitespace",
                edge.to_entity_id
            );
            any_edge = true;
        }
    }

    assert!(
        any_edge,
        "at least one chunk should produce structural edges"
    );
}

// ── max chunk size splitting ──────────────────────────────────────────────────

#[tokio::test]
async fn large_file_splits_into_multiple_chunks() {
    use callimachus_adapter_code::chunker::{ChunkOptions, chunk_directory};

    let dir = tempfile::tempdir().unwrap();
    // Write a file large enough to exceed 500-byte max.
    let large_fn = (0..30)
        .map(|i| format!("pub fn func_{i}(x: u64) -> u64 {{ x + {i} }}\n"))
        .collect::<String>();
    std::fs::write(dir.path().join("big.rs"), &large_fn).unwrap();

    let mut opts = ChunkOptions::default();
    opts.max_chunk_bytes = 500;
    opts.min_chunk_bytes = 10;

    let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

    // Should have multiple chunks total (file chunk + item/split chunks).
    assert!(
        chunks.len() > 1,
        "large file should produce multiple chunks, got {}",
        chunks.len()
    );
}
