use std::path::{Path, PathBuf};

use anyhow::Result;
use callimachus_core::types::{Chunk, Location};
use tree_sitter::{Parser, Query, QueryCursor};
use walkdir::WalkDir;

use crate::languages::{self, LangConfig, TEXT_EXTENSIONS};

/// Files larger than this are truncated before being stored as a text chunk.
const MAX_TEXT_FILE_BYTES: usize = 256 * 1024;

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
    /// If true, do not use git index to enumerate files even when the
    /// source path is a git repository. Defaults to false (git-aware).
    pub no_git_filter: bool,
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
            no_git_filter: false,
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

// ── File enumeration ─────────────────────────────────────────────────────────

/// Return candidate absolute file paths under `source_path`.
///
/// When the directory is a git repository and `opts.no_git_filter` is false,
/// the git index is used so that untracked files (build artefacts, etc.) are
/// excluded automatically.  For non-git directories, or when `no_git_filter`
/// is true, a plain `walkdir` traversal is used instead.
fn enumerate_files(source_path: &Path, opts: &ChunkOptions) -> Vec<PathBuf> {
    if !opts.no_git_filter {
        match git2::Repository::open(source_path) {
            Ok(repo) => match repo.index() {
                Ok(index) => {
                    // Build absolute paths anchored at source_path rather than
                    // repo.workdir() so that strip_prefix(source_path) is always
                    // consistent (avoids symlink-resolution mismatches on macOS).
                    let files: Vec<PathBuf> = index
                        .iter()
                        .filter_map(|entry| {
                            std::str::from_utf8(&entry.path)
                                .ok()
                                .map(|s| source_path.join(s))
                        })
                        .filter(|p| p.is_file())
                        .collect();
                    tracing::info!(
                        "[chunk] git repo detected — indexing {} tracked files",
                        files.len()
                    );
                    return files;
                }
                Err(e) => {
                    tracing::warn!(
                        "[chunk] git repo found but index unreadable ({e}); falling back to filesystem walk"
                    );
                }
            },
            Err(_) => {
                tracing::info!("[chunk] not a git repo, using filesystem walk");
            }
        }
    }

    WalkDir::new(source_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect()
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

    let files = enumerate_files(source_path, opts);
    for abs_path in files {
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

        // Read file contents.
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("could not read {}: {e}", abs_path.display());
                continue;
            }
        };

        // Vue SFCs get special handling: extract their script block and parse as
        // TypeScript, but also always emit a file-level chunk for the raw .vue content.
        if ext == "vue" {
            let file_chunks = chunk_vue_file(corpus_id, &rel_str, &content, opts);
            chunks.extend(file_chunks);
            continue;
        }

        let lang = match languages::for_extension(ext) {
            Some(l) => l,
            None => {
                // Text files without a tree-sitter grammar get a single file-level chunk.
                if is_text_extension(ext)
                    && let Some(chunk) = emit_text_file_chunk(&abs_path, corpus_id, &rel_str)
                {
                    chunks.push(chunk);
                }
                continue;
            }
        };

        // Chunk this file.
        let file_chunks = chunk_file(corpus_id, &rel_str, &content, lang, opts);
        chunks.extend(file_chunks);
    }

    Ok(chunks)
}

// ── Vue SFC chunking ─────────────────────────────────────────────────────────

/// Chunk a `.vue` Single-File Component.
///
/// Always emits a file-level chunk for the raw `.vue` content.  If the file
/// contains a `<script>` block, the script body is also parsed as TypeScript
/// (or TSX) and item-level chunks are emitted with URIs like
/// `src/path/Foo.vue#symbol`.
fn chunk_vue_file(
    corpus_id: &str,
    rel_path: &str,
    content: &str,
    opts: &ChunkOptions,
) -> Vec<Chunk> {
    let mut chunks = Vec::new();

    // Always emit a file chunk for the raw .vue content.
    let file_uri_path = format!("src/{rel_path}");
    let file_location = Location::new(corpus_id, &file_uri_path);
    let file_chunk = Chunk::new(
        corpus_id.to_string(),
        None,
        "file".to_string(),
        file_location,
        content.to_string(),
    );
    let file_chunk_uri = file_chunk.location.uri.clone();
    chunks.push(file_chunk);

    // Extract the script block and parse it as TypeScript.
    let (script_body, _is_tsx) = match crate::vue::extract_script_block(content) {
        Some(pair) => pair,
        None => return chunks, // template-only .vue: just the file chunk
    };

    let ts_lang_name = "typescript";
    let lang = match languages::for_name(ts_lang_name) {
        Some(l) => l,
        None => return chunks,
    };

    // Parse the script body for item chunks.
    let mut parser = tree_sitter::Parser::new();
    let language = (lang.language_fn)();
    if parser.set_language(&language).is_err() {
        return chunks;
    }
    let tree = match parser.parse(&script_body, None) {
        Some(t) => t,
        None => return chunks,
    };
    let query = match tree_sitter::Query::new(&language, lang.top_level_query) {
        Ok(q) => q,
        Err(_) => return chunks,
    };

    let items: Vec<ItemInfo> = {
        let mut cursor = tree_sitter::QueryCursor::new();
        let source_bytes = script_body.as_bytes();
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
        let item_text = match script_body.get(item.byte_range.clone()) {
            Some(t) => t,
            None => continue,
        };

        let symbol = extract_symbol_from_text(item_text, &item.node_kind, lang);
        let item_path = if let Some(sym) = &symbol {
            format!("src/{rel_path}#{sym}")
        } else {
            format!("src/{rel_path}#item_{}", item.start_row + 1)
        };

        let item_kind = node_kind_to_chunk_kind(&item.node_kind);

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
            vec![]
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
        "trait_item" | "interface_declaration" | "trait_declaration" => "interface",
        "mod_item" | "module" | "namespace" | "namespace_definition" => "module",
        "type_declaration" | "type_alias_declaration" => "interface",
        "export_statement" => "module",
        // PHP enum → treat as interface (no separate enum chunk kind).
        "enum_declaration" => "interface",
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
        "trait_item" | "interface_declaration" | "trait_declaration" => &["trait", "interface"],
        "mod_item" | "namespace_definition" => &["mod", "namespace"],
        "type_declaration" | "type_alias_declaration" => &["type"],
        "enum_declaration" => &["enum"],
        _ => &[
            "fn",
            "func",
            "function",
            "def",
            "class",
            "struct",
            "impl",
            "namespace",
            "enum",
        ],
    };

    // Find the first keyword and take the token after it.
    for (i, token) in tokens.iter().enumerate() {
        let clean = token.trim_start_matches("pub").trim_start_matches(' ');
        if (keywords.contains(&clean) || keywords.contains(token))
            && let Some(next) = tokens.get(i + 1)
        {
            // Extract the leading identifier portion of the next token.
            // This handles cases like `foo(args` where the opening paren is
            // attached to the symbol name (e.g. PHP, some JS patterns).
            let sym = next.trim_start_matches('<');
            let sym = sym
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("");
            if !sym.is_empty()
                && sym
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                && sym.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
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

// ── Text passthrough helpers ─────────────────────────────────────────────────

/// Returns true when `ext` is in the text-passthrough extension list.
fn is_text_extension(ext: &str) -> bool {
    TEXT_EXTENSIONS.contains(&ext)
}

/// Read `abs_path` and return a single file-level chunk.
///
/// Files exceeding [`MAX_TEXT_FILE_BYTES`] are truncated and a marker is
/// appended.  `byte_length` on the returned chunk reflects the actual
/// (pre-truncation) file size.
fn emit_text_file_chunk(abs_path: &Path, corpus_id: &str, rel_str: &str) -> Option<Chunk> {
    let raw = match std::fs::read(abs_path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("could not read {}: {e}", abs_path.display());
            return None;
        }
    };

    let actual_byte_length = raw.len();

    let content = if actual_byte_length > MAX_TEXT_FILE_BYTES {
        let truncated = match std::str::from_utf8(&raw[..MAX_TEXT_FILE_BYTES]) {
            Ok(s) => s.to_string(),
            Err(e) => {
                // Back off to the last valid UTF-8 boundary.
                let valid_up_to = e.valid_up_to();
                std::str::from_utf8(&raw[..valid_up_to])
                    .unwrap_or("")
                    .to_string()
            }
        };
        format!("{truncated}\n\n[truncated: file exceeds 256kb]")
    } else {
        match String::from_utf8(raw) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("could not decode {} as UTF-8: {e}", abs_path.display());
                return None;
            }
        }
    };

    let file_uri_path = format!("src/{rel_str}");
    let location = Location::new(corpus_id, &file_uri_path);

    let mut chunk = Chunk::new(
        corpus_id.to_string(),
        None,
        "file".to_string(),
        location,
        content,
    );
    // Override byte_length to reflect the actual pre-truncation file size.
    chunk.byte_length = actual_byte_length;

    Some(chunk)
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
    async fn truly_unknown_extension_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("weird.xyz"), "some content").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        // .xyz file must not produce any chunks; .rs file should.
        assert!(!chunks.is_empty(), "should have chunks from main.rs");
        for c in &chunks {
            assert!(
                !c.location.uri.contains(".xyz"),
                ".xyz file should not produce chunks: {}",
                c.location.uri
            );
        }
    }

    #[tokio::test]
    async fn text_json_file_produces_file_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let json_content = r#"{"a":1}"#;
        std::fs::write(dir.path().join("data.json"), json_content).unwrap();

        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        assert_eq!(chunks.len(), 1, "expected exactly one chunk for JSON file");
        let chunk = &chunks[0];
        assert_eq!(chunk.kind, "file");
        assert_eq!(chunk.content, json_content);
        assert!(chunk.location.uri.contains("data.json"));
    }

    #[tokio::test]
    async fn text_large_file_is_truncated() {
        let dir = tempfile::tempdir().unwrap();
        // Create content just over 256kb.
        let large_content = "x".repeat(MAX_TEXT_FILE_BYTES + 1024);
        std::fs::write(dir.path().join("big.md"), &large_content).unwrap();

        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        assert_eq!(chunks.len(), 1, "expected exactly one chunk for large file");
        let chunk = &chunks[0];
        assert!(
            chunk.content.ends_with("[truncated: file exceeds 256kb]"),
            "expected truncation marker, got ending: {:?}",
            &chunk.content[chunk.content.len().saturating_sub(50)..]
        );
        assert_eq!(chunk.byte_length, large_content.len());
    }

    #[tokio::test]
    async fn text_passthrough_no_sub_chunks() {
        let dir = tempfile::tempdir().unwrap();
        // A shell file with function-like syntax — must not produce sub-chunks.
        let sh_content = "#!/bin/bash\nmy_func() {\n  echo hello\n}\nmy_func\n";
        std::fs::write(dir.path().join("script.sh"), sh_content).unwrap();

        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        assert_eq!(
            chunks.len(),
            1,
            "shell file should produce exactly one chunk"
        );
        assert_eq!(chunks[0].kind, "file");
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
    async fn php_file_produces_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let php_content = r#"<?php
function foo(int $x): int {
    return $x * 2;
}

class Bar {
    public function baz(): void {}
}
"#;
        std::fs::write(dir.path().join("example.php"), php_content).unwrap();

        let opts = ChunkOptions {
            min_chunk_bytes: 10,
            ..Default::default()
        };
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        let item_chunks: Vec<_> = chunks.iter().filter(|c| c.kind != "file").collect();
        assert!(
            item_chunks.len() >= 2,
            "expected ≥2 item chunks from PHP file (function + class), got {}: {:?}",
            item_chunks.len(),
            item_chunks
                .iter()
                .map(|c| &c.location.uri)
                .collect::<Vec<_>>()
        );

        let uris: Vec<_> = item_chunks.iter().map(|c| &c.location.uri).collect();
        assert!(
            uris.iter().any(|u| u.contains("#foo")),
            "expected #foo chunk, got: {:?}",
            uris
        );
        assert!(
            uris.iter().any(|u| u.contains("#Bar")),
            "expected #Bar chunk, got: {:?}",
            uris
        );
    }

    #[tokio::test]
    async fn vue_sfc_extracts_script() {
        let dir = tempfile::tempdir().unwrap();
        let vue_content = r#"<template><div>hello</div></template>
<script setup lang="ts">
function greet(): string {
    return "hello world from greet function";
}
</script>
"#;
        std::fs::write(dir.path().join("Foo.vue"), vue_content).unwrap();

        let opts = ChunkOptions {
            min_chunk_bytes: 10,
            ..Default::default()
        };
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        let file_chunks: Vec<_> = chunks.iter().filter(|c| c.kind == "file").collect();
        let item_chunks: Vec<_> = chunks.iter().filter(|c| c.kind != "file").collect();

        assert_eq!(file_chunks.len(), 1, "expected exactly 1 file chunk");
        assert!(
            !item_chunks.is_empty(),
            "expected item chunks from .vue script"
        );

        let uris: Vec<_> = item_chunks.iter().map(|c| &c.location.uri).collect();
        assert!(
            uris.iter().any(|u| u.contains("#greet")),
            "expected #greet chunk, got: {:?}",
            uris
        );
    }

    #[tokio::test]
    async fn vue_sfc_without_script_still_emits_file_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let vue_content = r#"<template>
  <div class="hello">
    <h1>{{ msg }}</h1>
  </div>
</template>
<style scoped>
h1 { color: red; }
</style>
"#;
        std::fs::write(dir.path().join("NoScript.vue"), vue_content).unwrap();

        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        assert_eq!(
            chunks.len(),
            1,
            "template-only .vue should produce exactly 1 chunk, got {}: {:?}",
            chunks.len(),
            chunks.iter().map(|c| &c.location.uri).collect::<Vec<_>>()
        );
        assert_eq!(chunks[0].kind, "file");
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

    // ── Git-filter tests ──────────────────────────────────────────────────────

    fn init_repo_with(dir: &std::path::Path, tracked: &[(&str, &str)], untracked: &[(&str, &str)]) {
        let repo = git2::Repository::init(dir).unwrap();
        for (name, content) in tracked {
            std::fs::write(dir.join(name), content).unwrap();
        }
        for (name, content) in untracked {
            std::fs::write(dir.join(name), content).unwrap();
        }
        let mut index = repo.index().unwrap();
        for (name, _) in tracked {
            index.add_path(std::path::Path::new(name)).unwrap();
        }
        index.write().unwrap();
    }

    #[tokio::test]
    async fn git_filter_excludes_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with(
            dir.path(),
            &[("tracked.rs", "fn tracked() { let _ = 1; }")],
            &[("untracked.rs", "fn untracked() { let _ = 2; }")],
        );
        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();
        assert!(chunks.iter().any(|c| c.location.uri.contains("tracked.rs")));
        assert!(
            !chunks
                .iter()
                .any(|c| c.location.uri.contains("untracked.rs"))
        );
    }

    #[tokio::test]
    async fn no_git_filter_includes_all_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with(
            dir.path(),
            &[("tracked.rs", "fn tracked() { let _ = 1; }")],
            &[("untracked.rs", "fn untracked() { let _ = 2; }")],
        );
        let opts = ChunkOptions {
            no_git_filter: true,
            ..ChunkOptions::default()
        };
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();
        assert!(chunks.iter().any(|c| c.location.uri.contains("tracked.rs")));
        assert!(
            chunks
                .iter()
                .any(|c| c.location.uri.contains("untracked.rs"))
        );
    }

    #[tokio::test]
    async fn non_git_dir_falls_back_to_walkdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() { let _ = 1; }").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() { let _ = 2; }").unwrap();
        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();
        assert!(chunks.iter().any(|c| c.location.uri.contains("a.rs")));
        assert!(chunks.iter().any(|c| c.location.uri.contains("b.rs")));
    }
}
