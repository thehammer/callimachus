//! Integration tests verifying Dart extraction quality against a miniature
//! Flutter-shaped fixture corpus.
//!
//! These tests are the **durable artifact** for the Dart launch gate.
//! They assert exact chunk/entity/edge inventory so regressions are caught
//! before indexing the real launch repos (employee_app).
//!
//! Fixture layout:
//!   lib/domain/contact.dart          — class Contact with named + factory constructors
//!   lib/domain/serializable.dart     — abstract class Serializable + mixin Timestamped
//!   lib/domain/status.dart           — enum ContactStatus
//!   lib/ui/contact_view_model.dart   — class ContactViewModel extends … with … implements …
//!   lib/util/extensions.dart         — extension StringX + top-level slugify function
//!   lib/models/contact.g.dart        — GENERATED (must produce zero chunks)
//!   lib/main.dart                    — void main() + class MyApp

use callimachus_adapter_code::CodeAdapter;
use callimachus_core::adapter::{DiscoveredSource, SourceAdapter};
use std::path::PathBuf;

fn flutter_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("flutter_project")
}

fn flutter_source(corpus_id: &str) -> DiscoveredSource {
    DiscoveredSource {
        path: flutter_fixture_dir().to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({
            "corpus_id": corpus_id,
            "no_git_filter": true,
        }),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn chunks_for_file<'a>(
    chunks: &'a [callimachus_core::types::Chunk],
    file_suffix: &str,
) -> Vec<&'a callimachus_core::types::Chunk> {
    chunks
        .iter()
        .filter(|c| c.location.uri().contains(file_suffix))
        .collect()
}

// ── No panics ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn full_flutter_fixture_chunk_does_not_panic() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let result = adapter.chunk(&source).await;
    assert!(
        result.is_ok(),
        "chunk() must not error on the full Flutter fixture corpus: {:?}",
        result.err()
    );

    let chunks = result.unwrap();
    // 6 hand-written .dart files → at least 6 file chunks.
    // (lib/models/contact.g.dart is generated and must be excluded.)
    assert!(
        chunks.len() >= 6,
        "flutter fixture has 6 hand-written dart files; expected ≥6 chunks, got {}",
        chunks.len()
    );
}

#[tokio::test]
async fn extract_structure_does_not_panic_for_any_flutter_chunk() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
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

// ── Generated file exclusion ──────────────────────────────────────────────────

#[tokio::test]
async fn generated_dart_file_produces_zero_chunks() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    // contact.g.dart is a generated file and must be completely excluded.
    for chunk in &chunks {
        let uri = chunk.location.uri();
        assert!(
            !uri.contains(".g.dart"),
            "generated .g.dart file must produce zero chunks; got URI: {uri}"
        );
    }
}

// ── contact.dart ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn contact_dart_produces_file_and_class_chunks() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let contact_chunks = chunks_for_file(&chunks, "domain/contact.dart");
    assert!(
        !contact_chunks.is_empty(),
        "contact.dart should produce at least one chunk"
    );

    // File chunk exists.
    assert!(
        contact_chunks.iter().any(|c| c.kind == "file"),
        "contact.dart should have a file-level chunk"
    );

    // Class item chunk with #Contact symbol.
    assert!(
        contact_chunks
            .iter()
            .any(|c| c.kind == "class" && c.location.uri().contains("#Contact")),
        "contact.dart should have a class chunk with #Contact fragment; item chunks: {:?}",
        contact_chunks
            .iter()
            .filter(|c| c.kind != "file")
            .map(|c| format!("{}:{}", c.kind, c.location.uri()))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn contact_dart_structure_has_class_entity() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("domain/contact.dart"))
        .expect("contact.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let class_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "class" && e.canonical_name == "Contact");
    assert!(
        class_entity.is_some(),
        "should extract class entity 'Contact'; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn contact_dart_structure_has_method_entities() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("domain/contact.dart"))
        .expect("contact.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let method_names: Vec<&str> = structure
        .structural_entities
        .iter()
        .filter(|e| e.kind == "method")
        .map(|e| e.canonical_name.as_str())
        .collect();

    // Regular method.
    assert!(
        method_names.contains(&"toJson"),
        "should have method 'toJson'; method entities: {:?}",
        method_names
    );

    // Named constructor.
    assert!(
        method_names.contains(&"fromJson"),
        "should have named constructor 'fromJson' as a method entity; method entities: {:?}",
        method_names
    );

    // Factory constructor.
    assert!(
        method_names.contains(&"empty"),
        "should have factory constructor 'empty' as a method entity; method entities: {:?}",
        method_names
    );
}

#[tokio::test]
async fn contact_dart_structure_has_import_edges() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("domain/contact.dart"))
        .expect("contact.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let import_edges: Vec<_> = structure
        .structural_edges
        .iter()
        .filter(|e| e.kind == "imports")
        .collect();

    assert!(
        !import_edges.is_empty(),
        "contact.dart should have imports edges"
    );

    // import 'package:meta/meta.dart'
    assert!(
        import_edges.iter().any(|e| e.to_entity_id.contains("meta")),
        "should have imports edge containing 'meta'; edges: {:?}",
        import_edges
            .iter()
            .map(|e| &e.to_entity_id)
            .collect::<Vec<_>>()
    );
}

// ── serializable.dart ─────────────────────────────────────────────────────────

#[tokio::test]
async fn serializable_dart_has_class_and_mixin_entities() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("domain/serializable.dart"))
        .expect("serializable.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    // Abstract class Serializable → kind "class".
    let serializable_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "class" && e.canonical_name == "Serializable");
    assert!(
        serializable_entity.is_some(),
        "should extract class entity 'Serializable'; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );

    // Mixin Timestamped → kind "class" (mixin maps to class).
    let timestamped_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "class" && e.canonical_name == "Timestamped");
    assert!(
        timestamped_entity.is_some(),
        "should extract mixin entity 'Timestamped' as kind=class; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );
}

// ── status.dart ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn status_dart_has_enum_entity() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("domain/status.dart"))
        .expect("status.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let enum_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "enum" && e.canonical_name == "ContactStatus");
    assert!(
        enum_entity.is_some(),
        "should extract enum entity 'ContactStatus'; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );
}

// ── contact_view_model.dart ───────────────────────────────────────────────────

#[tokio::test]
async fn contact_view_model_has_extends_implements_with_edges() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("contact_view_model.dart"))
        .expect("contact_view_model.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    // extends ContactViewModel → StateNotifier
    let extends_state_notifier = structure.structural_edges.iter().any(|e| {
        e.kind == "extends"
            && e.from_entity_id.contains("contactviewmodel")
            && e.to_entity_id.contains("statenotifier")
    });
    assert!(
        extends_state_notifier,
        "should have extends edge ContactViewModel → StateNotifier; edges: {:?}",
        structure
            .structural_edges
            .iter()
            .filter(|e| e.kind == "extends")
            .map(|e| format!("{} → {}", e.from_entity_id, e.to_entity_id))
            .collect::<Vec<_>>()
    );

    // with Timestamped → implements edge (mixin application maps to implements)
    let with_timestamped = structure.structural_edges.iter().any(|e| {
        e.kind == "implements"
            && e.from_entity_id.contains("contactviewmodel")
            && e.to_entity_id.contains("timestamped")
    });
    assert!(
        with_timestamped,
        "should have implements edge ContactViewModel → Timestamped (from `with`); edges: {:?}",
        structure
            .structural_edges
            .iter()
            .filter(|e| e.kind == "implements")
            .map(|e| format!("{} → {}", e.from_entity_id, e.to_entity_id))
            .collect::<Vec<_>>()
    );

    // implements Disposable
    let implements_disposable = structure.structural_edges.iter().any(|e| {
        e.kind == "implements"
            && e.from_entity_id.contains("contactviewmodel")
            && e.to_entity_id.contains("disposable")
    });
    assert!(
        implements_disposable,
        "should have implements edge ContactViewModel → Disposable; edges: {:?}",
        structure
            .structural_edges
            .iter()
            .filter(|e| e.kind == "implements")
            .map(|e| format!("{} → {}", e.from_entity_id, e.to_entity_id))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn contact_view_model_has_method_entities() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("contact_view_model.dart"))
        .expect("contact_view_model.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let method_names: Vec<&str> = structure
        .structural_entities
        .iter()
        .filter(|e| e.kind == "method")
        .map(|e| e.canonical_name.as_str())
        .collect();

    assert!(
        method_names.contains(&"load"),
        "should have method 'load'; method entities: {:?}",
        method_names
    );
    assert!(
        method_names.contains(&"dispose"),
        "should have method 'dispose'; method entities: {:?}",
        method_names
    );
}

// ── extensions.dart ───────────────────────────────────────────────────────────

#[tokio::test]
async fn extensions_dart_has_extension_and_function_entities() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().contains("util/extensions.dart"))
        .expect("util/extensions.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    // Extension StringX → kind "class".
    let stringx_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "class" && e.canonical_name == "StringX");
    assert!(
        stringx_entity.is_some(),
        "should extract extension 'StringX' as kind=class; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );

    // Top-level function slugify → kind "function".
    let slugify_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "function" && e.canonical_name == "slugify");
    assert!(
        slugify_entity.is_some(),
        "should extract top-level function 'slugify'; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn extensions_dart_has_correct_chunk_uris() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let extension_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.location.uri().contains("util/extensions.dart") && c.kind != "file")
        .collect();

    let uris: Vec<_> = extension_chunks.iter().map(|c| c.location.uri()).collect();

    assert!(
        uris.iter().any(|u| u.contains("#StringX")),
        "should have chunk with #StringX fragment; item URIs: {:?}",
        uris
    );
    assert!(
        uris.iter().any(|u| u.contains("#slugify")),
        "should have chunk with #slugify fragment; item URIs: {:?}",
        uris
    );
}

// ── main.dart ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn main_dart_has_main_function_entity() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let file_chunk = chunks
        .iter()
        .find(|c| c.kind == "file" && c.location.uri().ends_with("main.dart"))
        .expect("main.dart file chunk must exist");

    let structure = adapter.extract_structure(file_chunk).await.unwrap();

    let main_entity = structure
        .structural_entities
        .iter()
        .find(|e| e.kind == "function" && e.canonical_name == "main");
    assert!(
        main_entity.is_some(),
        "should extract function entity 'main'; entities: {:?}",
        structure
            .structural_entities
            .iter()
            .map(|e| format!("{}:{}", e.kind, e.canonical_name))
            .collect::<Vec<_>>()
    );
}

// ── Cross-cutting: entity ID hygiene ─────────────────────────────────────────

#[tokio::test]
async fn all_dart_entity_ids_use_slug_format() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let dart_file_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.kind == "file" && c.location.uri().ends_with(".dart"))
        .collect();

    assert!(
        !dart_file_chunks.is_empty(),
        "should have at least one .dart file chunk"
    );

    for chunk in dart_file_chunks {
        let structure = adapter.extract_structure(chunk).await.unwrap();
        for entity in &structure.structural_entities {
            assert!(
                entity.id.starts_with("flutter:"),
                "Dart entity id '{}' in '{}' should start with 'flutter:'",
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

// ── Item-level extraction confirmed ──────────────────────────────────────────

/// Ensures the fixture produces entities with `#symbol` URIs (item-level extraction),
/// not merely file-level entities.
#[tokio::test]
async fn flutter_fixture_has_item_level_chunks_with_symbol_fragments() {
    let adapter = CodeAdapter::new();
    let source = flutter_source("flutter");
    let chunks = adapter.chunk(&source).await.unwrap();

    let symbol_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.location.uri().contains('#'))
        .collect();

    assert!(
        !symbol_chunks.is_empty(),
        "flutter fixture must produce chunks with #symbol fragments (item-level extraction); \
        all chunk URIs: {:?}",
        chunks.iter().map(|c| c.location.uri()).collect::<Vec<_>>()
    );

    // Confirm class, function, and enum kinds are present.
    let has_class = symbol_chunks.iter().any(|c| c.kind == "class");
    let has_function = symbol_chunks.iter().any(|c| c.kind == "function");
    assert!(
        has_class,
        "should have class-kind item chunks; item kinds: {:?}",
        symbol_chunks.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
    assert!(
        has_function,
        "should have function-kind item chunks; item kinds: {:?}",
        symbol_chunks.iter().map(|c| &c.kind).collect::<Vec<_>>()
    );
}

// ── Real-repo coverage gate (Step 5) ─────────────────────────────────────────
//
// These tests run against the actual employee_app corpus at
// ~/cf-index-staging/employee_app.  They are `#[ignore]`-by-default so that
// normal CI does not require the local corpus.
//
// Run them with:
//   cargo test -p callimachus-adapter-code --test flutter_coverage -- --include-ignored
//
// or selectively:
//   cargo test -p callimachus-adapter-code --test flutter_coverage employee_app -- --include-ignored

fn employee_app_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/hammer".to_string());
    std::path::PathBuf::from(home)
        .join("cf-index-staging")
        .join("employee_app")
}

fn employee_app_source(corpus_id: &str) -> callimachus_core::adapter::DiscoveredSource {
    callimachus_core::adapter::DiscoveredSource {
        path: employee_app_dir().to_string_lossy().to_string(),
        kind: "directory".to_string(),
        meta: serde_json::json!({ "corpus_id": corpus_id }),
    }
}

/// Verify the real employee_app corpus produces a non-zero, item-level entity
/// set — the primary GO/GAPS launch-gate assertion.
#[tokio::test]
#[ignore = "requires ~/cf-index-staging/employee_app (local corpus)"]
async fn employee_app_dart_entity_count_meets_floor() {
    let dir = employee_app_dir();
    assert!(
        dir.exists(),
        "employee_app corpus not found at {}",
        dir.display()
    );

    let adapter = CodeAdapter::new();
    let source = employee_app_source("employee_app");
    let chunks = adapter.chunk(&source).await.expect("chunk must not error");

    // Collect all structural entities from all file chunks.
    let mut entities = Vec::new();
    for chunk in &chunks {
        if chunk.kind != "file" {
            continue;
        }
        if let Ok(structure) = adapter.extract_structure(chunk).await {
            entities.extend(structure.structural_entities);
        }
    }

    let dart_entity_count = entities.len();
    let class_count = entities.iter().filter(|e| e.kind == "class").count();
    let method_count = entities.iter().filter(|e| e.kind == "method").count();
    let function_count = entities.iter().filter(|e| e.kind == "function").count();
    let enum_count = entities.iter().filter(|e| e.kind == "enum").count();

    // Print the GO/GAPS verdict for the PR body.
    let dart_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.location.uri().ends_with(".dart") || c.location.uri().contains(".dart#"))
        .collect();
    eprintln!(
        "\n--- Dart launch-corpus coverage (employee_app, real repo) ---\n\
        GO: chunks {total_chunks}, dart entities {dart_entity_count} \
        (classes {class_count}, methods {method_count}, functions {function_count}, enums {enum_count})\n\
        Item-level extraction confirmed (entities with kinds present: class={class_count} method={method_count} fn={function_count} enum={enum_count})\n\
        Generated .g.dart/.freezed.dart files excluded (checked below)\n\
        ---",
        total_chunks = dart_chunks.len(),
    );

    // Floor: ≥60 Dart entities (conservative; 69 hand-written files, most
    // declare at least one class or function).
    assert!(
        dart_entity_count >= 60,
        "expected ≥60 Dart entities from employee_app; got {dart_entity_count} \
        (classes={class_count}, methods={method_count}, functions={function_count}, enums={enum_count})"
    );

    // Item-level: at least one of each of the main kinds.
    assert!(class_count > 0, "expected class entities; got 0");
    assert!(method_count > 0, "expected method entities; got 0");
}

/// Verify no chunk URI references a generated .g.dart or .freezed.dart file.
#[tokio::test]
#[ignore = "requires ~/cf-index-staging/employee_app (local corpus)"]
async fn employee_app_generated_dart_files_excluded() {
    let dir = employee_app_dir();
    if !dir.exists() {
        eprintln!("skip: employee_app not found at {}", dir.display());
        return;
    }

    let adapter = CodeAdapter::new();
    let source = employee_app_source("employee_app");
    let chunks = adapter.chunk(&source).await.expect("chunk must not error");

    let generated_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| {
            let uri = c.location.uri();
            uri.contains(".g.dart") || uri.contains(".freezed.dart")
        })
        .collect();

    assert!(
        generated_chunks.is_empty(),
        "expected 0 generated-file chunks; got {} — URIs: {:?}",
        generated_chunks.len(),
        generated_chunks
            .iter()
            .map(|c| c.location.uri())
            .collect::<Vec<_>>()
    );
}
