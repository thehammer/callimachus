//! Integration tests verifying that extensionless scripts, Makefiles and
//! well-known config files are detected, chunked and structurally extracted,
//! against a miniature fixture project.
//!
//! Fixture layout (`tests/fixtures/script_project`):
//!   bin/tool         — extensionless `#!/usr/bin/env bash`; log_line, build_image,
//!                      push_image, deploy (deploy calls build_image + push_image)
//!   bin/pyscript     — extensionless `#!/usr/bin/env -S python3.12 -u`; run_report
//!   Makefile         — targets build, test, clean
//!   Dockerfile.dev   — text passthrough, label "dockerfile"
//!   Caddyfile.edge   — text passthrough, label "caddyfile"
//!   stack.env        — text passthrough, label "text"
//!   site.conf        — text passthrough, label "text"
//!   NOTES            — extensionless, no shebang: must NOT be indexed
//!   weird.xyz        — unknown extension: must NOT be indexed

use callimachus_adapter_code::CodeAdapter;
use callimachus_adapter_code::extractor;
use callimachus_adapter_code::languages;
use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};
use callimachus_core::types::Chunk;
use std::path::PathBuf;

fn script_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("script_project")
}

fn sample_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_project")
}

fn source_for(dir: &std::path::Path, corpus_id: &str) -> DiscoveredSource {
    DiscoveredSource {
        path: dir.to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({
            "corpus_id": corpus_id,
            "no_git_filter": true,
        }),
    }
}

async fn chunk_script_fixture() -> Vec<Chunk> {
    CodeAdapter::new()
        .chunk(&source_for(&script_fixture_dir(), "scripts"))
        .await
        .expect("chunk() must not error on the script fixture")
}

fn find<'a>(chunks: &'a [Chunk], path: &str) -> &'a Chunk {
    chunks
        .iter()
        .find(|c| c.location.path == path)
        .unwrap_or_else(|| {
            panic!(
                "no chunk at {path}; have: {:?}",
                chunks.iter().map(|c| &c.location.path).collect::<Vec<_>>()
            )
        })
}

fn of_file<'a>(chunks: &'a [Chunk], file_path: &str) -> Vec<&'a Chunk> {
    chunks
        .iter()
        .filter(|c| c.location.path.split('#').next() == Some(file_path))
        .collect()
}

// ── Chunking ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn script_fixture_chunking_does_not_error_and_skips_unrecognised_files() {
    let chunks = chunk_script_fixture().await;
    assert!(!chunks.is_empty());
    assert!(
        of_file(&chunks, "src/NOTES").is_empty(),
        "extensionless file without a shebang must not be indexed"
    );
    assert!(
        of_file(&chunks, "src/weird.xyz").is_empty(),
        "unknown extension must not be indexed"
    );
}

#[tokio::test]
async fn bin_tool_is_chunked_into_one_function_chunk_per_function() {
    let chunks = chunk_script_fixture().await;

    let file = find(&chunks, "src/bin/tool");
    assert_eq!(file.kind, "file");
    assert_eq!(file.language.as_deref(), Some("bash"));

    // Covers both `name() {` and `function name {` / `function name() {` forms.
    for name in ["log_line", "build_image", "push_image", "deploy"] {
        let c = find(&chunks, &format!("src/bin/tool#{name}"));
        assert_eq!(c.kind, "function", "{name}");
        assert_eq!(c.language.as_deref(), Some("bash"), "{name}");
        assert_eq!(c.parent_path.as_deref(), Some(file.location.uri().as_str()));
    }
}

#[tokio::test]
async fn bin_pyscript_is_chunked_as_python() {
    let chunks = chunk_script_fixture().await;

    let file = find(&chunks, "src/bin/pyscript");
    assert_eq!(file.language.as_deref(), Some("python"));
    let func = find(&chunks, "src/bin/pyscript#run_report");
    assert_eq!(func.kind, "function");
    assert_eq!(func.language.as_deref(), Some("python"));
}

#[tokio::test]
async fn makefile_is_chunked_into_one_item_chunk_per_target() {
    let chunks = chunk_script_fixture().await;

    let file = find(&chunks, "src/Makefile");
    assert_eq!(file.kind, "file");
    assert_eq!(file.language.as_deref(), Some("make"));
    for target in ["build", "test", "clean"] {
        let c = find(&chunks, &format!("src/Makefile#{target}"));
        assert_eq!(c.language.as_deref(), Some("make"), "{target}");
        assert!(
            c.content.starts_with(target),
            "chunk for {target} should hold that rule, got {:?}",
            c.content
        );
    }
}

#[tokio::test]
async fn config_files_are_single_text_chunks_with_their_label() {
    let chunks = chunk_script_fixture().await;

    for (path, label) in [
        ("src/Dockerfile.dev", "dockerfile"),
        ("src/Caddyfile.edge", "caddyfile"),
        ("src/stack.env", "text"),
        ("src/site.conf", "text"),
    ] {
        let of = of_file(&chunks, path);
        assert_eq!(of.len(), 1, "{path} should be exactly one chunk");
        assert_eq!(of[0].kind, "file", "{path}");
        assert_eq!(of[0].language.as_deref(), Some(label), "{path}");
    }
}

#[tokio::test]
async fn every_script_fixture_chunk_has_a_language() {
    let chunks = chunk_script_fixture().await;
    for c in &chunks {
        assert!(
            c.language.is_some(),
            "chunk {} has no language",
            c.location.path
        );
    }
}

#[tokio::test]
async fn every_sample_project_chunk_has_a_language() {
    let chunks = CodeAdapter::new()
        .chunk(&source_for(&sample_fixture_dir(), "sample"))
        .await
        .unwrap();
    assert!(!chunks.is_empty());
    for c in &chunks {
        assert!(
            c.language.is_some(),
            "chunk {} has no language",
            c.location.path
        );
    }
}

// ── Structure extraction ──────────────────────────────────────────────────────

#[tokio::test]
async fn bin_tool_file_chunk_yields_a_function_entity_per_function() {
    let chunks = chunk_script_fixture().await;
    let file = find(&chunks, "src/bin/tool");

    let structure = CodeAdapter::new().extract_structure(file).await.unwrap();

    let functions: Vec<&str> = structure
        .structural_entities
        .iter()
        .filter(|e| e.kind == "function")
        .map(|e| e.canonical_name.as_str())
        .collect();
    for name in ["log_line", "build_image", "push_image", "deploy"] {
        assert!(
            functions.contains(&name),
            "expected function entity {name}, got {functions:?}"
        );
    }
}

#[tokio::test]
async fn bin_tool_calls_between_functions_become_calls_edges() {
    let chunks = chunk_script_fixture().await;
    let deploy = find(&chunks, "src/bin/tool#deploy");

    let bash = languages::for_name("bash").expect("bash grammar is registered");
    let structure = extractor::extract_structure(deploy, bash).unwrap();

    for callee in ["build_image", "push_image"] {
        assert!(
            structure.edges.iter().any(|e| e.kind == "calls"
                && e.from_entity_id.ends_with("deploy")
                && e.to_entity_id.ends_with(callee)),
            "expected a `calls` edge deploy -> {callee}; calls: {:?}",
            structure
                .edges
                .iter()
                .filter(|e| e.kind == "calls")
                .map(|e| (&e.from_entity_id, &e.to_entity_id))
                .collect::<Vec<_>>()
        );
    }
}

/// The item chunk's body has no shebang and its path has no extension, so only
/// the persisted `language` can tell the adapter it is bash.
#[tokio::test]
async fn adapter_extracts_structure_from_extensionless_item_chunk_via_persisted_language() {
    let chunks = chunk_script_fixture().await;
    let deploy = find(&chunks, "src/bin/tool#deploy");
    assert!(
        !deploy.content.starts_with("#!"),
        "precondition: item chunk body carries no shebang"
    );
    assert_eq!(deploy.language.as_deref(), Some("bash"));

    let structure = CodeAdapter::new().extract_structure(deploy).await.unwrap();

    assert!(
        structure
            .structural_entities
            .iter()
            .any(|e| e.kind == "function" && e.canonical_name == "deploy"),
        "expected a `deploy` function entity, got {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| (&e.kind, &e.canonical_name))
            .collect::<Vec<_>>()
    );
    assert!(
        structure
            .structural_edges
            .iter()
            .any(|e| e.kind == "calls" && e.to_entity_id.ends_with("build_image")),
        "expected a calls edge to build_image"
    );
}

#[tokio::test]
async fn bin_pyscript_yields_a_function_entity() {
    let chunks = chunk_script_fixture().await;
    let file = find(&chunks, "src/bin/pyscript");

    let structure = CodeAdapter::new().extract_structure(file).await.unwrap();

    assert!(
        structure
            .structural_entities
            .iter()
            .any(|e| e.kind == "function" && e.canonical_name == "run_report"),
        "expected a run_report function entity, got {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| (&e.kind, &e.canonical_name))
            .collect::<Vec<_>>()
    );
}

// ── Real-repo coverage gate ───────────────────────────────────────────────────
//
// Runs against the real preview-stack repository at ~/Code/preview-stack.
// `#[ignore]`d so normal CI does not require the local checkout.
//
//   cargo test -p callimachus-adapter-code --test script_coverage -- --include-ignored

#[tokio::test]
#[ignore = "requires ~/Code/preview-stack (local checkout)"]
async fn preview_stack_bin_entities_meet_floor() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/hammer".to_string());
    let dir = PathBuf::from(home).join("Code").join("preview-stack");
    if !dir.exists() {
        eprintln!("skip: preview-stack not found at {}", dir.display());
        return;
    }

    let adapter = CodeAdapter::new();
    let source = DiscoveredSource {
        path: dir.to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": "preview_stack" }),
    };
    let chunks = adapter.chunk(&source).await.expect("chunk must not error");

    let mut function_names = std::collections::BTreeSet::new();
    for chunk in &chunks {
        if !chunk.location.path.starts_with("src/bin/preview-stack") {
            continue;
        }
        if let Ok(structure) = adapter.extract_structure(chunk).await {
            for e in structure.structural_entities {
                if e.kind == "function" {
                    function_names.insert(e.canonical_name);
                }
            }
        }
    }

    eprintln!(
        "preview-stack: {} distinct function entities from src/bin/preview-stack",
        function_names.len()
    );
    assert!(
        function_names.len() >= 60,
        "expected at least 60 function entities from bin/preview-stack; got {}",
        function_names.len()
    );
}
