//! Static contract analysis for source code.
//!
//! Uses tree-sitter to extract boolean signals from source code without any
//! LLM calls.  These signals feed into the LLM prompt as structured context,
//! dramatically reducing prompt length and improving consistency.

use tree_sitter::{Language, Parser, Query, QueryCursor};

/// All signals extracted from a single code entity via static analysis.
#[derive(Debug, Default, Clone)]
pub struct ContractSignals {
    /// Entity is publicly visible (`pub` keyword at function/impl level).
    pub is_public: bool,
    /// Entity has `#[must_use]` attribute.
    pub is_must_use: bool,
    /// Entity has `#[deprecated]` attribute.
    pub is_deprecated: bool,
    /// Return type mentions `Result<`.
    pub is_fallible: bool,
    /// Return type mentions `Option<`.
    pub is_nullable: bool,
    /// First parameter is `&mut self`.
    pub is_mutating: bool,
    /// Return type is the never type `!`.
    pub is_diverging: bool,
    /// Body calls `.unwrap()` or `.expect(…)`.
    pub has_panic_risk: bool,
    /// Body contains an `unsafe` block.
    pub has_unsafe: bool,
    /// Body contains `todo!()` or `unimplemented!()`.
    pub is_incomplete: bool,
    /// Count of `.unwrap()` / `.expect(…)` calls.
    pub panic_call_count: u32,
    /// Lines containing FIXME / HACK / TODO comments.
    pub debt_markers: Vec<String>,
    /// Entity has `#[test]` or `#[tokio::test]` attribute.
    pub is_test: bool,
    /// Number of control-flow branches (if/match/loop/for/while).
    pub branch_count: u32,
    /// Approximate body line count (end_row - start_row).
    pub body_lines: u32,
    /// Call expressions whose result is immediately discarded (let _ = …).
    pub discarded_calls: Vec<String>,
    /// All identifier names referenced in the body (for test→prod linking).
    pub referenced_names: Vec<String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Analyse a Rust source fragment (a single function / impl / struct body)
/// and return a `ContractSignals`.  `entity_name` is used only for debug
/// output; it doesn't affect the analysis.
pub fn analyze_rust(source_code: &str, _entity_name: &str) -> ContractSignals {
    let language: Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return ContractSignals::default();
    }

    let tree = match parser.parse(source_code, None) {
        Some(t) => t,
        None => return ContractSignals::default(),
    };

    let root = tree.root_node();
    let mut signals = ContractSignals::default();

    // ── Pass 1: scan top-level attributes ─────────────────────────────────────
    let attr_query_src = r#"
        (attribute_item
            (attribute
                (identifier) @attr_name))
    "#;
    if let Ok(q) = Query::new(&language, attr_query_src) {
        let mut cursor = QueryCursor::new();
        for m in cursor.matches(&q, root, source_code.as_bytes()) {
            for cap in m.captures {
                let name = cap.node.utf8_text(source_code.as_bytes()).unwrap_or("");
                match name {
                    "must_use" => signals.is_must_use = true,
                    "deprecated" => signals.is_deprecated = true,
                    "test" => signals.is_test = true,
                    _ => {}
                }
            }
        }
    }

    // Check for `#[tokio::test]` separately via scoped path.
    if source_code.contains("#[tokio::test]") || source_code.contains("# [tokio::test]") {
        signals.is_test = true;
    }

    // ── Pass 2: function signature signals ────────────────────────────────────
    // Note: `visibility_modifier` is NOT a named field in tree-sitter-rust's
    // function_item grammar, so it must be matched as an anonymous child.
    // We use separate queries for clarity.

    // 2a. Visibility: any function_item that has a visibility_modifier child.
    let vis_query_src = r#"(function_item (visibility_modifier) @vis)"#;
    if let Ok(q) = Query::new(&language, vis_query_src) {
        let mut cursor = QueryCursor::new();
        if cursor
            .matches(&q, root, source_code.as_bytes())
            .next()
            .is_some()
        {
            signals.is_public = true;
        }
    }

    // 2b. Parameters (named field) and return type (named field).
    let fn_query_src = r#"
        (function_item
            parameters: (parameters) @params
            return_type: (_)? @ret_type)
    "#;
    if let Ok(q) = Query::new(&language, fn_query_src) {
        let params_idx = q.capture_index_for_name("params");
        let ret_idx = q.capture_index_for_name("ret_type");

        let mut cursor = QueryCursor::new();
        for m in cursor.matches(&q, root, source_code.as_bytes()) {
            // Parameters — look for `&mut self`.
            if let Some(cap) = params_idx.and_then(|i| m.captures.iter().find(|c| c.index == i)) {
                let params_text = cap.node.utf8_text(source_code.as_bytes()).unwrap_or("");
                if params_text.contains("&mut self") {
                    signals.is_mutating = true;
                }
            }

            // Return type.
            if let Some(cap) = ret_idx.and_then(|i| m.captures.iter().find(|c| c.index == i)) {
                let ret_text = cap.node.utf8_text(source_code.as_bytes()).unwrap_or("");
                if ret_text.contains("Result<") || ret_text.contains("Result <") {
                    signals.is_fallible = true;
                }
                if ret_text.contains("Option<") || ret_text.contains("Option <") {
                    signals.is_nullable = true;
                }
                // Never type: `-> !`
                if ret_text.trim() == "!" {
                    signals.is_diverging = true;
                }
            }
        }
    }

    // ── Pass 3: panic calls (unwrap / expect) ─────────────────────────────────
    let panic_query_src = r#"
        (call_expression
            function: (field_expression
                field: (field_identifier) @method)
            (#match? @method "^(unwrap|expect)$"))
    "#;
    if let Ok(q) = Query::new(&language, panic_query_src) {
        let mut cursor = QueryCursor::new();
        let count = cursor.matches(&q, root, source_code.as_bytes()).count() as u32;
        signals.panic_call_count = count;
        signals.has_panic_risk = count > 0;
    }

    // ── Pass 4: unsafe blocks ─────────────────────────────────────────────────
    let unsafe_query_src = r#"(unsafe_block) @ub"#;
    if let Ok(q) = Query::new(&language, unsafe_query_src) {
        let mut cursor = QueryCursor::new();
        signals.has_unsafe = cursor
            .matches(&q, root, source_code.as_bytes())
            .next()
            .is_some();
    }

    // ── Pass 5: todo! / unimplemented! ────────────────────────────────────────
    let incomplete_query_src = r#"
        (macro_invocation
            macro: (identifier) @m
            (#match? @m "^(todo|unimplemented)$"))
    "#;
    if let Ok(q) = Query::new(&language, incomplete_query_src) {
        let mut cursor = QueryCursor::new();
        signals.is_incomplete = cursor
            .matches(&q, root, source_code.as_bytes())
            .next()
            .is_some();
    }

    // ── Pass 6: control-flow branch count ────────────────────────────────────
    let branch_query_src = r#"
        [(if_expression) (match_expression) (while_expression)
         (for_expression) (loop_expression)] @branch
    "#;
    if let Ok(q) = Query::new(&language, branch_query_src) {
        let mut cursor = QueryCursor::new();
        signals.branch_count = cursor.matches(&q, root, source_code.as_bytes()).count() as u32;
    }

    // ── Pass 7: body line count ───────────────────────────────────────────────
    // Use the block of the outermost function item as the body span.
    let block_query_src = r#"(function_item body: (block) @body)"#;
    if let Ok(q) = Query::new(&language, block_query_src) {
        let mut cursor = QueryCursor::new();
        if let Some(cap) = cursor
            .matches(&q, root, source_code.as_bytes())
            .next()
            .and_then(|m| m.captures.first().copied())
        {
            let start = cap.node.start_position().row;
            let end = cap.node.end_position().row;
            signals.body_lines = (end.saturating_sub(start)) as u32;
        }
    }

    // ── Pass 8: `let _ = call(...)` discarded results ────────────────────────
    // Simple heuristic: scan source text for `let _ =`.
    for line in source_code.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("let _ =") || trimmed.starts_with("let _=") {
            // Extract approximate callee name.
            if let Some(callee) = extract_callee_from_let_discard(trimmed) {
                signals.discarded_calls.push(callee);
            }
        }
        // `.ok()` result discarded (ignore returned value of Result).
        if trimmed.contains(".ok();") || trimmed.contains(".ok()?) {") {
            // best-effort: grab identifier before .
            if let Some(name) = extract_method_receiver(trimmed) {
                signals.discarded_calls.push(name);
            }
        }
    }

    // ── Pass 9: debt marker comments ─────────────────────────────────────────
    for line in source_code.lines() {
        let upper = line.to_uppercase();
        if upper.contains("FIXME")
            || upper.contains("HACK")
            || (upper.contains("TODO") && upper.contains("//"))
        {
            signals.debt_markers.push(line.trim().to_string());
        }
    }

    // ── Pass 10: referenced names (for test functions) ───────────────────────
    if signals.is_test {
        let ident_query_src = r#"(identifier) @id"#;
        if let Ok(q) = Query::new(&language, ident_query_src) {
            let mut cursor = QueryCursor::new();
            let mut names: Vec<String> = cursor
                .matches(&q, root, source_code.as_bytes())
                .flat_map(|m| {
                    m.captures.iter().map(|c| {
                        c.node
                            .utf8_text(source_code.as_bytes())
                            .unwrap_or("")
                            .to_string()
                    })
                })
                .filter(|n| n.len() > 2 && !is_keyword(n))
                .collect();
            names.sort();
            names.dedup();
            signals.referenced_names = names;
        }
    }

    signals
}

/// Dispatch to the right language analyser. Non-Rust returns default.
pub fn analyze(language: &str, source_code: &str, entity_name: &str) -> ContractSignals {
    if language == "rust" {
        analyze_rust(source_code, entity_name)
    } else {
        // TODO(phase-13): non-Rust static analysis
        ContractSignals::default()
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn extract_callee_from_let_discard(line: &str) -> Option<String> {
    // `let _ = foo(...)` or `let _ = obj.foo(...)`
    let after_eq = line.split_once('=')?.1.trim();
    let name = after_eq
        .split('(')
        .next()?
        .trim()
        .rsplit('.')
        .next()?
        .to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn extract_method_receiver(line: &str) -> Option<String> {
    // `foo.ok()` → "foo"
    let before_dot = line.split('.').next()?.trim();
    // Strip leading whitespace / `let …` prefix.
    let name = before_dot
        .rsplit_once(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|(_, r)| r)
        .unwrap_or(before_dot)
        .to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "union"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "yield"
    )
}
