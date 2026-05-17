use std::path::{Path, PathBuf};

use anyhow::Result;
use callimachus_core::types::{Chunk, Location};
use tree_sitter::{Parser, Query, QueryCursor};
use walkdir::WalkDir;

use crate::languages::{self, LangConfig};

// ── Options ──────────────────────────────────────────────────────────────────

/// Options controlling how a directory is chunked.
#[derive(Debug, Clone)]
pub struct ChunkOptions {
    pub max_chunk_bytes: usize,
    pub min_chunk_bytes: usize,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    /// If Some, restrict to files changed since this git ref (reserved for future use).
    pub since_ref: Option<String>,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_chunk_bytes: 4000,
            min_chunk_bytes: 100,
            include_globs: vec![],
            exclude_globs: vec![
                "target/**".into(),
                "node_modules/**".into(),
                ".git/**".into(),
                "dist/**".into(),
                "build/**".into(),
            ],
            since_ref: None,
        }
    }
}

// ── Intermediate item data ────────────────────────────────────────────────────

/// Data extracted from a top-level AST item before chunk creation.
struct ItemInfo {
    byte_range: std::ops::Range<usize>,
    node_kind: String,
    start_row: usize,
}

// ── Public entry-point ───────────────────────────────────────────────────────

/// Walk `source_path` and emit one or more `Chunk` objects per source file.
///
/// Files with unrecognised extensions are skipped silently.
/// `corpus_id` scopes all chunk location URIs.
pub async fn chunk_directory(
    source_path: &Path,
    corpus_id: &str,
    opts: &ChunkOptions,
) -> Result<Vec<Chunk>> {
    let mut chunks = Vec::new();

    for entry in WalkDir::new(source_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let abs_path = entry.path();
        let rel = match abs_path.strip_prefix(source_path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        // Apply exclude globs first.
        if is_excluded(&rel_str, &opts.exclude_globs) {
            continue;
        }

        // Apply include globs (if any).
        if !opts.include_globs.is_empty() && !is_included(&rel_str, &opts.include_globs) {
            continue;
        }

        // Detect language by extension.
        let ext = abs_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = match languages::for_extension(ext) {
            Some(l) => l,
            None => continue, // silently skip unknown extensions
        };

        // Read file contents.
        let content = match std::fs::read_to_string(abs_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("could not read {}: {e}", abs_path.display());
                continue;
            }
        };

        // Chunk this file.
        let file_chunks = chunk_file(corpus_id, &rel_str, &content, lang, opts);
        chunks.extend(file_chunks);
    }

    Ok(chunks)
}

// ── File-level chunking ──────────────────────────────────────────────────────

fn chunk_file(
    corpus_id: &str,
    rel_path: &str,
    content: &str,
    lang: &LangConfig,
    opts: &ChunkOptions,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();

    // Build the file-level (container) chunk URI path.
    let file_uri_path = format!("src/{rel_path}");
    let file_location = Location::new(corpus_id, &file_uri_path);

    // Always emit a file chunk.
    let file_chunk = Chunk::new(
        corpus_id.to_string(),
        None,
        "file".to_string(),
        file_location.clone(),
        content.to_string(),
    );
    let file_chunk_uri = file_chunk.location.uri.clone();
    chunks.push(file_chunk);

    // Parse with tree-sitter to extract top-level items.
    let mut parser = Parser::new();
    let language = (lang.language_fn)();
    if parser.set_language(&language).is_err() {
        tracing::warn!("could not set tree-sitter language for {rel_path}");
        return chunks;
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return chunks,
    };

    // Compile the top-level item query.
    let query = match Query::new(&language, lang.top_level_query) {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!("top-level query error for {}: {e:?}", lang.name);
            return chunks;
        }
    };

    // Extract item data from AST nodes in a single pass.
    // We extract owned data immediately to avoid tree-sitter cursor lifetime issues.
    let items: Vec<ItemInfo> = {
        let mut cursor = QueryCursor::new();
        let source_bytes = content.as_bytes();
        let root_node = tree.root_node();

        cursor
            .matches(&query, root_node, source_bytes)
            .flat_map(|m| {
                m.captures
                    .iter()
                    .map(|c| ItemInfo {
                        byte_range: c.node.byte_range(),
                        node_kind: c.node.kind().to_string(),
                        start_row: c.node.start_position().row,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    for item in &items {
        let item_text = match content.get(item.byte_range.clone()) {
            Some(t) => t,
            None => continue,
        };

        // Extract symbol name from the content slice.
        let symbol = extract_symbol_from_text(item_text, &item.node_kind, lang);

        // Build location path: src/<file>#<symbol>
        let item_path = if let Some(sym) = &symbol {
            format!("src/{rel_path}#{sym}")
        } else {
            format!("src/{rel_path}#item_{}", item.start_row + 1)
        };

        let item_kind = node_kind_to_chunk_kind(&item.node_kind);

        // Handle items exceeding max_chunk_bytes.
        let item_chunks = if item_text.len() > opts.max_chunk_bytes {
            split_large_item(
                corpus_id,
                &item_path,
                &file_chunk_uri,
                item_text,
                item_kind,
                opts.max_chunk_bytes,
            )
        } else if item_text.len() < opts.min_chunk_bytes {
            vec![] // drop tiny items
        } else {
            let loc = Location::new(corpus_id, &item_path);
            vec![Chunk::new(
                corpus_id.to_string(),
                Some(file_chunk_uri.clone()),
                item_kind.to_string(),
                loc,
                item_text.to_string(),
            )]
        };

        chunks.extend(item_chunks);
    }

    chunks
}

// ── Large item splitting ─────────────────────────────────────────────────────

/// Split an oversized item into line-boundary sub-chunks (~100 lines each).
fn split_large_item(
    corpus_id: &str,
    item_path: &str,
    parent_uri: &str,
    content: &str,
    kind: &str,
    max_bytes: usize,
) -> Vec<Chunk> {
    let mut out = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let target_lines = 100;

    let mut start = 0;
    let mut part = 0;
    while start < lines.len() {
        let end = (start + target_lines).min(lines.len());
        let slice = lines[start..end].join("\n");

        if slice.len() >= 100 {
            let loc_path = format!("{item_path}/part{part}");
            let loc = Location::new(corpus_id, loc_path);
            out.push(Chunk::new(
                corpus_id.to_string(),
                Some(parent_uri.to_string()),
                kind.to_string(),
                loc,
                slice,
            ));
            part += 1;
        }

        start = end;
        let _ = max_bytes; // used for doc, actual splitting is line-based
    }

    out
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Map a tree-sitter node kind string to a Callimachus chunk kind.
fn node_kind_to_chunk_kind(ts_kind: &str) -> &'static str {
    match ts_kind {
        "function_item"
        | "function_declaration"
        | "function_definition"
        | "method_declaration"
        | "arrow_function" => "function",
        "impl_item" | "struct_item" | "class_declaration" | "class_definition" => "class",
        "trait_item" | "interface_declaration" => "interface",
        "mod_item" | "module" | "namespace" => "module",
        "type_declaration" | "type_alias_declaration" => "interface",
        "export_statement" => "module",
        _ => "function",
    }
}

/// Extract a human-readable symbol name from a text slice using a simple heuristic.
///
/// Parses the first token after common keywords.
fn extract_symbol_from_text(text: &str, node_kind: &str, lang: &LangConfig) -> Option<String> {
    let _ = lang; // reserved for language-specific logic
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    // Keywords that precede the symbol name.
    let keywords: &[&str] = match node_kind {
        "function_item" | "function_declaration" | "function_definition" => {
            &["fn", "func", "function", "def", "async"]
        }
        "struct_item" | "class_declaration" | "class_definition" | "impl_item" => {
            &["struct", "class", "impl"]
        }
        "trait_item" | "interface_declaration" => &["trait", "interface"],
        "mod_item" => &["mod"],
        "type_declaration" | "type_alias_declaration" => &["type"],
        _ => &["fn", "func", "function", "def", "class", "struct", "impl"],
    };

    // Find the first keyword and take the token after it.
    for (i, token) in tokens.iter().enumerate() {
        let clean = token.trim_start_matches("pub").trim_start_matches(' ');
        if (keywords.contains(&clean) || keywords.contains(token))
            && let Some(next) = tokens.get(i + 1)
        {
            let sym = next
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .trim_start_matches('<');
            if !sym.is_empty() && sym.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Some(sym.to_string());
            }
        }
    }

    // Fallback: first identifier-looking token (not a keyword).
    let common_keywords = ["pub", "async", "unsafe", "extern", "use", "mod"];
    for tok in &tokens[..tokens.len().min(5)] {
        let clean: String = tok
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !clean.is_empty()
            && !common_keywords.contains(&clean.as_str())
            && clean
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            return Some(clean);
        }
    }

    None
}

// ── Parse file path from chunk location ───────────────────────────────────────

/// Given a chunk location path like `src/foo/bar.rs#MyFunc`, return the relative
/// file path (`foo/bar.rs`) and optional symbol name (`MyFunc`).
pub fn parse_location_path(path: &str) -> (PathBuf, Option<String>) {
    let without_src = path.strip_prefix("src/").unwrap_or(path);
    if let Some(idx) = without_src.find('#') {
        let file = PathBuf::from(&without_src[..idx]);
        let sym = without_src[idx + 1..].to_string();
        (file, Some(sym))
    } else {
        (PathBuf::from(without_src), None)
    }
}

// ── Glob matching ────────────────────────────────────────────────────────────

fn is_excluded(rel_path: &str, exclude_globs: &[String]) -> bool {
    exclude_globs.iter().any(|g| glob_matches(g, rel_path))
}

fn is_included(rel_path: &str, include_globs: &[String]) -> bool {
    include_globs.iter().any(|g| glob_matches(g, rel_path))
}

/// Simple glob matcher supporting `*` (within segment) and `**` (any depth).
fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_match_segments(&split_glob(pattern), &split_path(path))
}

fn split_glob(pat: &str) -> Vec<&str> {
    pat.split('/').collect()
}

fn split_path(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

fn glob_match_segments(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }

    match pattern[0] {
        "**" => {
            // `**` matches zero or more path segments.
            for i in 0..=path.len() {
                if glob_match_segments(&pattern[1..], &path[i..]) {
                    return true;
                }
            }
            false
        }
        seg => {
            if path.is_empty() {
                return false;
            }
            if wildcard_match(seg, path[0]) {
                glob_match_segments(&pattern[1..], &path[1..])
            } else {
                false
            }
        }
    }
}

/// Match a single path segment against a pattern segment (supports `*`).
fn wildcard_match(pattern: &str, segment: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == segment;
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !segment.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            if !segment[pos..].ends_with(part) {
                return false;
            }
        } else {
            match segment[pos..].find(part) {
                Some(idx) => pos += idx + part.len(),
                None => return false,
            }
        }
    }
    true
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclude_glob_target() {
        assert!(is_excluded("target/debug/foo.rs", &["target/**".into()]));
        assert!(!is_excluded("src/lib.rs", &["target/**".into()]));
    }

    #[test]
    fn include_glob_src_only() {
        let includes = vec!["src/**".into()];
        assert!(is_included("src/main.rs", &includes));
        assert!(!is_included("tests/foo.rs", &includes));
    }

    #[test]
    fn glob_matches_double_star() {
        assert!(glob_matches("target/**", "target/debug/foo.rs"));
        assert!(glob_matches("target/**", "target/foo.rs"));
        assert!(!glob_matches("target/**", "src/foo.rs"));
    }

    #[test]
    fn glob_matches_star() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "main.ts"));
    }

    #[tokio::test]
    async fn unknown_extension_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), "foo: bar").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        // YAML file must not produce any chunks; .rs file should.
        assert!(!chunks.is_empty(), "should have chunks from main.rs");
        for c in &chunks {
            assert!(
                !c.location.uri.contains(".yaml"),
                "YAML file should not produce chunks: {}",
                c.location.uri
            );
        }
    }

    #[tokio::test]
    async fn min_chunk_bytes_drops_tiny_items() {
        let dir = tempfile::tempdir().unwrap();
        // Very short function — will be below min_chunk_bytes threshold.
        std::fs::write(dir.path().join("tiny.rs"), "fn x() {}").unwrap();

        let mut opts = ChunkOptions::default();
        opts.min_chunk_bytes = 50; // "fn x() {}" is 10 bytes → dropped

        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        // The file chunk is always emitted; item chunks of tiny functions are dropped.
        let item_chunks: Vec<_> = chunks.iter().filter(|c| c.kind != "file").collect();
        assert!(
            item_chunks.is_empty(),
            "tiny item chunk should be dropped, got {} item chunks",
            item_chunks.len()
        );
    }

    #[tokio::test]
    async fn include_glob_filters_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn foo() -> u32 { let _ = 1; 0 }",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tests/test.rs"),
            "fn test_foo() { assert!(true); }",
        )
        .unwrap();

        let mut opts = ChunkOptions::default();
        opts.include_globs = vec!["src/**".into()];

        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        // No chunk should come from the tests directory.
        for c in &chunks {
            assert!(
                !c.location.uri.contains("/tests/"),
                "tests/ directory should be excluded, got: {}",
                c.location.uri
            );
        }
    }
}
