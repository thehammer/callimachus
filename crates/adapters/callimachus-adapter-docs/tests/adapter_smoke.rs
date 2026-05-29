/// Smoke test for the DocsAdapter.
///
/// Loads the three handwritten DocC JSON fixture files, calls discover → chunk →
/// extract_structure, and asserts entity kinds, edge kinds, and parent/child membership.
use callimachus_adapter_docs::{DocsAdapter, adapter::extract_from_source};
use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};
use std::path::Path;

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

#[tokio::test]
async fn discover_finds_three_json_files() {
    let dir = fixtures_dir();
    let adapter = DocsAdapter::new();
    let sources = adapter
        .discover(dir.to_str().unwrap())
        .await
        .expect("discover failed");
    assert_eq!(
        sources.len(),
        3,
        "expected 3 fixture files, got: {sources:?}"
    );
}

#[tokio::test]
async fn chunk_produces_page_and_section_chunks() {
    let dir = fixtures_dir();
    let adapter = DocsAdapter::new();
    let mut sources = adapter
        .discover(dir.to_str().unwrap())
        .await
        .expect("discover failed");

    // Sort for deterministic order.
    sources.sort_by(|a, b| a.path.cmp(&b.path));

    let mut total_pages = 0;
    let mut total_sections = 0;

    for source in &mut sources {
        // Inject corpus_id into meta.
        source.meta["corpus_id"] = serde_json::Value::String("test-corpus".to_string());

        let chunks = adapter.chunk(source).await.expect("chunk failed");
        assert!(
            !chunks.is_empty(),
            "expected at least one chunk for {source:?}"
        );

        for chunk in &chunks {
            match chunk.kind.as_str() {
                "page" => total_pages += 1,
                "section" => total_sections += 1,
                other => panic!("unexpected chunk kind: {other}"),
            }
        }
    }

    // Each fixture has one page chunk; the ones with Discussion sections get section chunks.
    assert_eq!(total_pages, 3, "expected 3 page chunks");
    assert!(
        total_sections >= 3,
        "expected at least one section chunk per fixture, got {total_sections}"
    );
}

#[tokio::test]
async fn nsview_entity_is_class_with_edges() {
    let dir = fixtures_dir();
    let nsview_path = dir.join("AppKit").join("NSView.json");

    let source = DiscoveredSource {
        path: nsview_path.to_string_lossy().to_string(),
        kind: "docc".to_string(),
        meta: serde_json::json!({
            "root": dir.to_str().unwrap(),
            "corpus_id": "test-corpus"
        }),
    };

    let (chunks, structure) = extract_from_source(&source)
        .await
        .expect("extract failed");

    // Must produce chunks.
    assert!(!chunks.is_empty());

    // Must produce a primary entity.
    assert!(
        !structure.structural_entities.is_empty(),
        "expected at least one entity"
    );

    let entity = &structure.structural_entities[0];
    assert_eq!(entity.kind, "class", "NSView should be kind=class");
    assert_eq!(entity.canonical_name, "NSView");

    // Must have inherits_from and conforms_to edges.
    let edge_kinds: Vec<&str> = structure
        .structural_edges
        .iter()
        .map(|e| e.kind.as_str())
        .collect();

    assert!(
        edge_kinds.contains(&"inherits_from"),
        "expected inherits_from edge, got: {edge_kinds:?}"
    );
    assert!(
        edge_kinds.contains(&"conforms_to"),
        "expected conforms_to edge, got: {edge_kinds:?}"
    );

    // Must have member_of edges (from topicSections).
    assert!(
        edge_kinds.contains(&"member_of"),
        "expected member_of edge, got: {edge_kinds:?}"
    );

    // Must have references_type edge for NSResponder (from declaration tokens).
    let refs_type_edges: Vec<_> = structure
        .structural_edges
        .iter()
        .filter(|e| e.kind == "references_type")
        .collect();
    assert!(
        !refs_type_edges.is_empty(),
        "expected references_type edges"
    );

    // De-duplication check: NSResponder appears once in tokens; should appear once in edges.
    let responder_edges: Vec<_> = refs_type_edges
        .iter()
        .filter(|e| e.to_entity_id.contains("NSResponder"))
        .collect();
    assert_eq!(
        responder_edges.len(),
        1,
        "NSResponder references_type edge should be de-duplicated to 1, got {responder_edges:?}"
    );
}

#[tokio::test]
async fn nsview_tag_entity_is_property_with_availability() {
    let dir = fixtures_dir();
    let tag_path = dir.join("AppKit").join("NSView-tag.json");

    let source = DiscoveredSource {
        path: tag_path.to_string_lossy().to_string(),
        kind: "docc".to_string(),
        meta: serde_json::json!({
            "root": dir.to_str().unwrap(),
            "corpus_id": "test-corpus"
        }),
    };

    let (_chunks, structure) = extract_from_source(&source)
        .await
        .expect("extract failed");

    let entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "property")
        .expect("expected a property entity for NSView.tag");

    // Availability text should appear in the description.
    let desc = entity
        .description
        .as_deref()
        .expect("entity description should not be empty");
    assert!(
        desc.contains("macOS"),
        "description should contain availability info, got: {desc}"
    );
}

#[tokio::test]
async fn nsstackview_inherits_nsview_conforms_two_protocols() {
    let dir = fixtures_dir();
    let stack_path = dir.join("AppKit").join("NSStackView.json");

    let source = DiscoveredSource {
        path: stack_path.to_string_lossy().to_string(),
        kind: "docc".to_string(),
        meta: serde_json::json!({
            "root": dir.to_str().unwrap(),
            "corpus_id": "test-corpus"
        }),
    };

    let (_chunks, structure) = extract_from_source(&source)
        .await
        .expect("extract failed");

    let inherits_edges: Vec<_> = structure
        .structural_edges
        .iter()
        .filter(|e| e.kind == "inherits_from")
        .collect();
    assert_eq!(
        inherits_edges.len(),
        1,
        "NSStackView should have exactly 1 inherits_from edge"
    );
    assert!(
        inherits_edges[0].to_entity_id.contains("NSView"),
        "NSStackView should inherit from NSView, got: {:?}",
        inherits_edges[0].to_entity_id
    );

    let conforms_edges: Vec<_> = structure
        .structural_edges
        .iter()
        .filter(|e| e.kind == "conforms_to")
        .collect();
    assert_eq!(
        conforms_edges.len(),
        2,
        "NSStackView should conform to 2 protocols"
    );
}
