use anyhow::Result;
use callimachus_core::types::{Chunk, Edge, Entity};
use sha2::{Digest, Sha256};
use tree_sitter::{Parser, Query, QueryCursor};

use crate::languages::LangConfig;

// ── Deterministic ID helpers ──────────────────────────────────────────────────

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

/// Build a deterministic, corpus-scoped edge ID from the edge's identity
/// fields: `(from_id, kind, to_id, origin_scope)`.
///
/// Uses SHA-256 over the pipe-separated fields and encodes the first 8 bytes
/// as a 16-character lowercase hex string, producing ids like
/// `{corpus_id}:edge:{hex16}`.  The result is whitespace-free.
///
/// Determinism over these four fields means that N call sites to the same
/// function in the same file produce N edges with the **same** id, which the
/// post-extraction aggregation step then collapses into one row with
/// `occurrence_count = N`.
fn edge_id(corpus_id: &str, from_id: &str, kind: &str, to_id: &str, origin_scope: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(from_id.as_bytes());
    hasher.update(b"|");
    hasher.update(kind.as_bytes());
    hasher.update(b"|");
    hasher.update(to_id.as_bytes());
    hasher.update(b"|");
    hasher.update(origin_scope.as_bytes());
    let hash = hasher.finalize();
    let short_hash = hex::encode(&hash[..8]); // 16 hex chars
    format!("{corpus_id}:edge:{short_hash}")
}

// ── Test-scope detection (Rust only) ─────────────────────────────────────────

/// Returns the byte ranges in `source` that constitute "test scope":
/// - entire `mod_item` nodes annotated with `#[cfg(test)]`
/// - entire `function_item` nodes annotated with `#[test]` or `#[tokio::test]`
///
/// A source node is "test scope" if its start byte falls within any of the
/// returned ranges.  Only called for Rust; returns empty vec for other langs.
fn rust_test_byte_ranges(
    root: tree_sitter::Node<'_>,
    source: &[u8],
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    collect_test_ranges(root, source, &mut ranges);
    ranges
}

fn collect_test_ranges(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    ranges: &mut Vec<std::ops::Range<usize>>,
) {
    let kind = node.kind();

    if kind == "mod_item" && item_has_cfg_test_attr(node, source) {
        // The entire mod is test scope — record its range and don't descend.
        ranges.push(node.byte_range());
        return;
    }

    if kind == "function_item" && item_has_test_attr(node, source) {
        ranges.push(node.byte_range());
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_test_ranges(child, source, ranges);
    }
}

/// Returns `true` when `node` (a `mod_item`) is annotated with `#[cfg(test)]`.
///
/// Uses a dual strategy to handle both grammar layouts seen in different
/// tree-sitter-rust versions:
///
/// - **Children** (attributes are children of the item node): walk
///   `node.children()` for an `attribute_item` containing `cfg` + `test`.
/// - **Preceding siblings** (attributes are sibling nodes that precede the
///   item, which is what tree-sitter-rust 0.23 actually emits): walk
///   backwards through `node.prev_sibling()` for the same pattern.
fn item_has_cfg_test_attr(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    // Strategy 1: attribute_item as a child of this node.
    {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "attribute_item" {
                let text = std::str::from_utf8(&source[child.byte_range()]).unwrap_or("");
                if text.contains("cfg") && text.contains("test") {
                    return true;
                }
            }
        }
    }
    // Strategy 2: attribute_item as a preceding sibling of this node.
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute_item" {
            let text = std::str::from_utf8(&source[s.byte_range()]).unwrap_or("");
            if text.contains("cfg") && text.contains("test") {
                return true;
            }
            sibling = s.prev_sibling();
        } else {
            break; // gap between attributes and item — stop walking
        }
    }
    false
}

/// Returns `true` when `node` (a `function_item`) is annotated with `#[test]`
/// or `#[tokio::test]`.
///
/// Uses the same dual child/preceding-sibling strategy as
/// `item_has_cfg_test_attr`.
fn item_has_test_attr(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    // Strategy 1: attribute_item as a child of this node.
    {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "attribute_item" {
                let text = std::str::from_utf8(&source[child.byte_range()]).unwrap_or("");
                if text.contains("test") {
                    return true;
                }
            }
        }
    }
    // Strategy 2: attribute_item as a preceding sibling of this node.
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute_item" {
            let text = std::str::from_utf8(&source[s.byte_range()]).unwrap_or("");
            if text.contains("test") {
                return true;
            }
            sibling = s.prev_sibling();
        } else {
            break;
        }
    }
    false
}

/// Returns `"test"` when `byte_pos` falls inside one of the test-scope ranges;
/// `"production"` otherwise.
fn scope_for_pos(byte_pos: usize, test_ranges: &[std::ops::Range<usize>]) -> String {
    if test_ranges.iter().any(|r| r.contains(&byte_pos)) {
        "test".to_string()
    } else {
        "production".to_string()
    }
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

    // Build test-scope byte ranges once per file (Rust only).
    // All non-Rust languages default every edge to "production".
    let test_ranges: Vec<std::ops::Range<usize>> = if lang_config.name == "rust" {
        rust_test_byte_ranges(root, source)
    } else {
        vec![]
    };

    // Extract top-level named entities.
    extract_entities(chunk, root, source, lang_config, &test_ranges, &mut result);

    // Extract call edges.
    extract_calls(chunk, root, source, lang_config, &test_ranges, &mut result);

    // Extract import edges.
    extract_imports(chunk, root, source, lang_config, &test_ranges, &mut result);

    // PHP-specific: extract `new ClassName()` instantiation edges.
    if lang_config.name == "php" {
        extract_php_instantiations(chunk, root, source, lang_config, &test_ranges, &mut result);
    }

    // Aggregate edges: collapse entries with the same deterministic id into a
    // single row, summing occurrence_counts.  Keep the first edge's location
    // and confidence.
    //
    // Because edge ids encode (from, kind, to, origin_scope), N call sites to
    // the same function become one edge with occurrence_count = N.  On
    // incremental reindex the cascade deletes the file's edges before this
    // batch is emitted, so overwrite upsert in the storage layer is correct
    // and idempotent — see edge_store::upsert for the storage-side invariant.
    let mut aggregated: std::collections::HashMap<String, Edge> = std::collections::HashMap::new();
    for edge in result.edges.drain(..) {
        aggregated
            .entry(edge.id.clone())
            .and_modify(|e| e.occurrence_count += edge.occurrence_count)
            .or_insert(edge);
    }
    result.edges = aggregated.into_values().collect();

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
    /// Name extracted from the AST node's `name` field or name-like children.
    name: Option<String>,
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
                    name: name_from_node(c.node, source),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

// ── AST name extraction ───────────────────────────────────────────────────────

/// Extract a symbol name from a tree-sitter node by inspecting its AST structure.
///
/// First tries the `name` field (e.g. `class_declaration name: (name)`).
/// Falls back to scanning named children for name-like node kinds.
fn name_from_node(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(n) = node.child_by_field_name("name") {
        let t = text_from_bytes(source, n.byte_range());
        if !t.trim().is_empty() {
            return Some(t.trim().to_string());
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "name" | "identifier" | "type_identifier" | "qualified_name" | "namespace_name" => {
                let t = text_from_bytes(source, child.byte_range());
                if !t.trim().is_empty() {
                    return Some(t.trim().to_string());
                }
            }
            _ => {}
        }
    }
    None
}

// ── Entity extraction ─────────────────────────────────────────────────────────

fn extract_entities(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    test_ranges: &[std::ops::Range<usize>],
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

    // Always emit the file entity — even when no top-level symbols are found,
    // the file entity must exist so call-edge from_entity_id FKs resolve.
    {
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

    for node in &nodes {
        let text = match std::str::from_utf8(source.get(node.byte_range.clone()).unwrap_or(&[])) {
            Ok(t) => t,
            Err(_) => continue,
        };

        let kind = ts_node_to_entity_kind(&node.kind);

        // Use AST-based name first, then keyword-scanning fallback.
        let name = node
            .name
            .as_deref()
            .map(|s| s.to_string())
            .or_else(|| extract_name_from_text(text, &node.kind))
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

        // Emit a "defines" edge: this file defines the symbol.
        let scope = scope_for_pos(node.byte_range.start, test_ranges);
        let eid = edge_id(
            &chunk.corpus_id,
            &file_entity_id,
            "defines",
            &sym_entity_id,
            &scope,
        );
        let mut defines_edge = Edge::new(
            eid,
            chunk.corpus_id.clone(),
            file_entity_id.clone(),
            sym_entity_id,
            "defines".to_string(),
            chunk.location.clone(),
        );
        defines_edge.origin_scope = scope;
        result.edges.push(defines_edge);
        result.entities.push(entity);
    }

    // Also extract extends/implements for class-like nodes.
    extract_inheritance(chunk, root, source, lang, test_ranges, result);

    // PHP: descend into class bodies to extract methods.
    if lang.name == "php" {
        extract_php_methods(chunk, root, source, lang, test_ranges, result);
    }
}

/// PHP-specific: extract method declarations nested inside class bodies.
///
/// Emits one `method` entity and one `defines` edge (class → method) per
/// method.  The file-level `defines` edges for the enclosing class are
/// already emitted by `extract_entities`; this adds the intra-class layer.
fn extract_php_methods(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    test_ranges: &[std::ops::Range<usize>],
    result: &mut ExtractedCodeStructure,
) {
    let query_str = r#"
        (class_declaration
            name: (name) @class_name
            body: (declaration_list
                (method_declaration
                    name: (name) @method_name)))
    "#;

    let language = (lang.language_fn)();
    let query = match Query::new(&language, query_str) {
        Ok(q) => q,
        Err(_) => return,
    };

    // Collect (class_name_range, method_name_range) pairs.
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

    for (class_range, method_range) in pairs {
        let class_name = text_from_bytes(source, class_range.clone());
        let method_name = text_from_bytes(source, method_range.clone());
        if class_name.is_empty() || method_name.is_empty() {
            continue;
        }

        let class_entity_id = entity_id(&chunk.corpus_id, &class_name);
        let method_entity_id = entity_id(&chunk.corpus_id, &method_name);

        // Emit method entity.
        let mut method_entity = Entity::new(
            method_entity_id.clone(),
            chunk.corpus_id.clone(),
            method_name.clone(),
            "method".to_string(),
        );
        method_entity.first_location = Some(chunk.location.clone());
        method_entity.last_location = Some(chunk.location.clone());
        method_entity.appearance_count = 1;
        method_entity.confidence = 0.9;
        result.entities.push(method_entity);

        // Emit defines edge: class → method.
        let scope = scope_for_pos(method_range.start, test_ranges);
        let eid = edge_id(
            &chunk.corpus_id,
            &class_entity_id,
            "defines",
            &method_entity_id,
            &scope,
        );
        let mut defines_edge = Edge::new(
            eid,
            chunk.corpus_id.clone(),
            class_entity_id,
            method_entity_id,
            "defines".to_string(),
            chunk.location.clone(),
        );
        defines_edge.origin_scope = scope;
        result.edges.push(defines_edge);
    }
}

fn extract_inheritance(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    test_ranges: &[std::ops::Range<usize>],
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
        "php" => &[
            (
                r#"(class_declaration name: (name) @class (base_clause [(name) (qualified_name)] @parent))"#,
                "extends",
            ),
            (
                r#"(class_declaration name: (name) @class (class_interface_clause [(name) (qualified_name)] @iface))"#,
                "implements",
            ),
            (
                r#"(interface_declaration name: (name) @iface (base_clause [(name) (qualified_name)] @parent))"#,
                "extends",
            ),
        ],
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
            let from = text_from_bytes(source, from_range.clone());
            let mut to = text_from_bytes(source, to_range);
            if from.is_empty() || to.is_empty() {
                continue;
            }

            // Strip PHP namespace separator prefix (e.g. `\App\Models\User` → `App\Models\User`).
            if lang.name == "php" {
                to = to.trim_start_matches('\\').to_string();
            }

            let from_id = entity_id(&chunk.corpus_id, &from);
            let to_id = entity_id(&chunk.corpus_id, &to);
            let scope = scope_for_pos(from_range.start, test_ranges);
            let eid = edge_id(&chunk.corpus_id, &from_id, edge_kind, &to_id, &scope);
            let mut edge = Edge::new(
                eid,
                chunk.corpus_id.clone(),
                from_id,
                to_id,
                edge_kind.to_string(),
                chunk.location.clone(),
            );
            edge.origin_scope = scope;
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
    test_ranges: &[std::ops::Range<usize>],
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

    // Strip any '#fragment' to get the bare file path.
    let file_path = chunk
        .location
        .path
        .split('#')
        .next()
        .unwrap_or(&chunk.location.path)
        .to_string();

    // The "from" symbol is the function/method named in the chunk's location
    // fragment.  When no fragment is present, fall back to the file entity so
    // the edge always points to an entity that was emitted by extract_entities.
    let from_id = match chunk.location.path.split_once('#') {
        Some((_, fragment)) if !fragment.is_empty() => entity_id(&chunk.corpus_id, fragment),
        _ => entity_id(&chunk.corpus_id, &file_path),
    };

    for callee_range in callees {
        let callee_text = text_from_bytes(source, callee_range.clone());
        if callee_text.len() < 2
            || callee_text
                .chars()
                .next()
                .is_none_or(|c| c.is_ascii_digit())
        {
            continue;
        }

        let to_id = entity_id(&chunk.corpus_id, &callee_text);
        let scope = scope_for_pos(callee_range.start, test_ranges);
        let eid = edge_id(&chunk.corpus_id, &from_id, "calls", &to_id, &scope);
        let mut edge = Edge::new(
            eid,
            chunk.corpus_id.clone(),
            from_id.clone(),
            to_id,
            "calls".to_string(),
            chunk.location.clone(),
        );
        edge.origin_scope = scope;
        result.edges.push(edge);
    }
}

// ── Rust use-declaration decomposition ──────────────────────────────────────

/// Join two Rust path segments with `::`, handling empty parts gracefully.
fn join_path_segments(prefix: &str, segment: &str) -> String {
    match (prefix.is_empty(), segment.is_empty()) {
        (true, _) => segment.to_string(),
        (_, true) => prefix.to_string(),
        _ => format!("{prefix}::{segment}"),
    }
}

/// Recursively collect leaf import paths from a Rust `_use_clause` AST node.
///
/// `prefix` is the path accumulated from enclosing `scoped_use_list` nodes.
/// Each leaf is a `(full_path, byte_pos)` pair — `byte_pos` is used for
/// `scope_for_pos` to tag the edge with the correct `origin_scope`.
///
/// Handles:
/// - `scoped_use_list` (`path::{list}`) — extend prefix with path, recurse
/// - `use_list` (`{item, ...}`) — recurse each item with the same prefix
/// - `use_as_clause` (`path as alias`) — emit the original path, ignore alias
/// - `use_wildcard` (`path::*` or bare `*`) — emit `prefix::*` as a single leaf
/// - `identifier` — emit `prefix::name` (or just `name` if prefix is empty)
/// - `scoped_identifier` and other path-like nodes — use full text as the leaf
fn collect_rust_use_leaves_into(
    node: tree_sitter::Node<'_>,
    prefix: &str,
    source: &[u8],
    out: &mut Vec<(String, usize)>,
) {
    match node.kind() {
        "scoped_use_list" => {
            // path::{list} — extend prefix with path, then recurse into list items.
            let path_text = node
                .child_by_field_name("path")
                .map(|n| text_from_bytes(source, n.byte_range()))
                .unwrap_or_default();
            let new_prefix = join_path_segments(prefix, &path_text);
            if let Some(list_node) = node.child_by_field_name("list") {
                let mut cursor = list_node.walk();
                for child in list_node.named_children(&mut cursor) {
                    collect_rust_use_leaves_into(child, &new_prefix, source, out);
                }
            }
        }
        "use_list" => {
            // {item, ...} — bare list, recurse each item with the same prefix.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_rust_use_leaves_into(child, prefix, source, out);
            }
        }
        "use_as_clause" => {
            // `path as alias` — emit the original path, ignore the alias.
            let path_text = if let Some(path_node) = node.child_by_field_name("path") {
                text_from_bytes(source, path_node.byte_range())
            } else if let Some(first) = node.named_child(0) {
                text_from_bytes(source, first.byte_range())
            } else {
                text_from_bytes(source, node.byte_range())
            };
            let leaf = join_path_segments(prefix, &path_text);
            if !leaf.is_empty() {
                out.push((leaf, node.start_byte()));
            }
        }
        "use_wildcard" => {
            // `path::*` at any nesting level — treated as a single leaf `path::*`.
            //
            // In tree-sitter-rust 0.23, `child_by_field_name("path")` is not
            // reliably populated for `use_wildcard` nodes; instead we parse the
            // node's full text to extract the prefix (e.g. `std::io::*` →
            // path = `std::io`, leaf = `std::io::*`; bare `*` → leaf = `*` or
            // `prefix::*` when nested inside a scoped_use_list).
            // TODO(per-language): other languages don't use this path (non-Rust
            // branches are handled by the query-based extractor below).
            let full_text = text_from_bytes(source, node.byte_range());
            let path_text = if let Some(stripped) = full_text.strip_suffix("::*") {
                stripped.to_string()
            } else {
                // Bare `*` (inside a use_list / at the top level of a use_declaration).
                String::new()
            };
            let base = join_path_segments(prefix, &path_text);
            let leaf = if base.is_empty() {
                "*".to_string()
            } else {
                format!("{base}::*")
            };
            out.push((leaf, node.start_byte()));
        }
        "identifier" => {
            let name = text_from_bytes(source, node.byte_range());
            if name == "self" {
                // `use path::{self}` — the imported symbol IS the path prefix.
                if !prefix.is_empty() {
                    out.push((prefix.to_string(), node.start_byte()));
                }
            } else if !name.is_empty() {
                out.push((join_path_segments(prefix, &name), node.start_byte()));
            }
        }
        _ => {
            // `scoped_identifier` and other path-like nodes — use the full text
            // as the leaf (e.g. `crate::storage::Foo` for a plain qualified import).
            let text = text_from_bytes(source, node.byte_range());
            if !text.is_empty() {
                out.push((join_path_segments(prefix, &text), node.start_byte()));
            }
        }
    }
}

/// Walk the full AST rooted at `node` and emit one `import` entity + `imports`
/// edge per leaf name found inside each `use_declaration`.
///
/// A grouped `use a::{B, C, D};` yields three edges; a plain
/// `use a::B;` yields one.  Origin scope and deterministic edge IDs come from
/// PR 1/2 helpers already in scope.
fn walk_tree_for_rust_imports(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    chunk: &Chunk,
    file_entity_id: &str,
    test_ranges: &[std::ops::Range<usize>],
    result: &mut ExtractedCodeStructure,
) {
    if node.kind() == "use_declaration" {
        if let Some(arg) = node.child_by_field_name("argument") {
            let mut leaves: Vec<(String, usize)> = Vec::new();
            collect_rust_use_leaves_into(arg, "", source, &mut leaves);

            for (leaf_path, byte_pos) in leaves {
                if leaf_path.is_empty() {
                    continue;
                }

                let import_entity_id = entity_id(&chunk.corpus_id, &leaf_path);
                let mut entity = Entity::new(
                    import_entity_id.clone(),
                    chunk.corpus_id.clone(),
                    leaf_path.clone(),
                    "import".to_string(),
                );
                entity.confidence = 0.95;
                result.entities.push(entity);

                let scope = scope_for_pos(byte_pos, test_ranges);
                let eid = edge_id(
                    &chunk.corpus_id,
                    file_entity_id,
                    "imports",
                    &import_entity_id,
                    &scope,
                );
                let mut edge = Edge::new(
                    eid,
                    chunk.corpus_id.clone(),
                    file_entity_id.to_string(),
                    import_entity_id,
                    "imports".to_string(),
                    chunk.location.clone(),
                );
                edge.origin_scope = scope;
                result.edges.push(edge);
            }
        }
        // Don't descend into use_declaration children — the argument was
        // already processed above.
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_tree_for_rust_imports(child, source, chunk, file_entity_id, test_ranges, result);
    }
}

// ── Import extraction ────────────────────────────────────────────────────────

fn extract_imports(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    test_ranges: &[std::ops::Range<usize>],
    result: &mut ExtractedCodeStructure,
) {
    // Rust: use the AST-walking decomposer so that grouped `use a::{B, C, D}`
    // yields one import entity and edge per leaf name rather than one mega-entity
    // for the entire use-tree.
    if lang.name == "rust" {
        let file_path = chunk
            .location
            .path
            .split('#')
            .next()
            .unwrap_or(&chunk.location.path)
            .to_string();
        let file_entity_id = entity_id(&chunk.corpus_id, &file_path);
        walk_tree_for_rust_imports(root, source, chunk, &file_entity_id, test_ranges, result);
        return;
    }

    let query_str = match lang.name {
        "typescript" | "javascript" => r#"(import_statement source: (string) @import)"#,
        "python" => {
            r#"[(import_statement name: (dotted_name) @import)
               (import_from_statement module_name: (dotted_name) @import)]"#
        }
        "go" => r#"(import_declaration (import_spec path: (interpreted_string_literal) @import))"#,
        "php" => r#"(namespace_use_clause [(name) (qualified_name)] @import)"#,
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
        let import_text = text_from_bytes(source, range.clone());
        // Strip string delimiters (for languages like Go/TS that quote imports).
        let import_text = import_text.trim_matches('"').trim_matches('\'');
        // Strip PHP namespace separator prefix.
        let import_text = if lang.name == "php" {
            import_text.trim_start_matches('\\')
        } else {
            import_text
        };
        let import_text = import_text.to_string();
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
        let scope = scope_for_pos(range.start, test_ranges);
        let eid = edge_id(
            &chunk.corpus_id,
            &file_entity_id,
            "imports",
            &import_entity_id,
            &scope,
        );
        let mut edge = Edge::new(
            eid,
            chunk.corpus_id.clone(),
            file_entity_id.clone(),
            import_entity_id,
            "imports".to_string(),
            chunk.location.clone(),
        );
        edge.origin_scope = scope;
        result.edges.push(edge);
    }
}

// ── PHP instantiation extraction ─────────────────────────────────────────────

/// PHP-specific: extract `new ClassName()` expressions as `instantiates` edges.
fn extract_php_instantiations(
    chunk: &Chunk,
    root: tree_sitter::Node<'_>,
    source: &[u8],
    lang: &LangConfig,
    test_ranges: &[std::ops::Range<usize>],
    result: &mut ExtractedCodeStructure,
) {
    let query_str = r#"(object_creation_expression [(name) (qualified_name)] @target)"#;

    let target_ranges: Vec<std::ops::Range<usize>> = {
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

    // Use the same from-id logic as extract_calls.
    let file_path = chunk
        .location
        .path
        .split('#')
        .next()
        .unwrap_or(&chunk.location.path)
        .to_string();
    let from_id = match chunk.location.path.split_once('#') {
        Some((_, fragment)) if !fragment.is_empty() => entity_id(&chunk.corpus_id, fragment),
        _ => entity_id(&chunk.corpus_id, &file_path),
    };

    for range in target_ranges {
        let target_text = text_from_bytes(source, range.clone());
        // Strip leading PHP namespace separator.
        let target_text = target_text.trim_start_matches('\\').to_string();
        if target_text.is_empty() {
            continue;
        }

        let to_id = entity_id(&chunk.corpus_id, &target_text);
        let scope = scope_for_pos(range.start, test_ranges);
        let eid = edge_id(&chunk.corpus_id, &from_id, "instantiates", &to_id, &scope);
        let mut edge = Edge::new(
            eid,
            chunk.corpus_id.clone(),
            from_id.clone(),
            to_id,
            "instantiates".to_string(),
            chunk.location.clone(),
        );
        edge.origin_scope = scope;
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
        "function_item" | "function_declaration" | "function_definition" | "arrow_function" => {
            "function"
        }
        "method_declaration" => "method",
        "impl_item" | "struct_item" | "class_declaration" | "class_definition" => "class",
        "trait_item" | "interface_declaration" => "interface",
        "trait_declaration" => "trait",
        "enum_declaration" => "enum",
        "mod_item" | "namespace_declaration" | "namespace_definition" => "module",
        "type_declaration" | "type_alias_declaration" => "interface",
        "export_statement" => "module",
        _ => "function",
    }
}

/// Extract a symbol name from a code text slice.
///
/// Uses a keyword-based heuristic — no AST traversal.  This is a fallback
/// used when `name_from_node` cannot find a name via the AST.
fn extract_name_from_text(text: &str, node_kind: &str) -> Option<String> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    let keywords: &[&str] = match node_kind {
        "function_item" | "function_declaration" | "function_definition" => {
            &["fn", "func", "function", "def"]
        }
        "method_declaration" => &["function", "fn"],
        "struct_item" | "class_declaration" | "class_definition" => &["struct", "class"],
        "impl_item" => &["impl"],
        "trait_item" | "interface_declaration" => &["trait", "interface"],
        "trait_declaration" => &["trait"],
        "enum_declaration" => &["enum"],
        "namespace_definition" | "namespace_declaration" => &["namespace"],
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

    // ── PHP tests ─────────────────────────────────────────────────────────────

    #[test]
    fn php_class_produces_named_class_entity() {
        let chunk = make_chunk(
            "test",
            "src/Services/OrderService.php",
            "file",
            r#"<?php namespace App\Services; final class OrderService { public function place(): void {} }"#,
        );
        let lang = languages::for_extension("php").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        // No anonymous_ entities.
        for entity in &result.entities {
            assert!(
                !entity.canonical_name.starts_with("anonymous_"),
                "should not produce anonymous entities, got: {}",
                entity.canonical_name
            );
        }

        // Should have a class entity named OrderService.
        let class_entity = result
            .entities
            .iter()
            .find(|e| e.kind == "class" && e.canonical_name == "OrderService");
        assert!(
            class_entity.is_some(),
            "should extract class entity named OrderService; entities: {:?}",
            result
                .entities
                .iter()
                .map(|e| format!("{}:{}", e.kind, e.canonical_name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn php_method_descent_produces_method_entities() {
        let chunk = make_chunk(
            "test",
            "src/Services/OrderService.php",
            "file",
            r#"<?php namespace App\Services; final class OrderService { public function place(): void {} }"#,
        );
        let lang = languages::for_extension("php").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        // Should have a method entity named place.
        let method_entity = result
            .entities
            .iter()
            .find(|e| e.kind == "method" && e.canonical_name == "place");
        assert!(
            method_entity.is_some(),
            "should extract method entity named place; entities: {:?}",
            result
                .entities
                .iter()
                .map(|e| format!("{}:{}", e.kind, e.canonical_name))
                .collect::<Vec<_>>()
        );

        // Should have a defines edge from OrderService (class) to place (method).
        let class_to_method = result.edges.iter().any(|e| {
            e.kind == "defines"
                && e.from_entity_id.contains("orderservice")
                && e.to_entity_id.contains("place")
        });
        assert!(
            class_to_method,
            "should have defines edge from OrderService to place; edges: {:?}",
            result
                .edges
                .iter()
                .filter(|e| e.kind == "defines")
                .map(|e| format!("{} → {}", e.from_entity_id, e.to_entity_id))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn php_extends_implements_edges() {
        let chunk = make_chunk(
            "test",
            "src/Admin.php",
            "file",
            r#"<?php class Admin extends User implements Loggable, Cacheable {}"#,
        );
        let lang = languages::for_extension("php").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        // extends: Admin → User
        assert!(
            result.edges.iter().any(|e| {
                e.kind == "extends"
                    && e.from_entity_id.contains("admin")
                    && e.to_entity_id.contains("user")
            }),
            "should have extends edge from admin to user; edges: {:?}",
            result
                .edges
                .iter()
                .map(|e| format!("{}:{} → {}", e.kind, e.from_entity_id, e.to_entity_id))
                .collect::<Vec<_>>()
        );

        // implements: Admin → Loggable
        assert!(
            result.edges.iter().any(|e| {
                e.kind == "implements"
                    && e.from_entity_id.contains("admin")
                    && e.to_entity_id.contains("loggable")
            }),
            "should have implements edge from admin to loggable"
        );

        // implements: Admin → Cacheable
        assert!(
            result.edges.iter().any(|e| {
                e.kind == "implements"
                    && e.from_entity_id.contains("admin")
                    && e.to_entity_id.contains("cacheable")
            }),
            "should have implements edge from admin to cacheable"
        );
    }

    #[test]
    fn php_namespace_use_produces_imports_edge() {
        let chunk = make_chunk(
            "test",
            "src/foo.php",
            "file",
            r#"<?php use App\Models\User; use App\Models\Order as O;"#,
        );
        let lang = languages::for_extension("php").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        let import_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == "imports")
            .collect();
        assert!(!import_edges.is_empty(), "should extract import edges");

        assert!(
            import_edges.iter().any(|e| e.to_entity_id.contains("user")),
            "should have imports edge containing 'user'; edges: {:?}",
            import_edges
                .iter()
                .map(|e| &e.to_entity_id)
                .collect::<Vec<_>>()
        );

        assert!(
            import_edges
                .iter()
                .any(|e| e.to_entity_id.contains("order")),
            "should have imports edge containing 'order'"
        );
    }

    #[test]
    fn php_new_class_produces_instantiates_edge() {
        let chunk = make_chunk(
            "test",
            "src/foo.php#run",
            "function",
            r#"<?php function run() { $u = new User(); $o = new \App\Models\Order(); }"#,
        );
        let lang = languages::for_extension("php").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        let instantiates_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == "instantiates")
            .collect();
        assert!(
            !instantiates_edges.is_empty(),
            "should extract instantiates edges"
        );

        assert!(
            instantiates_edges
                .iter()
                .any(|e| e.to_entity_id.contains("user")),
            "should have instantiates edge to slug containing 'user'; edges: {:?}",
            instantiates_edges
                .iter()
                .map(|e| &e.to_entity_id)
                .collect::<Vec<_>>()
        );

        assert!(
            instantiates_edges
                .iter()
                .any(|e| e.to_entity_id.contains("order")),
            "should have instantiates edge to slug containing 'order'"
        );
    }

    #[test]
    fn php_method_call_from_id_is_resolvable() {
        let chunk = make_chunk(
            "test",
            "src/foo.php", // No '#' fragment
            "file",
            r#"<?php Foo::run(); $x->bar();"#,
        );
        let lang = languages::for_extension("php").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        // The file entity must exist.
        let file_entity = result
            .entities
            .iter()
            .find(|e| e.kind == "file")
            .expect("should have a file entity");
        let file_entity_id = &file_entity.id;

        // At least one calls edge must have from_entity_id == file entity id.
        let call_edges: Vec<_> = result.edges.iter().filter(|e| e.kind == "calls").collect();
        assert!(
            !call_edges.is_empty(),
            "should extract at least one calls edge"
        );

        assert!(
            call_edges
                .iter()
                .any(|e| &e.from_entity_id == file_entity_id),
            "at least one calls edge should have from_entity_id equal to the file entity id ({}); got: {:?}",
            file_entity_id,
            call_edges
                .iter()
                .map(|e| &e.from_entity_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn interface_and_trait_extract_named() {
        // Interface
        let chunk = make_chunk(
            "test",
            "src/Stringable.php",
            "file",
            r#"<?php interface Stringable {}"#,
        );
        let lang = languages::for_extension("php").unwrap();
        let result = extract_structure(&chunk, lang).unwrap();

        let iface = result
            .entities
            .iter()
            .find(|e| e.kind == "interface" && e.canonical_name == "Stringable");
        assert!(
            iface.is_some(),
            "should extract interface entity named Stringable; entities: {:?}",
            result
                .entities
                .iter()
                .map(|e| format!("{}:{}", e.kind, e.canonical_name))
                .collect::<Vec<_>>()
        );

        // Trait
        let chunk = make_chunk(
            "test",
            "src/Loggable.php",
            "file",
            r#"<?php trait Loggable {}"#,
        );
        let result = extract_structure(&chunk, lang).unwrap();

        let tr = result
            .entities
            .iter()
            .find(|e| e.kind == "trait" && e.canonical_name == "Loggable");
        assert!(
            tr.is_some(),
            "should extract trait entity named Loggable; entities: {:?}",
            result
                .entities
                .iter()
                .map(|e| format!("{}:{}", e.kind, e.canonical_name))
                .collect::<Vec<_>>()
        );
    }
}
