//! Integration tests verifying PHP / Vue extraction quality against a
//! miniature Laravel-shaped fixture corpus.
//!
//! These tests are the **durable artifact** for the PHP/Vue launch gate
//! (W21).  They assert exact chunk/entity/edge inventory so regressions
//! are caught before indexing the real launch repos.
//!
//! Fixture layout:
//!   app/Http/Controllers/UserController.php  — Laravel controller
//!   app/Models/User.php                      — Eloquent model
//!   app/Jobs/SendWelcomeEmail.php             — Job class (implements interface)
//!   resources/views/users/index.blade.php    — Blade template (HTML + directives)
//!   resources/js/components/UserCard.vue     — Vue 2-style SFC
//!   resources/js/components/ProfilePage.vue  — Vue 3 <script setup lang="ts">
//!   routes/web.php                           — Route declarations (no classes)

use callimachus_adapter_code::CodeAdapter;
use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};
use std::path::PathBuf;

fn laravel_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("laravel_project")
}

fn laravel_source(corpus_id: &str) -> DiscoveredSource {
    DiscoveredSource {
        path: laravel_fixture_dir().to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": corpus_id }),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Collect all chunks for a specific file (by suffix match on URI).
fn chunks_for_file<'a>(
    chunks: &'a [callimachus_core::types::Chunk],
    file_suffix: &str,
) -> Vec<&'a callimachus_core::types::Chunk> {
    chunks
        .iter()
        .filter(|c| c.location.uri().contains(file_suffix))
        .collect()
}

// ── PHP controller ────────────────────────────────────────────────────────────

#[tokio::test]
async fn php_controller_produces_file_and_class_chunks() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunks = chunks_for_file(&chunks, "UserController.php");
    assert!(
        !file_chunks.is_empty(),
        "UserController.php should produce at least one chunk"
    );

    // File chunk exists.
    let file_chunk = file_chunks.iter().find(|c| c.kind == "file");
    assert!(
        file_chunk.is_some(),
        "UserController.php should have a file-level chunk; got kinds: {:?}",
        file_chunks.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );

    // A class-level item chunk exists for UserController.
    // (PHP chunker produces one chunk per class_declaration, not per method.)
    let class_chunk = file_chunks
        .iter()
        .find(|c| c.kind == "class" && c.location.uri().contains("UserController"));
    assert!(
        class_chunk.is_some(),
        "UserController.php should have a class-level chunk; item chunks: {:?}",
        file_chunks
            .iter()
            .filter(|c| c.kind != "file")
            .map(|c| format!("{}:{}", c.kind, c.location.uri()))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn php_controller_structure_has_class_entity() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("UserController.php"))
        .expect("UserController.php file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let class_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "class" && e.canonical_name == "UserController");
    assert!(
        class_entity.is_some(),
        "should extract class entity 'UserController'; entities found: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn php_controller_structure_has_method_entities() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("UserController.php"))
        .expect("UserController.php file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let method_names: Vec<&str> = structure
        .structural_entities
        .iter()
        .filter(|e| e.kind == "method")
        .map(|e| e.canonical_name.as_str())
        .collect();

    for expected_method in &["index", "show", "store", "destroy"] {
        assert!(
            method_names.contains(expected_method),
            "should have method entity '{}'; method entities found: {:?}",
            expected_method,
            method_names
        );
    }
}

#[tokio::test]
async fn php_controller_structure_has_extends_edge() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("UserController.php"))
        .expect("UserController.php file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let extends_to_controller = structure.structural_edges.iter().any(|e| {
        e.kind == "extends"
            && e.from_entity_id.contains("usercontroller")
            && e.to_entity_id.contains("controller")
    });
    assert!(
        extends_to_controller,
        "should have extends edge UserController → Controller; edges: {:?}",
        structure
            .structural_edges
            .iter()
            .filter(|e| e.kind == "extends")
            .map(|e| format!("{} → {}", e.from_entity_id, e.to_entity_id))
            .collect::<Vec<_>>()
    );
}

// ── Eloquent model ────────────────────────────────────────────────────────────

#[tokio::test]
async fn php_model_produces_class_entity_extending_model() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("Models/User.php"))
        .expect("Models/User.php file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    // Class entity for User.
    let class_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "class" && e.canonical_name == "User");
    assert!(
        class_entity.is_some(),
        "should extract class entity 'User'; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );

    // extends User → Model.
    let extends_model = structure.structural_edges.iter().any(|e| {
        e.kind == "extends" && e.from_entity_id.contains("user") && e.to_entity_id.contains("model")
    });
    assert!(
        extends_model,
        "should have extends edge User → Model; edges: {:?}",
        structure
            .structural_edges
            .iter()
            .filter(|e| e.kind == "extends")
            .map(|e| format!("{} → {}", e.from_entity_id, e.to_entity_id))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn php_model_has_relationship_method_entities() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("Models/User.php"))
        .expect("Models/User.php file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let method_names: Vec<&str> = structure
        .structural_entities
        .iter()
        .filter(|e| e.kind == "method")
        .map(|e| e.canonical_name.as_str())
        .collect();

    for expected in &["posts", "profile"] {
        assert!(
            method_names.contains(expected),
            "should have relationship method '{}'; method entities: {:?}",
            expected,
            method_names
        );
    }
}

// ── Job class (implements interface) ─────────────────────────────────────────

#[tokio::test]
async fn php_job_has_implements_edge_to_should_queue() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("SendWelcomeEmail.php"))
        .expect("SendWelcomeEmail.php file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    // Class entity.
    let class_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "class" && e.canonical_name == "SendWelcomeEmail");
    assert!(
        class_entity.is_some(),
        "should extract class entity 'SendWelcomeEmail'; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );

    // implements SendWelcomeEmail → ShouldQueue.
    let implements_should_queue = structure.structural_edges.iter().any(|e| {
        e.kind == "implements"
            && e.from_entity_id.contains("sendwelcomeemail")
            && e.to_entity_id.contains("shouldqueue")
    });
    assert!(
        implements_should_queue,
        "should have implements edge SendWelcomeEmail → ShouldQueue; edges: {:?}",
        structure
            .structural_edges
            .iter()
            .filter(|e| e.kind == "implements")
            .map(|e| format!("{} → {}", e.from_entity_id, e.to_entity_id))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn php_job_has_handle_and_failed_method_entities() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("SendWelcomeEmail.php"))
        .expect("SendWelcomeEmail.php file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let method_names: Vec<&str> = structure
        .structural_entities
        .iter()
        .filter(|e| e.kind == "method")
        .map(|e| e.canonical_name.as_str())
        .collect();

    for expected in &["handle", "failed"] {
        assert!(
            method_names.contains(expected),
            "should have method entity '{}'; method entities: {:?}",
            expected,
            method_names
        );
    }
}

// ── Blade template ────────────────────────────────────────────────────────────

/// Blade templates have `.blade.php` extension.  The PHP parser is applied
/// (ext == "php") but Blade directives like `@extends` are not valid PHP.
/// Expected behavior: clean degradation to a single file-level chunk only,
/// with zero entities (no class/function declarations in a Blade view).
#[tokio::test]
async fn blade_template_degrades_to_single_file_chunk() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let blade_chunks = chunks_for_file(&chunks, "index.blade.php");
    assert!(
        !blade_chunks.is_empty(),
        "index.blade.php should produce at least one chunk (file chunk)"
    );

    let non_file: Vec<_> = blade_chunks.iter().filter(|c| c.kind != "file").collect();
    assert!(
        non_file.is_empty(),
        "Blade template should produce ONLY a file chunk, not item chunks; \
        got {} non-file chunks with kinds: {:?}",
        non_file.len(),
        non_file.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn blade_template_produces_no_class_or_function_entities() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let blade_file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("index.blade.php"))
        .expect("index.blade.php file chunk must exist");

    let structure = adapter.extract_structure(blade_file_chunk).await.unwrap();

    // The extractor always emits a file-level entity as an FK anchor.
    // What we must NOT see: class or function/method entities produced by
    // accidentally parsing Blade directives as PHP declarations.
    let class_or_fn_entities: Vec<_> = structure
        .structural_entities
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "class" | "function" | "method" | "interface"
            )
        })
        .collect();

    assert!(
        class_or_fn_entities.is_empty(),
        "Blade template must not produce class/function/method entities (Blade directives \
        are not PHP declarations); got: {:?}",
        class_or_fn_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );
}

// ── Vue 2-style SFC ───────────────────────────────────────────────────────────

#[tokio::test]
async fn vue2_sfc_produces_file_chunk_and_script_chunks() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let vue_chunks = chunks_for_file(&chunks, "UserCard.vue");

    let file_chunk = vue_chunks.iter().find(|c| c.kind == "file");
    assert!(
        file_chunk.is_some(),
        "UserCard.vue should produce a file-level chunk"
    );

    // Vue 2 `export default { ... }` is an export_statement, which the TS
    // grammar captures.  We expect at least one non-file chunk from the script.
    let item_chunks: Vec<_> = vue_chunks.iter().filter(|c| c.kind != "file").collect();
    assert!(
        !item_chunks.is_empty(),
        "UserCard.vue should produce at least one item chunk from the <script> block; \
        all chunk kinds: {:?}",
        vue_chunks.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
}

// ── Vue 3 <script setup lang="ts"> ───────────────────────────────────────────

#[tokio::test]
async fn vue3_setup_sfc_produces_file_chunk_and_function_chunks() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let vue_chunks = chunks_for_file(&chunks, "ProfilePage.vue");

    let file_chunk = vue_chunks.iter().find(|c| c.kind == "file");
    assert!(
        file_chunk.is_some(),
        "ProfilePage.vue should produce a file-level chunk"
    );

    // <script setup lang="ts"> is detected as TSX and the TypeScript parser
    // extracts top-level function declarations.
    let fn_chunks: Vec<_> = vue_chunks
        .iter()
        .filter(|c| c.kind == "function" || c.kind == "class")
        .collect();
    assert!(
        !fn_chunks.is_empty(),
        "ProfilePage.vue should produce function chunks from <script setup>; \
        all chunk kinds: {:?}",
        vue_chunks.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn vue3_setup_function_names_are_correct() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let function_uris: Vec<_> = chunks
        .iter()
        .filter(|c| {
            c.location.uri().contains("ProfilePage.vue")
                && (c.kind == "function" || c.kind == "class")
        })
        .map(|c| c.location.uri())
        .collect();

    for expected_fn in &["save", "reset"] {
        assert!(
            function_uris.iter().any(|uri| uri.contains(expected_fn)),
            "ProfilePage.vue should have a chunk for function '{}'; function URIs: {:?}",
            expected_fn,
            function_uris
        );
    }
}

// ── Routes file ───────────────────────────────────────────────────────────────

// ── Routes file ───────────────────────────────────────────────────────────────

/// The routes file has no PHP class declarations — just `Route::get()` calls
/// and anonymous closures.  Expected: one file chunk only.
#[tokio::test]
async fn routes_file_produces_file_chunk_only() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let route_chunks = chunks_for_file(&chunks, "routes/web.php");
    assert!(
        !route_chunks.is_empty(),
        "routes/web.php should produce at least a file chunk"
    );

    let non_file: Vec<_> = route_chunks.iter().filter(|c| c.kind != "file").collect();
    assert!(
        non_file.is_empty(),
        "routes/web.php has no class/function declarations at top level; \
        expected zero item chunks, got {} with kinds: {:?}",
        non_file.len(),
        non_file
            .iter()
            .map(|c| format!("{}:{}", c.kind, c.location.uri()))
            .collect::<Vec<_>>()
    );
}

// ── Cross-cutting: entity ID hygiene ─────────────────────────────────────────

#[tokio::test]
async fn all_php_entity_ids_use_slug_format() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    let php_file_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| {
            c.kind == "file"
                && (c.location.uri().ends_with(".php"))
                && !c.location.uri().contains(".blade.php")
        })
        .collect();

    for chunk in php_file_chunks {
        let structure = adapter.extract_structure(chunk).await.unwrap();
        for entity in &structure.structural_entities {
            assert!(
                entity.id.starts_with("laravel:"),
                "PHP entity id '{}' in '{}' should start with 'laravel:'",
                entity.id,
                chunk.location.uri()
            );
            assert!(
                !entity.id.contains(char::is_whitespace),
                "entity id '{}' must not contain whitespace",
                entity.id
            );
        }
    }
}

// ── No panics on full corpus walk ─────────────────────────────────────────────

#[tokio::test]
async fn full_laravel_fixture_chunk_does_not_panic() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let result = adapter.chunk(&source).await;
    assert!(
        result.is_ok(),
        "chunk() must not error on the full Laravel fixture corpus: {:?}",
        result.err()
    );

    let chunks = result.unwrap();
    assert!(
        chunks.len() >= 7,
        "laravel fixture has 7 source files; expected ≥7 chunks (file chunks alone), got {}",
        chunks.len()
    );
}

#[tokio::test]
async fn extract_structure_does_not_panic_for_any_laravel_chunk() {
    let adapter = CodeAdapter::new();
    let source = laravel_source("laravel");
    let chunks = adapter.chunk(&source).await.unwrap();

    for chunk in &chunks {
        let result = adapter.extract_structure(chunk).await;
        assert!(
            result.is_ok(),
            "extract_structure must not error for chunk '{}' (kind={}): {:?}",
            chunk.location.uri(),
            chunk.kind,
            result.err()
        );
    }
}
