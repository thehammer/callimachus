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
        calls_to_helper[0].occurrence_count,
        4,
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

    let prod_edge = result
        .edges
        .iter()
        .find(|e| {
            e.kind == "calls"
                && e.to_entity_id.contains("shared_helper")
                && e.origin_scope == "production"
        });

    let test_edge = result
        .edges
        .iter()
        .find(|e| {
            e.kind == "calls"
                && e.to_entity_id.contains("shared_helper")
                && e.origin_scope == "test"
        });

    assert!(
        prod_edge.is_some(),
        "should have a calls edge to shared_helper with origin_scope=production; \
         edges present: {:?}",
        result
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .map(|e| format!(
                "to={} scope={}",
                e.to_entity_id, e.origin_scope
            ))
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
            .map(|e| format!(
                "to={} scope={}",
                e.to_entity_id, e.origin_scope
            ))
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
            e.kind == "calls"
                && e.to_entity_id.contains("setup_data")
                && e.origin_scope == "test"
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
            .map(|e| format!(
                "to={} scope={}",
                e.to_entity_id, e.origin_scope
            ))
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
