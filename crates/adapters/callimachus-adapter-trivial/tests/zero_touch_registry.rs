//! A3 / A5 — Zero-touch external adapter integration tests.
//!
//! **A3**: A new corpus kind (`"plain"`) can be indexed end-to-end with zero edits
//! to `callimachus-core` and zero edits to the host binary's adapter-selection logic.
//! The trivial adapter is registered into an [`AdapterRegistry`] alongside the four
//! built-ins, resolved by kind, and driven through the full chunk + structure pipeline.
//!
//! **A5**: The adapter's non-dev dependency closure contains only
//! `callimachus-adapter-contract` (plus transitive deps). It does NOT depend on
//! `callimachus-core` or `rusqlite`. This constraint is verified structurally by the
//! Cargo.toml; these tests exercise the runtime behaviour that proves the seam is
//! operational.

use std::sync::Arc;

use callimachus_adapter_contract::AdapterRegistry;
use callimachus_core::{
    indexing::pipeline::{IndexOptions, IndexPipeline},
    storage::{SqliteBackend, StorageBackend},
    types::{Corpus, Pass},
};
use callimachus_llm::DryRunProvider;

use callimachus_adapter_book::BookAdapter;
use callimachus_adapter_capture::CaptureAdapter;
use callimachus_adapter_code::CodeAdapter;
use callimachus_adapter_wiki::WikiAdapter;
use callimachus_adapter_trivial::TrivialAdapter;

/// Build a registry with the four built-in adapters plus `TrivialAdapter`.
fn five_adapter_registry() -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(BookAdapter::new()));
    registry.register(Arc::new(CaptureAdapter::new()));
    registry.register(Arc::new(CodeAdapter::new()));
    registry.register(Arc::new(WikiAdapter::new()));
    registry.register(TrivialAdapter::arc());
    registry
}

// ── A3: end-to-end indexing of a new kind without touching callimachus-core ──

/// A `"plain"` corpus whose source contains two `.txt` files is indexed
/// end-to-end (Chunk + Structure passes) and produces chunks and entities —
/// with zero changes to callimachus-core or to any existing adapter-selection
/// logic. The adapter is resolved purely through the registry.
#[tokio::test]
async fn plain_kind_indexes_end_to_end_via_registry() {
    // Set up a source directory with two text files.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("alpha.txt"), "Hello from alpha.").unwrap();
    std::fs::write(dir.path().join("beta.txt"), "Hello from beta.").unwrap();

    let db: Arc<dyn StorageBackend> = Arc::new(SqliteBackend::open_in_memory().unwrap());
    let corpus = Corpus::new(
        "plain-e2e".to_string(),
        "Plain E2E Corpus".to_string(),
        "plain".to_string(),
        dir.path().to_string_lossy().to_string(),
    );
    db.corpus_insert(&corpus).unwrap();

    // Build registry with built-ins + trivial, then resolve via kind lookup.
    let registry = five_adapter_registry();
    let adapter = registry
        .get("plain")
        .expect("plain adapter must be present after registration");

    let pipeline = IndexPipeline {
        db: db.clone(),
        adapter,
        llm: Arc::new(DryRunProvider::new()),
        embedder: None,
    };

    pipeline
        .run(
            &corpus,
            IndexOptions {
                passes: vec![Pass::Chunk, Pass::Structure],
                ..IndexOptions::default()
            },
        )
        .await
        .expect("pipeline run must succeed for plain kind");

    // The TrivialAdapter turns each .txt file into one chunk and one entity.
    let chunks = db.chunk_count("plain-e2e").unwrap();
    let entities = db.entity_count("plain-e2e").unwrap();

    assert!(
        chunks > 0,
        "expected at least one chunk after indexing 2 .txt files, got 0"
    );
    assert!(
        entities > 0,
        "expected at least one entity after structure pass on plain corpus, got 0"
    );
}

// ── A3: registry composes new kind alongside built-ins without conflicts ──

/// Registering `TrivialAdapter` into a registry that already holds the four
/// built-in adapters results in all five kinds being available — no kind is
/// evicted or shadowed.
#[test]
fn registry_lists_all_five_kinds_including_plain() {
    let registry = five_adapter_registry();
    let kinds = registry.list();

    assert!(
        kinds.contains(&"book"),
        "registry should contain built-in kind 'book'; got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"capture"),
        "registry should contain built-in kind 'capture'; got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"code"),
        "registry should contain built-in kind 'code'; got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"wiki"),
        "registry should contain built-in kind 'wiki'; got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&"plain"),
        "registry should contain new kind 'plain' after TrivialAdapter registration; got: {:?}",
        kinds
    );
    assert_eq!(
        kinds.len(),
        5,
        "expected exactly 5 adapter kinds; got: {:?}",
        kinds
    );
}
