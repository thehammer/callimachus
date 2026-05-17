//! Unit tests for static contract analysis (tree-sitter).
//!
//! Reads the fixture file `tests/fixtures/contract_sample.rs` as raw text
//! and passes individual function snippets through `analyze_rust` to verify
//! that each signal is detected correctly.

use callimachus_adapter_code::contracts::analyze_rust;

const FIXTURE: &str = include_str!("fixtures/contract_sample.rs");

/// Extract a named function and everything until the next `//` section comment.
fn fn_snippet(fn_name: &str) -> String {
    let needle = format!("fn {}(", fn_name);
    let pos = FIXTURE.find(&needle).expect("function not found");
    // Walk backwards to include any attribute lines.
    let pre_start = FIXTURE[..pos].rfind("\n\n").map(|i| i + 2).unwrap_or(0);
    // Walk forward to the end of the block (find the matching `}`).
    let after = &FIXTURE[pos..];
    let mut depth = 0usize;
    let mut end_off = after.len();
    for (i, ch) in after.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                if depth > 0 {
                    depth -= 1;
                }
                if depth == 0 && i > 0 {
                    end_off = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    FIXTURE[pre_start..pos + end_off].to_string()
}

#[test]
fn panic_risk_detected() {
    let src = fn_snippet("risky_function");
    let s = analyze_rust(&src, "risky_function");
    assert!(
        s.has_panic_risk,
        "expected has_panic_risk for risky_function"
    );
    assert!(s.panic_call_count >= 2, "expected at least 2 panic calls");
}

#[test]
fn unsafe_detected() {
    let src = fn_snippet("unsafe_function");
    let s = analyze_rust(&src, "unsafe_function");
    assert!(s.has_unsafe, "expected has_unsafe for unsafe_function");
}

#[test]
fn must_use_detected() {
    let src = fn_snippet("important_value");
    let s = analyze_rust(&src, "important_value");
    assert!(s.is_must_use, "expected is_must_use for important_value");
}

#[test]
fn deprecated_detected() {
    let src = fn_snippet("old_api");
    let s = analyze_rust(&src, "old_api");
    assert!(s.is_deprecated, "expected is_deprecated for old_api");
}

#[test]
fn fallible_method_detected() {
    // Standalone snippet: ensures Result<> return type detection works.
    let src = r#"pub fn fallible_method(&self) -> Result<i32, String> {
    if true { Ok(1) } else { Err("e".to_string()) }
}"#;
    let s = analyze_rust(src, "fallible_method");
    assert!(s.is_fallible, "expected is_fallible for fallible_method");
    assert!(s.is_public, "expected is_public for fallible_method");
}

#[test]
fn nullable_method_detected() {
    let src = r#"pub fn nullable_method(&self) -> Option<i32> {
    Some(1)
}"#;
    let s = analyze_rust(src, "nullable_method");
    assert!(s.is_nullable, "expected is_nullable for nullable_method");
}

#[test]
fn mutating_method_detected() {
    let src = r#"pub fn mutating_method(&mut self) -> i32 {
    self.value += 1;
    self.value
}"#;
    let s = analyze_rust(src, "mutating_method");
    assert!(s.is_mutating, "expected is_mutating for mutating_method");
}

#[test]
fn private_method_not_public() {
    let src = r#"fn private_method(&self) -> i32 {
    self.value
}"#;
    let s = analyze_rust(src, "private_method");
    assert!(
        !s.is_public,
        "private_method should NOT be marked is_public"
    );
}

#[test]
fn debt_markers_detected() {
    let src = fn_snippet("debt_fn");
    let s = analyze_rust(&src, "debt_fn");
    assert!(
        !s.debt_markers.is_empty(),
        "expected debt_markers for debt_fn"
    );
    // All three marker types should appear.
    let joined = s.debt_markers.join("\n").to_uppercase();
    assert!(joined.contains("TODO"), "expected TODO in debt_markers");
    assert!(joined.contains("FIXME"), "expected FIXME in debt_markers");
    assert!(joined.contains("HACK"), "expected HACK in debt_markers");
}

#[test]
fn is_incomplete_via_unimplemented() {
    let src = fn_snippet("not_implemented_yet");
    let s = analyze_rust(&src, "not_implemented_yet");
    assert!(
        s.is_incomplete,
        "expected is_incomplete for not_implemented_yet"
    );
}

#[test]
fn is_incomplete_via_todo() {
    let src = fn_snippet("todo_fn");
    let s = analyze_rust(&src, "todo_fn");
    assert!(s.is_incomplete, "expected is_incomplete for todo_fn");
}

#[test]
fn test_attribute_detected() {
    let src = fn_snippet("test_fallible_method");
    let s = analyze_rust(&src, "test_fallible_method");
    assert!(s.is_test, "expected is_test for test_fallible_method");
}

#[test]
fn diverging_detected() {
    // Explicit snippet: tree-sitter never_type detection.
    let src = r#"pub fn diverging_fn() -> ! {
    panic!("always panics")
}"#;
    let s = analyze_rust(src, "diverging_fn");
    assert!(s.is_diverging, "expected is_diverging for diverging_fn");
}

#[test]
fn discarded_calls_detected() {
    let src = fn_snippet("discards_result_fn");
    let s = analyze_rust(&src, "discards_result_fn");
    assert!(
        !s.discarded_calls.is_empty(),
        "expected discarded_calls for discards_result_fn"
    );
}

#[test]
fn branch_count_nonzero_for_branchy_fn() {
    let src = fn_snippet("branchy_fn");
    let s = analyze_rust(&src, "branchy_fn");
    assert!(
        s.branch_count >= 2,
        "expected at least 2 branches in branchy_fn"
    );
}

#[test]
fn non_rust_language_returns_default() {
    use callimachus_adapter_code::contracts::analyze;
    let s = analyze("python", "def foo(): pass", "foo");
    assert!(!s.is_public);
    assert!(!s.is_fallible);
    assert_eq!(s.panic_call_count, 0);
}
