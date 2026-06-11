/// Edge hygiene integration tests.
///
/// Covers occurrence_count aggregation and origin_scope detection, the two
/// behaviours added in the edge-dedup-and-origin-scope feature branch.
///
/// These tests operate on `extract_structure` directly — no LLM, no storage.
/// They are behavioural: they assert on what comes out, not on how the
/// internal hash is computed.
use callimachus_adapter_code::extractor::extract_structure;
use callimachus_core::types::{Chunk, Location};

fn make_chunk(corpus_id: &str, path: &str, kind: &str, content: &str) -> Chunk {
    Chunk::new(
        corpus_id.to_string(),
        None,
        kind.to_string(),
        Location::new(corpus_id, path),
        content.to_string(),
    )
}

fn rust_lang() -> &'static callimachus_adapter_code::languages::LangConfig {
    callimachus_adapter_code::languages::for_extension("rs").unwrap()
}

// ── Test A: occurrence_count from multiple call sites ────────────────────────

/// N call sites to the same function collapse into exactly one `calls` edge
/// whose `occurrence_count` equals N.
#[test]
fn repeated_calls_collapse_to_single_edge_with_correct_count() {
    let source = r#"
fn caller() {
    helper();
    helper();
    helper();
    helper();
}
fn helper() {}
"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let calls_to_helper: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == "calls" && e.to_entity_id.contains("helper"))
        .collect();

    assert_eq!(
        calls_to_helper.len(),
        1,
        "four call sites to helper should collapse into exactly one calls edge; \
         got {} edges: {:?}",
        calls_to_helper.len(),
        calls_to_helper
            .iter()
            .map(|e| format!("id={} count={}", e.id, e.occurrence_count))
            .collect::<Vec<_>>()
    );

    assert_eq!(
        calls_to_helper[0].occurrence_count, 4,
        "occurrence_count should be 4 (one per call site)"
    );
}

/// Extracting the same source twice produces identical edge IDs — the IDs are
/// deterministic over (from, kind, to, origin_scope), not random.
#[test]
fn edge_ids_are_deterministic_across_extractions() {
    let source = r#"
fn caller() {
    helper();
    helper();
    helper();
}
fn helper() {}
"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let lang = rust_lang();

    let result_a = extract_structure(&chunk, lang).unwrap();
    let result_b = extract_structure(&chunk, lang).unwrap();

    let ids_a: std::collections::BTreeSet<_> = result_a.edges.iter().map(|e| &e.id).collect();
    let ids_b: std::collections::BTreeSet<_> = result_b.edges.iter().map(|e| &e.id).collect();

    assert_eq!(
        ids_a, ids_b,
        "edge IDs must be identical across two extractions of the same source"
    );
}

// ── Test B: origin_scope from #[cfg(test)] module ────────────────────────────

/// A call made from ordinary production code gets `origin_scope = "production"`.
/// A call to the same callee made from inside `#[cfg(test)] mod tests` gets
/// `origin_scope = "test"`.  They are distinct edges (different IDs, different rows).
#[test]
fn cfg_test_mod_calls_tagged_as_test_scope() {
    let source = r#"
fn prod_caller() {
    shared_helper();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        shared_helper();
    }
}

fn shared_helper() {}
"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let prod_edge = result.edges.iter().find(|e| {
        e.kind == "calls"
            && e.to_entity_id.contains("shared_helper")
            && e.origin_scope == "production"
    });

    let test_edge = result.edges.iter().find(|e| {
        e.kind == "calls" && e.to_entity_id.contains("shared_helper") && e.origin_scope == "test"
    });

    assert!(
        prod_edge.is_some(),
        "should have a calls edge to shared_helper with origin_scope=production; \
         edges present: {:?}",
        result
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .map(|e| format!("to={} scope={}", e.to_entity_id, e.origin_scope))
            .collect::<Vec<_>>()
    );

    assert!(
        test_edge.is_some(),
        "should have a calls edge to shared_helper with origin_scope=test; \
         edges present: {:?}",
        result
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .map(|e| format!("to={} scope={}", e.to_entity_id, e.origin_scope))
            .collect::<Vec<_>>()
    );

    // They must be distinct edges — different IDs.
    assert_ne!(
        prod_edge.unwrap().id,
        test_edge.unwrap().id,
        "production and test edges to the same callee must have different IDs"
    );
}

// ── Test C: #[test] function scope detection ─────────────────────────────────

/// A function annotated with `#[test]` (not inside a cfg(test) mod) produces
/// edges tagged `origin_scope = "test"`.
#[test]
fn test_attr_on_function_tags_edges_as_test_scope() {
    let source = r#"
fn setup_data() {}

#[test]
fn my_standalone_test() {
    setup_data();
}
"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let calls_from_test: Vec<_> = result
        .edges
        .iter()
        .filter(|e| {
            e.kind == "calls" && e.to_entity_id.contains("setup_data") && e.origin_scope == "test"
        })
        .collect();

    assert!(
        !calls_from_test.is_empty(),
        "calls from inside a #[test] fn should have origin_scope=test; \
         calls edges present: {:?}",
        result
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .map(|e| format!("to={} scope={}", e.to_entity_id, e.origin_scope))
            .collect::<Vec<_>>()
    );
}

/// Calls from outside any test annotation remain `origin_scope = "production"`.
#[test]
fn non_test_function_calls_tagged_as_production_scope() {
    let source = r#"
fn helper() {}

fn normal_fn() {
    helper();
}
"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let call_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == "calls" && e.to_entity_id.contains("helper"))
        .collect();

    assert!(
        !call_edges.is_empty(),
        "should have at least one calls edge to helper"
    );

    for edge in &call_edges {
        assert_eq!(
            edge.origin_scope, "production",
            "call from a non-test function should have origin_scope=production, \
             got: {}",
            edge.origin_scope
        );
    }
}

// ── Test D: glob-import decomposition (PR 3) ─────────────────────────────────

/// A grouped Rust use-tree `use a::{B, C, D}` must yield **three** `imports`
/// edges — one per imported name — not a single mega-entity for the entire
/// use-tree.  Each entity's canonical name must be the full qualified path
/// (e.g. `crate::storage::B`), not underscore-soup encoding multiple names.
#[test]
fn grouped_use_tree_decomposes_into_one_edge_per_leaf() {
    let source = r#"use crate::storage::{A, B, C};"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let import_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == "imports")
        .collect();

    assert_eq!(
        import_edges.len(),
        3,
        "grouped import `use crate::storage::{{A, B, C}}` should yield 3 import \
         edges (one per leaf); got {} edges with targets: {:?}",
        import_edges.len(),
        import_edges
            .iter()
            .map(|e| &e.to_entity_id)
            .collect::<Vec<_>>()
    );

    // Each target entity must have a clean, single-name canonical path.
    let leaf_names: Vec<String> = import_edges
        .iter()
        .filter_map(|e| result.entities.iter().find(|ent| ent.id == e.to_entity_id))
        .map(|ent| ent.canonical_name.clone())
        .collect();

    assert!(
        leaf_names
            .iter()
            .any(|n| n == "crate::storage::A" || n.ends_with("::A")),
        "should have an import entity for A; found: {:?}",
        leaf_names
    );
    assert!(
        leaf_names
            .iter()
            .any(|n| n == "crate::storage::B" || n.ends_with("::B")),
        "should have an import entity for B; found: {:?}",
        leaf_names
    );
    assert!(
        leaf_names
            .iter()
            .any(|n| n == "crate::storage::C" || n.ends_with("::C")),
        "should have an import entity for C; found: {:?}",
        leaf_names
    );

    // No entity canonical name should encode multiple imported names (underscore soup).
    for name in &leaf_names {
        // A multi-name blob would look like `crate__storage_____a___b___c` or contain
        // brace-like delimiters after slugification.  The simplest guard: the
        // canonical name must not contain `{` or `}` and must end with a single
        // identifier segment (no `,` in the last segment).
        assert!(
            !name.contains('{') && !name.contains('}') && !name.contains(','),
            "entity canonical name looks like underscore soup / multi-name blob: {:?}",
            name
        );
    }
}

/// A plain (non-grouped) `use crate::storage::Foo;` still yields exactly one
/// import edge and its entity has the full qualified path as its canonical name.
#[test]
fn plain_import_yields_single_edge_with_qualified_path() {
    let source = r#"use crate::storage::Foo;"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let import_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == "imports")
        .collect();

    assert_eq!(
        import_edges.len(),
        1,
        "plain `use crate::storage::Foo` should yield exactly 1 import edge; \
         got {} edges",
        import_edges.len()
    );

    let entity = result
        .entities
        .iter()
        .find(|e| e.id == import_edges[0].to_entity_id)
        .expect("import entity should exist in result");

    assert_eq!(
        entity.canonical_name, "crate::storage::Foo",
        "entity canonical name should be the full qualified path"
    );
}

/// Nested grouped use-trees decompose correctly to their full leaf paths.
/// `use std::{fmt::Display, io::{Read, Write}};` → 3 edges.
#[test]
fn nested_grouped_use_tree_decomposes_recursively() {
    let source = r#"use std::{fmt::Display, io::{Read, Write}};"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let import_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == "imports")
        .collect();

    assert_eq!(
        import_edges.len(),
        3,
        "nested grouped use-tree should yield 3 import edges; got {} with targets: {:?}",
        import_edges.len(),
        import_edges
            .iter()
            .map(|e| &e.to_entity_id)
            .collect::<Vec<_>>()
    );

    let leaf_names: Vec<String> = import_edges
        .iter()
        .filter_map(|e| result.entities.iter().find(|ent| ent.id == e.to_entity_id))
        .map(|ent| ent.canonical_name.clone())
        .collect();

    assert!(
        leaf_names.iter().any(|n| n == "std::fmt::Display"),
        "should have std::fmt::Display; found: {:?}",
        leaf_names
    );
    assert!(
        leaf_names.iter().any(|n| n == "std::io::Read"),
        "should have std::io::Read; found: {:?}",
        leaf_names
    );
    assert!(
        leaf_names.iter().any(|n| n == "std::io::Write"),
        "should have std::io::Write; found: {:?}",
        leaf_names
    );
}

/// Wildcard imports `use foo::*;` emit a single `foo::*` edge.
#[test]
fn wildcard_import_emits_single_star_edge() {
    let source = r#"use std::io::*;"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    let import_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == "imports")
        .collect();

    assert_eq!(
        import_edges.len(),
        1,
        "wildcard import should yield 1 edge; got {}",
        import_edges.len()
    );

    let entity = result
        .entities
        .iter()
        .find(|e| e.id == import_edges[0].to_entity_id)
        .expect("import entity should exist");

    assert!(
        entity.canonical_name.ends_with("::*"),
        "wildcard import entity should have canonical name ending with `::*`; \
         got: {}",
        entity.canonical_name
    );
}

/// Imports inside a `#[cfg(test)] mod` are tagged `origin_scope = "test"`.
#[test]
fn grouped_import_inside_cfg_test_mod_tagged_as_test_scope() {
    let source = r#"
use crate::production::Service;

#[cfg(test)]
mod tests {
    use crate::storage::{A, B};
}
"#;

    let chunk = make_chunk("corp", "src/lib.rs", "file", source);
    let result = extract_structure(&chunk, rust_lang()).unwrap();

    // Production import
    let prod_import: Vec<_> = result
        .edges
        .iter()
        .filter(|e| {
            e.kind == "imports"
                && e.to_entity_id.contains("service")
                && e.origin_scope == "production"
        })
        .collect();

    assert!(
        !prod_import.is_empty(),
        "import of Service should be production-scoped; all import edges: {:?}",
        result
            .edges
            .iter()
            .filter(|e| e.kind == "imports")
            .map(|e| format!("to={} scope={}", e.to_entity_id, e.origin_scope))
            .collect::<Vec<_>>()
    );

    // Test imports — both A and B should be test-scoped
    let test_imports: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == "imports" && e.origin_scope == "test")
        .collect();

    assert_eq!(
        test_imports.len(),
        2,
        "imports of A and B inside #[cfg(test)] mod should both be test-scoped; \
         test import edges: {:?}",
        test_imports
            .iter()
            .map(|e| format!("to={} scope={}", e.to_entity_id, e.origin_scope))
            .collect::<Vec<_>>()
    );
}
