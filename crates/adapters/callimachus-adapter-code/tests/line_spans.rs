//! Tests that line spans are populated correctly by the code adapter.
//!
//! Acceptance criteria (from the plan):
//!   - Code chunks carry start_line/end_line (0-based, matching tree-sitter rows).
//!   - Code entities carry start_line/end_line for functions and classes.
//!   - PHP class and method entities have spans.
//!   - Vue SFC entities/chunks have **file-relative** spans (not script-block-relative).

use callimachus_adapter_code::{extractor::extract_structure, languages};
use callimachus_core::types::{Chunk, Location};
use std::path::PathBuf;

const CORPUS_ID: &str = "spans";

fn make_chunk(path: &str, kind: &str, content: &str) -> Chunk {
    Chunk::new(
        CORPUS_ID.to_string(),
        None,
        kind.to_string(),
        Location::new(CORPUS_ID, path),
        content.to_string(),
    )
}

// ── Rust spans (baseline) ─────────────────────────────────────────────────────

/// Rust function entities extracted from a chunk carry start_line/end_line.
#[test]
fn rust_function_entities_carry_spans() {
    let src = r#"fn greet(name: &str) -> String {
    format!("Hello, {name}")
}

struct Config {
    debug: bool,
}

impl Config {
    fn new() -> Self {
        Config { debug: false }
    }
}
"#;
    let chunk = make_chunk("src/lib.rs", "file", src);
    let lang = languages::for_extension("rs").unwrap();
    let result = extract_structure(&chunk, lang).unwrap();

    let entities_with_spans: Vec<_> = result
        .entities
        .iter()
        .filter(|e| e.start_line.is_some())
        .collect();

    assert!(
        !entities_with_spans.is_empty(),
        "expected at least one Rust entity with start_line; got {} entities: {:?}",
        result.entities.len(),
        result.entities.iter().map(|e| &e.canonical_name).collect::<Vec<_>>()
    );

    for entity in &entities_with_spans {
        let start = entity.start_line.unwrap();
        let end = entity.end_line.unwrap();
        assert!(
            end >= start,
            "end_line ({end}) >= start_line ({start}) for entity '{}'",
            entity.canonical_name
        );
    }
}

/// Rust chunks produced by the chunker carry start_line/end_line.
#[tokio::test]
async fn rust_chunks_carry_line_spans() {
    use callimachus_adapter_code::CodeAdapter;
    use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};

    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_project");

    let adapter = CodeAdapter::new();
    let source = DiscoveredSource {
        path: fixture_dir.to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": CORPUS_ID }),
    };
    let chunks = adapter.chunk(&source).await.unwrap();

    let fn_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.kind != "file" && c.start_line.is_some())
        .collect();

    assert!(
        !fn_chunks.is_empty(),
        "expected at least one function chunk with a span; got {} chunks total",
        chunks.len()
    );

    for chunk in &fn_chunks {
        let start = chunk.start_line.unwrap();
        let end = chunk.end_line.unwrap();
        assert!(
            end >= start,
            "end_line ({end}) >= start_line ({start}) for chunk {}",
            chunk.location.uri()
        );
    }
}

// ── PHP spans ─────────────────────────────────────────────────────────────────

/// PHP class and method entities carry line spans.
///
/// `UserService.php` (in `tests/fixtures/span_fixtures/`) defines:
///   - class `UserService`  (line 7, 0-based)
///   - method `findById`    (line 17, 0-based)
///   - method `create`      (line 27, 0-based)
///
/// We assert spans are populated and file-length-bounded; exact line numbers
/// are not hardcoded so minor reformatting doesn't break the test.
#[test]
fn php_class_and_method_entities_carry_spans() {
    let src = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/span_fixtures/UserService.php"),
    )
    .expect("UserService.php fixture must be readable");

    let chunk = make_chunk("UserService.php", "file", &src);
    let lang = languages::for_extension("php").unwrap();
    let result = extract_structure(&chunk, lang).unwrap();

    let entities_with_spans: Vec<_> = result
        .entities
        .iter()
        .filter(|e| e.start_line.is_some())
        .collect();

    assert!(
        !entities_with_spans.is_empty(),
        "expected PHP entities (class/methods) to have start_line; got {} entities: {:?}",
        result.entities.len(),
        result.entities.iter().map(|e| &e.canonical_name).collect::<Vec<_>>()
    );

    let file_lines = src.lines().count() as u32;
    for entity in &entities_with_spans {
        let start = entity.start_line.unwrap();
        let end = entity.end_line.unwrap();
        assert!(
            end >= start,
            "end_line ({end}) >= start_line ({start}) for PHP entity '{}'",
            entity.canonical_name
        );
        assert!(
            end < file_lines,
            "PHP entity '{}' end_line ({end}) must be < file length ({file_lines})",
            entity.canonical_name
        );
    }
}

/// PHP chunks from the chunker carry spans.
#[tokio::test]
async fn php_chunks_carry_line_spans() {
    use callimachus_adapter_code::CodeAdapter;
    use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};

    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/span_fixtures");

    let adapter = CodeAdapter::new();
    let source = DiscoveredSource {
        path: fixture_dir.to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": CORPUS_ID }),
    };
    let chunks = adapter.chunk(&source).await.unwrap();

    let php_fn_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.location.uri().contains("UserService.php") && c.kind != "file")
        .collect();

    assert!(
        !php_fn_chunks.is_empty(),
        "expected non-file PHP chunks from UserService.php"
    );

    for chunk in &php_fn_chunks {
        assert!(
            chunk.start_line.is_some(),
            "PHP chunk {} should have start_line",
            chunk.location.uri()
        );
        let start = chunk.start_line.unwrap();
        let end = chunk.end_line.unwrap();
        assert!(end >= start, "end_line >= start_line for PHP chunk");
    }
}

// ── Vue SFC spans — file-relative ─────────────────────────────────────────────

/// Vue SFC chunks carry file-relative spans (not script-block-relative).
///
/// `UserCard.vue` layout (0-based lines):
///   0:  `<template>`
///   ...
///   7:  `</template>` (empty line)
///   8:  `<script lang="ts">`   ← script-tag line
///   9:  `import { defineComponent } from 'vue';`  ← first body line
///   ...
///
/// File-relative spans for items inside the `<script>` block must have
/// `start_line >= 8`.  Script-relative spans would start near 0, which
/// would be incorrect for GitHub deep-links.
#[tokio::test]
async fn vue_chunks_carry_file_relative_spans() {
    use callimachus_adapter_code::CodeAdapter;
    use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};

    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/span_fixtures");

    let adapter = CodeAdapter::new();
    let source = DiscoveredSource {
        path: fixture_dir.to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": CORPUS_ID }),
    };
    let chunks = adapter.chunk(&source).await.unwrap();

    let vue_fn_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.location.uri().contains("UserCard.vue") && c.kind != "file")
        .collect();

    assert!(
        !vue_fn_chunks.is_empty(),
        "expected non-file Vue chunks from UserCard.vue; all chunks: {:?}",
        chunks
            .iter()
            .map(|c| c.location.uri())
            .collect::<Vec<_>>()
    );

    // <script lang="ts"> is on line 8 (0-based) in UserCard.vue.
    // File-relative spans for items inside it must be >= 8.
    let script_tag_line: u32 = 8;

    for chunk in &vue_fn_chunks {
        assert!(
            chunk.start_line.is_some(),
            "Vue SFC chunk {} should have a start_line",
            chunk.location.uri()
        );
        let start = chunk.start_line.unwrap();
        let end = chunk.end_line.unwrap();
        assert!(
            start >= script_tag_line,
            "Vue chunk '{}' start_line ({start}) must be file-relative (>= {script_tag_line}); \
             a smaller value indicates script-block-relative spans",
            chunk.location.uri()
        );
        assert!(end >= start, "end_line ({end}) >= start_line ({start}) for Vue chunk");
    }
}
