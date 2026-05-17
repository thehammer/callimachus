use anyhow::Result;
use callimachus_core::types::{Chunk, Edge, Entity};
use std::collections::HashSet;
use tree_sitter::{Parser, Query, QueryCursor};
use uuid::Uuid;

use crate::languages::LangConfig;

// ── Deterministic ID helper ───────────────────────────────────────────────────

/// Build a deterministic, corpus-scoped entity ID from a symbol name.
///
/// Slugifies to lowercase, keeping alphanumeric characters and underscores,
/// and replacing everything else with `_`.  The result is
/// `{corpus_id}:{slug}`.
fn entity_id(corpus_id: &str, name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{corpus_id}:{slug}")
}

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct ExtractedCodeStructure {
    /// Functions, classes, interfaces, modules, imports.
    pub entities: Vec<Entity>,
    /// calls, imports, extends, implements, defines.
    pub edges: Vec<Edge>,
    /// Leading doc comment or docstring for the chunk's root symbol.
    pub doc_comment: Option<String>,
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Extract entities and edges from a chunk using tree-sitter.
/// No LLM calls — purely structural.
pub fn extract_structure(
    chunk: &Chunk,
    lang_config: &LangConfig,
) -> Result<ExtractedCodeStructure> {
    let mut result = ExtractedCodeStructure::default();

    let mut parser = Parser::new();
    let language = (lang_config.language_fn)();
    parser
        .set_language(&language)
        .map_err(|e| anyhow::anyhow!("set_language error: {e:?}"))?;

    let tree = match parser.parse(&chunk.content, None) {
        Some(t) => t,
        None => return Ok(result),
    };

    let root = tree.root_node();
    let source = chunk.content.as_bytes();

    // Extract top-level named entities.
    extract_entities(chunk, root, source, lang_config, &mut result);

    // Extract call edges.
    extract_calls(chunk, root, source, lang_config, &mut result);

    // Extract import edges.
    extract_imports(chunk, root, source, lang_config, &mut result);

    // Extract doc comment for the top-level symbol.
    result.doc_comment = extract_doc_comment(root, source, lang_config.name);

    Ok(result)
}

// ── Captured node data ────────────────────────────────────────────────────────

/// Owned data extracted from a query capture (avoids lifetime issues with QueryCursor).
struct CapturedNode {
    byte_range: std::ops::Range<usize>,
    kind: String,
    start_row: usize,
}

fn capture_nodes(
    source: &[u8],
    lang_config: &LangConfig,
    root: tree_sitter::Node<'_>,
    query_str: &str,
) -> Vec<CapturedNode> {
    let language = (lang_config.language_fn)();
    let query = match Query::new(&language, query_str) {
        Ok(q) => q,
        Err(_) => return vec![],
    };
    let mut cursor = QueryCursor::new();
    cursor
        .matches(&query, root, source)
        .flat_map(|m| {
            m.captures
                .iter()
                .map(|c| CapturedNode {
                    byte_range: c.node.byte_range(),
                    kind: c.node.kind().to_string(),
                    start_row: c.node.start_position().row,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

// ── Entity extraction ─────────────────────────────────────────────────────────

fn extract_entities(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    result: &mut ExtractedCodeStructure,
) {
    let nodes = capture_nodes(source, lang, root, lang.top_level_query);

    // Strip any '#fragment' to get the bare file path.
    let file_path = chunk
        .location
        .path
        .split('#')
        .next()
        .unwrap_or(&chunk.location.path)
        .to_string();
    let file_entity_id = entity_id(&chunk.corpus_id, &file_path);

    // Track which file entities we've already emitted to avoid duplicates.
    let mut emitted_file_entities: HashSet<String> = HashSet::new();

    for node in &nodes {
        let text = match std::str::from_utf8(source.get(node.byte_range.clone()).unwrap_or(&[])) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let kind = ts_node_to_entity_kind(&node.kind);
        let name = extract_name_from_text(text, &node.kind)
            .unwrap_or_else(|| format!("anonymous_{}", node.start_row + 1));

        let sym_entity_id = entity_id(&chunk.corpus_id, &name);
        let mut entity = Entity::new(
            sym_entity_id.clone(),
            chunk.corpus_id.clone(),
            name.clone(),
            kind.to_string(),
        );
        entity.first_location = Some(chunk.location.clone());
        entity.last_location = Some(chunk.location.clone());
        entity.appearance_count = 1;
        entity.confidence = 0.9;

        // Ensure the file entity exists so the defines edge FK resolves.
        if emitted_file_entities.insert(file_entity_id.clone()) {
            let mut file_entity = Entity::new(
                file_entity_id.clone(),
                chunk.corpus_id.clone(),
                file_path.clone(),
                "file".to_string(),
            );
            file_entity.first_location = Some(chunk.location.clone());
            file_entity.last_location = Some(chunk.location.clone());
            file_entity.confidence = 1.0;
            result.entities.push(file_entity);
        }

        // Emit a "defines" edge: this file defines the symbol.
        let edge_id = format!("{}-{}", chunk.corpus_id, Uuid::new_v4());
        let defines_edge = Edge::new(
            edge_id,
            chunk.corpus_id.clone(),
            file_entity_id.clone(),
            sym_entity_id,
            "defines".to_string(),
            chunk.location.clone(),
        );
        result.edges.push(defines_edge);
        result.entities.push(entity);
    }

    // Also extract extends/implements for class-like nodes.
    extract_inheritance(chunk, root, source, lang, result);
}

fn extract_inheritance(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    result: &mut ExtractedCodeStructure,
) {
    let patterns: &[(&str, &str)] = match lang.name {
        "typescript" | "javascript" => &[
            (
                r#"(class_declaration name: (type_identifier) @class (class_heritage (extends_clause (identifier) @parent)))"#,
                "extends",
            ),
            (
                r#"(class_declaration name: (type_identifier) @class (class_heritage (implements_clause (type_identifier) @iface)))"#,
                "implements",
            ),
        ],
        "python" => &[(
            r#"(class_definition name: (identifier) @class (argument_list (identifier) @parent))"#,
            "extends",
        )],
        _ => &[],
    };

    for (query_str, edge_kind) in patterns {
        let language = (lang.language_fn)();
        let query = match Query::new(&language, query_str) {
            Ok(q) => q,
            Err(_) => continue,
        };

        // Collect pairs of (from, to) text ranges.
        let pairs: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = {
            let mut cursor = QueryCursor::new();
            cursor
                .matches(&query, root, source)
                .filter_map(|m| {
                    if m.captures.len() >= 2 {
                        Some((
                            m.captures[0].node.byte_range(),
                            m.captures[1].node.byte_range(),
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        };

        for (from_range, to_range) in pairs {
            let from = text_from_bytes(source, from_range);
            let to = text_from_bytes(source, to_range);
            if from.is_empty() || to.is_empty() {
                continue;
            }
            let from_id = entity_id(&chunk.corpus_id, &from);
            let to_id = entity_id(&chunk.corpus_id, &to);
            let edge_id = format!("{}-{}", chunk.corpus_id, Uuid::new_v4());
            let edge = Edge::new(
                edge_id,
                chunk.corpus_id.clone(),
                from_id,
                to_id,
                edge_kind.to_string(),
                chunk.location.clone(),
            );
            result.edges.push(edge);
        }
    }
}

// ── Call extraction ──────────────────────────────────────────────────────────

fn extract_calls(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    result: &mut ExtractedCodeStructure,
) {
    let callees: Vec<std::ops::Range<usize>> = {
        let language = (lang.language_fn)();
        let query = match Query::new(&language, lang.call_query) {
            Ok(q) => q,
            Err(_) => return,
        };
        let mut cursor = QueryCursor::new();
        cursor
            .matches(&query, root, source)
            .flat_map(|m| {
                m.captures
                    .iter()
                    .map(|c| c.node.byte_range())
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    // The "from" symbol is derived from the chunk's location path fragment.
    let from_symbol = chunk
        .location
        .path
        .split('#')
        .nth(1)
        .unwrap_or(&chunk.location.path)
        .to_string();
    let from_id = entity_id(&chunk.corpus_id, &from_symbol);

    for callee_range in callees {
        let callee_text = text_from_bytes(source, callee_range);
        if callee_text.len() < 2
            || callee_text
                .chars()
                .next()
                .is_none_or(|c| c.is_ascii_digit())
        {
            continue;
        }

        let to_id = entity_id(&chunk.corpus_id, &callee_text);
        let edge_id = format!("{}-{}", chunk.corpus_id, Uuid::new_v4());
        let edge = Edge::new(
            edge_id,
            chunk.corpus_id.clone(),
            from_id.clone(),
            to_id,
            "calls".to_string(),
            chunk.location.clone(),
        );
        result.edges.push(edge);
    }
}

// ── Import extraction ────────────────────────────────────────────────────────

fn extract_imports(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    result: &mut ExtractedCodeStructure,
) {
    let query_str = match lang.name {
        "rust" => r#"(use_declaration argument: (_) @import)"#,
        "typescript" | "javascript" => r#"(import_statement source: (string) @import)"#,
        "python" => {
            r#"[(import_statement name: (dotted_name) @import)
               (import_from_statement module_name: (dotted_name) @import)]"#
        }
        "go" => r#"(import_declaration (import_spec path: (interpreted_string_literal) @import))"#,
        _ => return,
    };

    let import_ranges: Vec<std::ops::Range<usize>> = {
        let language = (lang.language_fn)();
        let query = match Query::new(&language, query_str) {
            Ok(q) => q,
            Err(_) => return,
        };
        let mut cursor = QueryCursor::new();
        cursor
            .matches(&query, root, source)
            .flat_map(|m| {
                m.captures
                    .iter()
                    .map(|c| c.node.byte_range())
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    let file_path = chunk
        .location
        .path
        .split('#')
        .next()
        .unwrap_or(&chunk.location.path)
        .to_string();

    let file_entity_id = entity_id(&chunk.corpus_id, &file_path);

    for range in import_ranges {
        let import_text = text_from_bytes(source, range);
        let import_text = import_text.trim_matches('"').trim_matches('\'').to_string();
        if import_text.is_empty() {
            continue;
        }

        // Entity for the imported module.
        let import_entity_id = entity_id(&chunk.corpus_id, &import_text);
        let mut entity = Entity::new(
            import_entity_id.clone(),
            chunk.corpus_id.clone(),
            import_text.clone(),
            "import".to_string(),
        );
        entity.confidence = 0.95;
        result.entities.push(entity);

        // Edge: this file imports the module.
        let edge_id = format!("{}-{}", chunk.corpus_id, Uuid::new_v4());
        let edge = Edge::new(
            edge_id,
            chunk.corpus_id.clone(),
            file_entity_id.clone(),
            import_entity_id,
            "imports".to_string(),
            chunk.location.clone(),
        );
        result.edges.push(edge);
    }
}

// ── Doc comment extraction ────────────────────────────────────────────────────

fn extract_doc_comment(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang_name: &str,
) -> Option<String> {
    let first_child = root.child(0)?;
    let comment_text = match lang_name {
        "rust" => {
            if first_child.kind() == "line_comment" || first_child.kind() == "block_comment" {
                let text = text_from_bytes(source, first_child.byte_range());
                if text.starts_with("///") || text.starts_with("/*!") || text.starts_with("/**") {
                    Some(text.trim().to_string())
                } else {
                    None
                }
            } else {
                None
            }
        }
        "typescript" | "javascript" => {
            if first_child.kind() == "comment" {
                let text = text_from_bytes(source, first_child.byte_range());
                if text.starts_with("/**") || text.starts_with("//") {
                    Some(text.trim().to_string())
                } else {
                    None
                }
            } else {
                None
            }
        }
        "python" => {
            if first_child.kind() == "expression_statement" {
                let child = first_child.child(0)?;
                if child.kind() == "string" {
                    Some(
                        text_from_bytes(source, child.byte_range())
                            .trim()
                            .to_string(),
                    )
                } else {
                    None
                }
            } else {
                None
            }
        }
        "go" => {
            if first_child.kind() == "comment" {
                Some(
                    text_from_bytes(source, first_child.byte_range())
                        .trim()
                        .to_string(),
                )
            } else {
                None
            }
        }
        _ => None,
    };

    comment_text.filter(|s| !s.is_empty())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn ts_node_to_entity_kind(ts_kind: &str) -> &'static str {
    match ts_kind {
        "function_item"
        | "function_declaration"
        | "function_definition"
        | "method_declaration"
        | "arrow_function" => "function",
        "impl_item" | "struct_item" | "class_declaration" | "class_definition" => "class",
        "trait_item" | "interface_declaration" => "interface",
        "mod_item" | "namespace_declaration" => "module",
        "type_declaration" | "type_alias_declaration" => "interface",
        "export_statement" => "module",
        _ => "function",
    }
}

/// Extract a symbol name from a code text slice.
///
/// Uses a keyword-based heuristic — no AST traversal.
fn extract_name_from_text(text: &str, node_kind: &str) -> Option<String> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    let keywords: &[&str] = match node_kind {
        "function_item" | "function_declaration" | "function_definition" => {
            &["fn", "func", "function", "def"]
        }
        "struct_item" | "class_declaration" | "class_definition" => &["struct", "class"],
        "impl_item" => &["impl"],
        "trait_item" | "interface_declaration" => &["trait", "interface"],
        "mod_item" => &["mod"],
        "type_declaration" | "type_alias_declaration" => &["type"],
        _ => &["fn", "func", "function", "def", "class", "struct"],
    };

    for (i, &token) in tokens.iter().enumerate() {
        let trimmed = token
            .trim_start_matches("pub(crate)")
            .trim_start_matches("pub");
        let trimmed = trimmed.trim();
        if (keywords.contains(&trimmed) || keywords.contains(&token))
            && let Some(next) = tokens.get(i + 1)
        {
            let sym: String = next
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !sym.is_empty() {
                return Some(sym);
            }
        }
    }

    None
}

fn text_from_bytes(source: &[u8], range: std::ops::Range<usize>) -> String {
    source
        .get(range)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
        .to_string()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::languages;
    use callimachus_core::types::Location;

    fn make_chunk(corpus_id: &str, path: &str, kind: &str, content: &str) -> Chunk {
        Chunk::new(
            corpus_id.to_string(),
            None,
            kind.to_string(),
            Location::new(corpus_id, path),
            content.to_string(),
        )
    }

    #[test]
    fn rust_function_produces_function_entity() {
        let chunk = make_chunk(
            "test",
            "src/lib.rs#process_order",
            "function",
            r#"/// Process an order.
fn process_order(id: u64) -> Result<(), String> {
    let _ = validate(id);
    Ok(())
}"#,
        );
        let lang = languages::for_extension("rs").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        let functions: Vec<_> = result
            .entities
            .iter()
            .filter(|e| e.kind == "function")
            .collect();
        assert!(
            !functions.is_empty(),
            "should extract at least one function entity"
        );

        let process_fn = functions
            .iter()
            .find(|e| e.canonical_name == "process_order");
        assert!(process_fn.is_some(), "should find process_order entity");
    }

    #[test]
    fn rust_call_produces_calls_edge() {
        let chunk = make_chunk(
            "test",
            "src/main.rs#main",
            "function",
            r#"fn main() {
    process_order(42);
    log_event("started");
}"#,
        );
        let lang = languages::for_extension("rs").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        let call_edges: Vec<_> = result.edges.iter().filter(|e| e.kind == "calls").collect();
        assert!(!call_edges.is_empty(), "should extract call edges");

        let calls_process = call_edges
            .iter()
            .any(|e| e.to_entity_id.contains("process_order"));
        assert!(calls_process, "should have a calls edge to process_order");
    }

    #[test]
    fn typescript_class_produces_class_entity() {
        let chunk = make_chunk(
            "test",
            "src/auth.ts#AuthService",
            "class",
            r#"/** Auth service */
class AuthService {
    login(user: string): boolean {
        return true;
    }
}"#,
        );
        let lang = languages::for_extension("ts").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        let classes: Vec<_> = result
            .entities
            .iter()
            .filter(|e| e.kind == "class")
            .collect();
        assert!(
            !classes.is_empty(),
            "should extract at least one class entity"
        );

        let defines_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == "defines")
            .collect();
        assert!(!defines_edges.is_empty(), "should have defines edges");
    }

    #[test]
    fn python_import_produces_imports_edge() {
        let chunk = make_chunk(
            "test",
            "src/utils.py",
            "file",
            r#"import os
import sys

def helper():
    return os.getcwd()
"#,
        );
        let lang = languages::for_extension("py").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        let import_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == "imports")
            .collect();
        assert!(!import_edges.is_empty(), "should extract import edges");
        // All import edges must use slug-format IDs.
        for e in &import_edges {
            assert!(
                e.from_entity_id.starts_with("test:"),
                "from_entity_id should start with corpus prefix: {}",
                e.from_entity_id
            );
            assert!(
                e.to_entity_id.starts_with("test:"),
                "to_entity_id should start with corpus prefix: {}",
                e.to_entity_id
            );
        }
        assert!(
            import_edges.iter().any(|e| e.to_entity_id.contains("os")),
            "should have an import edge containing 'os'"
        );
    }

    #[test]
    fn all_entity_ids_use_slug_format() {
        let chunk = make_chunk(
            "test",
            "src/lib.rs#process_order",
            "function",
            r#"fn process_order(id: u64) -> Result<(), String> {
    let _ = validate(id);
    Ok(())
}"#,
        );
        let lang = languages::for_extension("rs").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        for entity in &result.entities {
            assert!(
                entity.id.starts_with("test:"),
                "entity id should start with corpus prefix 'test:': {}",
                entity.id
            );
        }
        for edge in &result.edges {
            assert!(
                edge.from_entity_id.starts_with("test:"),
                "edge from_entity_id should start with 'test:': {}",
                edge.from_entity_id
            );
            assert!(
                edge.to_entity_id.starts_with("test:"),
                "edge to_entity_id should start with 'test:': {}",
                edge.to_entity_id
            );
        }
    }

    #[test]
    fn rust_doc_comment_extracted() {
        let chunk = make_chunk(
            "test",
            "src/lib.rs#helper",
            "function",
            r#"/// This is a doc comment.
fn helper() -> u32 {
    42
}"#,
        );
        let lang = languages::for_extension("rs").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();
        assert!(result.doc_comment.is_some(), "should extract a doc comment");
        assert!(
            result
                .doc_comment
                .as_deref()
                .unwrap()
                .contains("doc comment"),
            "doc comment content incorrect: {:?}",
            result.doc_comment
        );
    }
}
