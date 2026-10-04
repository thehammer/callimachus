use std::path::{Path, PathBuf};

use anyhow::Result;
use callimachus_adapter_contract::{Chunk, Location};
use tree_sitter::{Parser, Query, QueryCursor};
use walkdir::WalkDir;

use crate::languages::{self, DetectionKind, LangConfig};

/// Files larger than this are truncated before being stored as a text chunk.
const MAX_TEXT_FILE_BYTES: usize = 256 * 1024;

/// Files whose extension did not decide their language are only considered
/// (shebang / well-known filename sniffing) when they are at most this big.
const MAX_SNIFF_FILE_BYTES: u64 = 1024 * 1024;

/// How much of a sniffed file is read to look for a NUL byte (binary check)
/// and the shebang line.
const SNIFF_HEAD_BYTES: u64 = 8 * 1024;

/// Default file globs that are always excluded from chunking. Per-corpus
/// `exclude_globs` from corpus metadata are appended to (not substituted
/// for) this list.
pub const DEFAULT_EXCLUDE_GLOBS: &[&str] = &[
    ".git/**",
    ".claude/**",
    "vendor/**",
    "node_modules/**",
    "storage/**",
    "bootstrap/cache/**",
    "public/build/**",
    "target/**",
    "dist/**",
    "build/**",
    // Flutter-generated files (build_runner / json_serializable / freezed).
    // These are machine-generated and add noise; excluded via explicit suffix
    // check in chunk_directory AND here for glob-based filtering.
    "**/*.g.dart",
    "**/*.freezed.dart",
];

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
            exclude_globs: DEFAULT_EXCLUDE_GLOBS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
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
    end_row: usize,
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
/// The language of each file is detected by [`languages::detect`]: extension,
/// then shebang, then well-known filename, then a plain-text extension list.
/// Files matching none of these, binaries (NUL byte in the first 8 KiB) and
/// extensionless files over 1 MiB are skipped silently.  Every emitted chunk
/// carries the detected language label in `Chunk::language`.
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

        // Skip Flutter-generated Dart files (build_runner / freezed).
        // These are machine-generated and add noise to the index.  The glob
        // entries in DEFAULT_EXCLUDE_GLOBS also cover this, but an explicit
        // suffix check is faster and clearer.
        if rel_str.ends_with(".g.dart") || rel_str.ends_with(".freezed.dart") {
            tracing::debug!("[chunk] skipping Flutter-generated file: {rel_str}");
            continue;
        }

        // Apply include globs (if any).
        if !opts.include_globs.is_empty() && !is_included(&rel_str, &opts.include_globs) {
            continue;
        }

        // Detect the language.  The extension decides first and needs no I/O;
        // only files it cannot decide (no / unknown extension) are sniffed for a
        // shebang, and binaries / oversized files are dropped before any full read.
        let detection = match languages::detect_by_extension(&rel_str) {
            Some(d) => d,
            None => {
                let Some(head) = sniff_head(&abs_path, &rel_str) else {
                    continue;
                };
                match languages::detect(&rel_str, Some(&head)) {
                    Some(d) => d,
                    None => continue,
                }
            }
        };

        let mut file_chunks = match detection.kind {
            // Text files without a tree-sitter grammar get a single file-level chunk.
            DetectionKind::Text => emit_text_file_chunk(&abs_path, corpus_id, &rel_str)
                .into_iter()
                .collect(),
            DetectionKind::Grammar(lang) => match read_source(&abs_path) {
                Some(content) => chunk_file(corpus_id, &rel_str, &content, lang, opts),
                None => continue,
            },
            // Vue SFCs: a file chunk for the raw .vue plus item chunks from the
            // script block parsed as TypeScript.
            DetectionKind::Vue => match read_source(&abs_path) {
                Some(content) => chunk_vue_file(corpus_id, &rel_str, &content, opts),
                None => continue,
            },
        };

        // Persist the detected language on every chunk so later passes, which
        // only see a URI or a chunk body, resolve the same language.
        for chunk in &mut file_chunks {
            chunk.language = Some(detection.label.to_string());
        }
        chunks.extend(file_chunks);
    }

    Ok(chunks)
}

fn read_source(abs_path: &Path) -> Option<String> {
    match std::fs::read_to_string(abs_path) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("could not read {}: {e}", abs_path.display());
            None
        }
    }
}

/// Read the first line of a file whose extension did not decide its language.
///
/// Returns `None` (file skipped) when the file is over [`MAX_SNIFF_FILE_BYTES`],
/// unreadable, or looks binary (NUL byte within the first [`SNIFF_HEAD_BYTES`]).
/// Binaries under `bin/` are expected, so skips log at debug level only.
fn sniff_head(abs_path: &Path, rel_str: &str) -> Option<String> {
    use std::io::Read;

    let len = match std::fs::metadata(abs_path) {
        Ok(m) => m.len(),
        Err(e) => {
            tracing::debug!("[chunk] cannot stat {rel_str}: {e}");
            return None;
        }
    };
    if len > MAX_SNIFF_FILE_BYTES {
        tracing::debug!("[chunk] skipping {rel_str}: {len} bytes exceeds sniff limit");
        return None;
    }

    let mut buf = Vec::new();
    let read =
        std::fs::File::open(abs_path).and_then(|f| f.take(SNIFF_HEAD_BYTES).read_to_end(&mut buf));
    if let Err(e) = read {
        tracing::debug!("[chunk] cannot read {rel_str}: {e}");
        return None;
    }
    if buf.contains(&0) {
        tracing::debug!("[chunk] skipping binary file {rel_str}");
        return None;
    }

    let line_end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..line_end]).into_owned())
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
    let file_chunk_uri = file_chunk.location.uri();
    chunks.push(file_chunk);

    // Extract the script block and parse it as TypeScript.
    let (script_body, _is_tsx, script_line_offset) =
        match crate::vue::extract_script_block_with_line_offset(content) {
            Some(triple) => triple,
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
                        end_row: c.node.end_position().row,
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

        // Vue SFC spans are file-relative: add the script block's line offset.
        let file_start_row = script_line_offset + item.start_row;
        let file_end_row = script_line_offset + item.end_row;

        let item_chunks = if item_text.len() > opts.max_chunk_bytes {
            // Split chunks don't carry precise spans (lines are arbitrary slices).
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
            let mut chunk = Chunk::new(
                corpus_id.to_string(),
                Some(file_chunk_uri.clone()),
                item_kind.to_string(),
                loc,
                item_text.to_string(),
            );
            chunk.start_line = Some(file_start_row as u32);
            chunk.end_line = Some(file_end_row as u32);
            vec![chunk]
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
    let file_chunk_uri = file_chunk.location.uri();
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
    let raw_items: Vec<ItemInfo> = {
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
                        end_row: c.node.end_position().row,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    // Dart: top-level functions are split into sibling `function_signature` +
    // `function_body` nodes under `program`.  The query captures only the
    // signature (no containing wrapper exists), so we extend its byte range to
    // include the immediately-following body node.  Without this, the signature
    // alone is typically < min_chunk_bytes and gets dropped silently.
    let items: Vec<ItemInfo> = if lang.name == "dart" {
        // Build a map: signature_start_byte → (body_end_byte, body_end_row)
        let mut body_ext: std::collections::HashMap<usize, (usize, usize)> =
            std::collections::HashMap::new();
        let root_node = tree.root_node();
        let mut walker = root_node.walk();
        for child in root_node.children(&mut walker) {
            if child.kind() == "function_signature"
                && let Some(next) = child.next_sibling()
                && next.kind() == "function_body"
            {
                body_ext.insert(
                    child.byte_range().start,
                    (next.byte_range().end, next.end_position().row),
                );
            }
        }
        raw_items
            .into_iter()
            .map(|item| {
                if item.node_kind == "function_signature"
                    && let Some(&(end_byte, end_row)) = body_ext.get(&item.byte_range.start)
                {
                    return ItemInfo {
                        byte_range: item.byte_range.start..end_byte,
                        end_row,
                        node_kind: item.node_kind,
                        start_row: item.start_row,
                    };
                }
                item
            })
            .collect()
    } else {
        raw_items
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
            // Split chunks don't carry precise spans (lines are arbitrary slices).
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
            let mut chunk = Chunk::new(
                corpus_id.to_string(),
                Some(file_chunk_uri.clone()),
                item_kind.to_string(),
                loc,
                item_text.to_string(),
            );
            chunk.start_line = Some(item.start_row as u32);
            chunk.end_line = Some(item.end_row as u32);
            vec![chunk]
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
        // Dart top-level functions use `function_signature` (body is a sibling node).
        "function_signature" => "function",
        "impl_item" | "struct_item" | "class_declaration" | "class_definition" => "class",
        // Dart: mixins and extensions map to `class` (closest existing chunk kind).
        "mixin_declaration" | "extension_declaration" => "class",
        "trait_item" | "interface_declaration" | "trait_declaration" => "interface",
        "mod_item" | "module" | "namespace" | "namespace_definition" => "module",
        "type_declaration" | "type_alias_declaration" => "interface",
        "export_statement" => "module",
        // PHP enum → treat as interface (no separate enum chunk kind).
        // Dart enum_declaration also maps here (no separate `enum` chunk kind).
        "enum_declaration" => "interface",
        _ => "function",
    }
}

/// Extract a human-readable symbol name from a text slice using a simple heuristic.
///
/// Parses the first token after common keywords.
fn extract_symbol_from_text(text: &str, node_kind: &str, lang: &LangConfig) -> Option<String> {
    // Shell functions and Makefile rules have their own header syntax; the
    // keyword scan below would pick up words from their bodies.
    match lang.name {
        "bash" => return extract_bash_function_name(text),
        "make" => return extract_make_target_name(text),
        _ => {}
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    // Dart `function_signature` special case: the symbol name is the last identifier
    // before the opening parenthesis.  Return types (void, String, int, …) precede
    // the name and vary, so the keyword-list heuristic below doesn't apply.
    if node_kind == "function_signature" {
        if let Some(paren_pos) = text.find('(') {
            let before_paren = &text[..paren_pos];
            let last_ident: String = before_paren
                .split_whitespace()
                .last()
                .map(|t| {
                    t.chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect()
                })
                .unwrap_or_default();
            if !last_ident.is_empty()
                && last_ident
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                return Some(last_ident);
            }
        }
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
        // Dart: mixins and extensions follow the same pattern as class/struct.
        "mixin_declaration" => &["mixin"],
        "extension_declaration" => &["extension"],
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

/// Name of a shell function from its definition text: handles `foo() {`,
/// `function foo {` and `function foo() {`.
fn extract_bash_function_name(text: &str) -> Option<String> {
    let text = text.trim_start();
    let rest = text
        .strip_prefix("function")
        .filter(|r| r.starts_with(char::is_whitespace))
        .unwrap_or(text)
        .trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '(' && *c != '{')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// First target of a Makefile rule (`build: deps` → `build`).  Targets built
/// from variables or pattern wildcards (`$(OBJ)`, `%.o`) yield `None` so the
/// caller falls back to a positional name.
fn extract_make_target_name(text: &str) -> Option<String> {
    let header = text.lines().next()?;
    let target = header.split(':').next()?.split_whitespace().next()?;
    target
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        .then(|| target.to_string())
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
                !c.location.uri().contains(".xyz"),
                ".xyz file should not produce chunks: {}",
                c.location.uri()
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
        assert!(chunk.location.uri().contains("data.json"));
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
    async fn sh_file_is_grammar_chunked_into_function_chunks() {
        let dir = tempfile::tempdir().unwrap();
        // A shell file is parsed with the bash grammar: one file chunk plus one
        // chunk per function.
        let sh_content = "#!/bin/bash\nmy_func() {\n  echo hello\n}\nmy_func\n";
        std::fs::write(dir.path().join("script.sh"), sh_content).unwrap();

        let opts = ChunkOptions {
            min_chunk_bytes: 10,
            ..ChunkOptions::default()
        };
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        let file_chunks: Vec<_> = chunks.iter().filter(|c| c.kind == "file").collect();
        assert_eq!(file_chunks.len(), 1, "expected exactly one file chunk");
        assert_eq!(file_chunks[0].location.path, "src/script.sh");
        assert_eq!(file_chunks[0].language.as_deref(), Some("bash"));

        let func = chunks
            .iter()
            .find(|c| c.location.path == "src/script.sh#my_func")
            .unwrap_or_else(|| panic!("expected a #my_func chunk, got {:?}", paths(&chunks)));
        assert_eq!(func.kind, "function");
        assert_eq!(func.language.as_deref(), Some("bash"));
    }

    #[tokio::test]
    async fn yaml_file_stays_text_passthrough_with_single_chunk() {
        let dir = tempfile::tempdir().unwrap();
        // YAML that merely looks function-like must not be split into sub-chunks.
        let yaml = "name: demo\nsteps:\n  - run: my_func() { echo hello; }\n";
        std::fs::write(dir.path().join("ci.yaml"), yaml).unwrap();

        let chunks = chunk_directory(dir.path(), "test", &ChunkOptions::default())
            .await
            .unwrap();

        assert_eq!(chunks.len(), 1, "yaml should produce exactly one chunk");
        assert_eq!(chunks[0].kind, "file");
        assert_eq!(chunks[0].content, yaml);
        assert_eq!(chunks[0].language.as_deref(), Some("text"));
    }

    #[tokio::test]
    async fn min_chunk_bytes_drops_tiny_items() {
        let dir = tempfile::tempdir().unwrap();
        // Very short function — will be below min_chunk_bytes threshold.
        std::fs::write(dir.path().join("tiny.rs"), "fn x() {}").unwrap();

        let opts = ChunkOptions {
            min_chunk_bytes: 50, // "fn x() {}" is 10 bytes → dropped
            ..ChunkOptions::default()
        };

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
                .map(|c| c.location.uri())
                .collect::<Vec<_>>()
        );

        let uris: Vec<_> = item_chunks.iter().map(|c| c.location.uri()).collect();
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

        let uris: Vec<_> = item_chunks.iter().map(|c| c.location.uri()).collect();
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
            chunks.iter().map(|c| c.location.uri()).collect::<Vec<_>>()
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

        let opts = ChunkOptions {
            include_globs: vec!["src/**".into()],
            ..ChunkOptions::default()
        };

        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        // No chunk should come from the tests directory.
        for c in &chunks {
            assert!(
                !c.location.uri().contains("/tests/"),
                "tests/ directory should be excluded, got: {}",
                c.location.uri()
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
        assert!(
            chunks
                .iter()
                .any(|c| c.location.uri().contains("tracked.rs"))
        );
        assert!(
            !chunks
                .iter()
                .any(|c| c.location.uri().contains("untracked.rs"))
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
        assert!(
            chunks
                .iter()
                .any(|c| c.location.uri().contains("tracked.rs"))
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.location.uri().contains("untracked.rs"))
        );
    }

    #[tokio::test]
    async fn non_git_dir_falls_back_to_walkdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() { let _ = 1; }").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() { let _ = 2; }").unwrap();
        let opts = ChunkOptions::default();
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();
        assert!(chunks.iter().any(|c| c.location.uri().contains("a.rs")));
        assert!(chunks.iter().any(|c| c.location.uri().contains("b.rs")));
    }

    #[tokio::test]
    async fn dart_generated_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        // Hand-written Dart file — should be chunked.
        std::fs::write(
            dir.path().join("contact.dart"),
            r#"class Contact { void save() {} }"#,
        )
        .unwrap();
        // build_runner generated file — must be excluded.
        std::fs::write(
            dir.path().join("contact.g.dart"),
            "// GENERATED CODE - DO NOT MODIFY BY HAND\nclass _$Contact {}",
        )
        .unwrap();
        // freezed generated file — must be excluded.
        std::fs::write(
            dir.path().join("contact.freezed.dart"),
            "// coverage:ignore-file\nclass _$ContactCopyWith {}",
        )
        .unwrap();

        let opts = ChunkOptions {
            min_chunk_bytes: 1,
            ..ChunkOptions::default()
        };
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        // No chunk should reference a .g.dart or .freezed.dart file.
        for c in &chunks {
            let uri = c.location.uri();
            assert!(
                !uri.ends_with(".g.dart"),
                ".g.dart file must produce zero chunks, got: {uri}"
            );
            assert!(
                !uri.contains(".g.dart#"),
                ".g.dart symbol chunk must not appear: {uri}"
            );
            assert!(
                !uri.ends_with(".freezed.dart"),
                ".freezed.dart file must produce zero chunks, got: {uri}"
            );
        }

        // The hand-written contact.dart must still produce chunks.
        assert!(
            chunks
                .iter()
                .any(|c| c.location.uri().contains("contact.dart")
                    && !c.location.uri().contains(".g.dart")
                    && !c.location.uri().contains(".freezed.dart")),
            "contact.dart should produce at least one chunk; URIs: {:?}",
            chunks.iter().map(|c| c.location.uri()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn dart_file_produces_class_and_function_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let dart_content = r#"import 'package:meta/meta.dart';

class Contact {
  final String id;
  Contact(this.id);
  void save() {}
}

void main() {
  final c = Contact('1');
  c.save();
}
"#;
        std::fs::write(dir.path().join("main.dart"), dart_content).unwrap();

        let opts = ChunkOptions {
            min_chunk_bytes: 10,
            ..ChunkOptions::default()
        };
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        let item_chunks: Vec<_> = chunks.iter().filter(|c| c.kind != "file").collect();
        assert!(
            !item_chunks.is_empty(),
            "Dart file should produce item chunks; all chunks: {:?}",
            chunks
                .iter()
                .map(|c| format!("{}:{}", c.kind, c.location.uri()))
                .collect::<Vec<_>>()
        );

        let uris: Vec<_> = item_chunks.iter().map(|c| c.location.uri()).collect();
        assert!(
            uris.iter().any(|u| u.contains("#Contact")),
            "expected #Contact chunk; item URIs: {:?}",
            uris
        );
        assert!(
            uris.iter().any(|u| u.contains("#main")),
            "expected #main chunk; item URIs: {:?}",
            uris
        );
    }

    #[test]
    fn default_excludes_cover_known_dirs() {
        let defaults: Vec<String> = DEFAULT_EXCLUDE_GLOBS
            .iter()
            .map(|s| (*s).to_string())
            .collect();

        // These paths must be excluded by the default globs.
        let should_exclude = [
            ".claude/worktrees/foo/bar.rs",
            "vendor/laravel/framework/src/Foo.php",
            "storage/logs/laravel.log",
            "bootstrap/cache/config.php",
            "public/build/assets/app.js",
            "node_modules/foo/index.js",
            "target/debug/build/x.rs",
            ".git/HEAD",
        ];
        for path in &should_exclude {
            assert!(
                is_excluded(path, &defaults),
                "expected {path:?} to be excluded by default globs"
            );
        }

        // These paths must NOT be excluded by the default globs.
        let should_not_exclude = ["src/lib.rs", "app/Http/Controllers/FooController.php"];
        for path in &should_not_exclude {
            assert!(
                !is_excluded(path, &defaults),
                "expected {path:?} to NOT be excluded by default globs"
            );
        }
    }

    // ── Language detection (shebang / filename / allow-list) ──────────────────

    fn paths(chunks: &[Chunk]) -> Vec<String> {
        chunks.iter().map(|c| c.location.path.clone()).collect()
    }

    fn small_opts() -> ChunkOptions {
        ChunkOptions {
            min_chunk_bytes: 10,
            ..ChunkOptions::default()
        }
    }

    fn write(dir: &Path, rel: &str, content: impl AsRef<[u8]>) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    /// Chunks belonging to `file_path` (the file chunk and all its item chunks).
    fn chunks_of<'a>(chunks: &'a [Chunk], file_path: &str) -> Vec<&'a Chunk> {
        chunks
            .iter()
            .filter(|c| c.location.path.split('#').next() == Some(file_path))
            .collect()
    }

    fn item_names(chunks: &[Chunk], file_path: &str) -> Vec<String> {
        chunks
            .iter()
            .filter_map(|c| {
                let (file, frag) = c.location.path.split_once('#')?;
                (file == file_path).then(|| frag.to_string())
            })
            .collect()
    }

    const BASH_SCRIPT: &str = "#!/usr/bin/env bash\n\
        set -euo pipefail\n\
        \n\
        foo() {\n  echo \"foo ran\"\n}\n\
        \n\
        function bar {\n  echo \"bar ran\"\n}\n\
        \n\
        function baz() {\n  foo\n  bar\n}\n\
        \n\
        baz\n";

    #[tokio::test]
    async fn extensionless_bash_script_is_chunked_with_bash_grammar() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "bin/tool", BASH_SCRIPT);

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        let file_chunks: Vec<_> = chunks.iter().filter(|c| c.kind == "file").collect();
        assert_eq!(file_chunks.len(), 1, "got {:?}", paths(&chunks));
        assert_eq!(file_chunks[0].location.path, "src/bin/tool");
        assert_eq!(file_chunks[0].content, BASH_SCRIPT);

        let names = item_names(&chunks, "src/bin/tool");
        for expected in ["foo", "bar", "baz"] {
            assert!(
                names.iter().any(|n| n == expected),
                "expected a #{expected} function chunk (covers `name() {{`, `function name {{` and `function name() {{`), got {names:?}"
            );
        }
        for c in chunks.iter().filter(|c| c.kind != "file") {
            assert_eq!(c.kind, "function", "{}", c.location.path);
        }
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("bash"), "{}", c.location.path);
        }
    }

    #[tokio::test]
    async fn bash_function_name_is_not_confused_by_keywords_in_its_body() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "bin/tool",
            "#!/usr/bin/env bash\n\
             build_it() {\n  # wraps the class and def helpers, plus a function or two\n  echo \"building\"\n}\n",
        );

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(
            paths(&chunks).iter().any(|p| p == "src/bin/tool#build_it"),
            "got {:?}",
            paths(&chunks)
        );
    }

    #[tokio::test]
    async fn extensionless_python_script_with_env_split_shebang_is_chunked_as_python() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "bin/pyscript",
            "#!/usr/bin/env -S python3.12 -u\nimport sys\n\ndef run_report(rows):\n    return len(rows)\n",
        );

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(
            paths(&chunks)
                .iter()
                .any(|p| p == "src/bin/pyscript#run_report"),
            "got {:?}",
            paths(&chunks)
        );
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("python"), "{}", c.location.path);
        }
    }

    #[tokio::test]
    async fn extension_wins_over_shebang_when_chunking() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "foo.py",
            "#!/usr/bin/env bash\n\ndef greet(name):\n    return \"hello \" + name\n",
        );

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        let greet = chunks
            .iter()
            .find(|c| c.location.path == "src/foo.py#greet")
            .unwrap_or_else(|| panic!("expected #greet, got {:?}", paths(&chunks)));
        assert_eq!(greet.kind, "function");
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("python"), "{}", c.location.path);
        }
    }

    #[tokio::test]
    async fn unknown_interpreter_shebang_is_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "bin/awk-thing",
            "#!/usr/bin/awk -f\nBEGIN { print 1 }\n",
        );
        write(dir.path(), "main.rs", "fn main() {}");

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(chunks_of(&chunks, "src/bin/awk-thing").is_empty());
        assert!(!chunks.is_empty(), "main.rs should still be chunked");
    }

    #[tokio::test]
    async fn ruby_and_perl_shebang_scripts_are_text_passthrough_with_their_label() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "bin/rb",
            "#!/usr/bin/env ruby\ndef hi\n  puts 1\nend\n",
        );
        write(dir.path(), "bin/pl", "#!/usr/bin/perl\nprint \"hi\\n\";\n");

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        let rb = chunks_of(&chunks, "src/bin/rb");
        assert_eq!(rb.len(), 1, "got {:?}", paths(&chunks));
        assert_eq!(rb[0].kind, "file");
        assert_eq!(rb[0].language.as_deref(), Some("ruby"));
        let pl = chunks_of(&chunks, "src/bin/pl");
        assert_eq!(pl.len(), 1, "got {:?}", paths(&chunks));
        assert_eq!(pl[0].language.as_deref(), Some("perl"));
    }

    #[tokio::test]
    async fn makefiles_are_chunked_per_target_with_make_grammar() {
        let makefile = "all: build test\n\
            \n\
            build: deps\n\tcargo build --release\n\
            \n\
            test: build\n\tcargo test --all\n";
        let dir = tempfile::tempdir().unwrap();
        for rel in ["a/Makefile", "b/GNUmakefile", "c/makefile", "d/rules.mk"] {
            write(dir.path(), rel, makefile);
        }

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        for rel in ["a/Makefile", "b/GNUmakefile", "c/makefile", "d/rules.mk"] {
            let file_path = format!("src/{rel}");
            let names = item_names(&chunks, &file_path);
            for target in ["all", "build", "test"] {
                assert!(
                    names.iter().any(|n| n == target),
                    "{rel}: expected a #{target} chunk, got {names:?}"
                );
            }
            let of_file = chunks_of(&chunks, &file_path);
            assert!(
                of_file.iter().any(|c| c.kind == "file"),
                "{rel}: no file chunk"
            );
            for c in of_file {
                assert_eq!(c.language.as_deref(), Some("make"), "{}", c.location.path);
            }
        }
    }

    #[tokio::test]
    async fn well_known_filenames_and_allow_listed_extensions_are_single_text_chunks() {
        let cases = [
            ("Dockerfile", "dockerfile"),
            ("Dockerfile.dev", "dockerfile"),
            ("svc/Dockerfile.prod", "dockerfile"),
            ("Caddyfile", "caddyfile"),
            ("Caddyfile.edge", "caddyfile"),
            ("Procfile", "procfile"),
            ("site.conf", "text"),
            ("x.tpl", "text"),
            ("x.tmpl", "text"),
        ];
        let dir = tempfile::tempdir().unwrap();
        for (rel, _) in cases {
            write(dir.path(), rel, format!("# {rel}\nFOO=bar\nweb: run it\n"));
        }

        let chunks = chunk_directory(dir.path(), "test", &ChunkOptions::default())
            .await
            .unwrap();

        assert_eq!(chunks.len(), cases.len(), "got {:?}", paths(&chunks));
        for (rel, label) in cases {
            let file_path = format!("src/{rel}");
            let of_file = chunks_of(&chunks, &file_path);
            assert_eq!(of_file.len(), 1, "{rel}: got {:?}", paths(&chunks));
            assert_eq!(of_file[0].kind, "file", "{rel}");
            assert_eq!(of_file[0].language.as_deref(), Some(label), "{rel}");
            assert_eq!(
                of_file[0].content,
                format!("# {rel}\nFOO=bar\nweb: run it\n")
            );
        }
    }

    #[tokio::test]
    async fn env_files_are_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".env", "SECRET=hunter2\n");
        write(dir.path(), "prod.env", "SECRET=hunter2\n");
        write(dir.path(), "secrets.env", "SECRET=hunter2\n");

        let chunks = chunk_directory(dir.path(), "test", &ChunkOptions::default())
            .await
            .unwrap();

        assert!(chunks.is_empty(), "got {:?}", paths(&chunks));
    }

    #[tokio::test]
    async fn extensionless_file_without_shebang_is_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "NOTES", "Scratch notes.\nNothing executable.\n");
        write(dir.path(), "LICENSE", "Permission is hereby granted...\n");
        write(dir.path(), "weird.xyz", "some content");
        write(dir.path(), "main.rs", "fn main() {}");

        let chunks = chunk_directory(dir.path(), "test", &ChunkOptions::default())
            .await
            .unwrap();

        assert!(!chunks.is_empty(), "main.rs should still be chunked");
        for c in &chunks {
            assert!(
                c.location.path.starts_with("src/main.rs"),
                "only main.rs should be chunked, got {}",
                c.location.path
            );
        }
    }

    #[tokio::test]
    async fn binary_extensionless_file_is_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let mut elf: Vec<u8> = b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0".to_vec();
        elf.extend((0..2048u32).map(|i| (i % 251) as u8));
        write(dir.path(), "bin/blob", &elf);
        // A shebang line does not rescue a file that contains NUL bytes.
        let mut sneaky = b"#!/bin/bash\necho hi\n".to_vec();
        sneaky.extend_from_slice(&[0, 0, 0, 1, 2, 3]);
        write(dir.path(), "bin/sneaky", &sneaky);
        write(dir.path(), "main.rs", "fn main() {}");

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(chunks_of(&chunks, "src/bin/blob").is_empty());
        assert!(chunks_of(&chunks, "src/bin/sneaky").is_empty());
        assert!(!chunks.is_empty(), "main.rs should still be chunked");
    }

    #[tokio::test]
    async fn oversized_extensionless_file_is_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let mut big = String::from("#!/usr/bin/env bash\n");
        while big.len() <= 1024 * 1024 + 1024 {
            big.push_str("# padding line to make this file large\n");
        }
        write(dir.path(), "bin/huge", &big);

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(chunks.is_empty(), "got {:?}", paths(&chunks));
    }

    #[tokio::test]
    async fn extensionless_script_just_under_the_sniff_limit_is_still_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let mut script = String::from("#!/usr/bin/env bash\n");
        while script.len() < 600 * 1024 {
            script.push_str("# padding line to make this file biggish\n");
        }
        write(dir.path(), "bin/large", &script);

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        let of_file = chunks_of(&chunks, "src/bin/large");
        assert!(of_file.iter().any(|c| c.kind == "file"), "no file chunk");
        assert!(
            of_file
                .iter()
                .all(|c| c.language.as_deref() == Some("bash")),
            "all chunks should be labelled bash"
        );
    }

    #[tokio::test]
    async fn known_extension_files_are_not_subject_to_the_sniff_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(1024 * 1024 + 10 * 1024);
        write(dir.path(), "big.md", &big);

        let chunks = chunk_directory(dir.path(), "test", &ChunkOptions::default())
            .await
            .unwrap();

        assert_eq!(chunks.len(), 1, "got {:?}", paths(&chunks));
        assert_eq!(chunks[0].language.as_deref(), Some("text"));
        assert_eq!(chunks[0].byte_length, big.len());
    }

    #[tokio::test]
    async fn excluded_directories_are_not_sniffed_into_the_index() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "node_modules/pkg/bin/cli",
            "#!/usr/bin/env node\nfunction main() { return 1; }\n",
        );

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(chunks.is_empty(), "got {:?}", paths(&chunks));
    }

    #[tokio::test]
    async fn git_tracked_extensionless_script_is_detected_by_shebang() {
        let dir = tempfile::tempdir().unwrap();
        init_repo_with(dir.path(), &[("tool", BASH_SCRIPT)], &[]);

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(
            paths(&chunks).iter().any(|p| p == "src/tool#foo"),
            "got {:?}",
            paths(&chunks)
        );
    }

    #[tokio::test]
    async fn vue_chunks_are_labelled_vue() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Foo.vue",
            "<template><div>hello</div></template>\n<script setup lang=\"ts\">\nfunction greet(): string {\n    return \"hello world from greet function\";\n}\n</script>\n",
        );

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        assert!(chunks.len() >= 2, "got {:?}", paths(&chunks));
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("vue"), "{}", c.location.path);
        }
    }

    #[tokio::test]
    async fn every_chunk_carries_the_label_of_its_file() {
        let files: [(&str, &str, &str); 14] = [
            ("a.rs", "fn main() { println!(\"hello\"); }\n", "rust"),
            (
                "a.ts",
                "export function f(): number { return 1; }\n",
                "typescript",
            ),
            ("a.tsx", "function C() { return <div/>; }\n", "typescript"),
            ("a.js", "function f() { return 1; }\n", "javascript"),
            ("a.jsx", "function f() { return <b/>; }\n", "javascript"),
            ("a.mjs", "function f() { return 1; }\n", "javascript"),
            ("a.py", "def f():\n    return 1\n", "python"),
            ("a.go", "package main\n\nfunc f() int { return 1 }\n", "go"),
            ("a.php", "<?php\nfunction f() { return 1; }\n", "php"),
            ("a.dart", "void main() { print('x'); }\n", "dart"),
            ("a.sh", "f() {\n  echo one\n}\n", "bash"),
            ("a.bash", "function f {\n  echo one\n}\n", "bash"),
            ("a.mk", "all: dep\n\techo hi\n", "make"),
            ("data.json", "{\"a\": 1}", "text"),
        ];
        let dir = tempfile::tempdir().unwrap();
        for (rel, content, _) in files {
            write(dir.path(), rel, content);
        }

        let chunks = chunk_directory(dir.path(), "test", &small_opts())
            .await
            .unwrap();

        for (rel, _, label) in files {
            let of_file = chunks_of(&chunks, &format!("src/{rel}"));
            assert!(!of_file.is_empty(), "{rel}: no chunks");
            for c in of_file {
                assert_eq!(c.language.as_deref(), Some(label), "{}", c.location.path);
            }
        }
        for c in &chunks {
            assert!(c.language.is_some(), "{} has no language", c.location.path);
        }
    }

    #[tokio::test]
    async fn split_parts_of_large_bash_functions_keep_the_bash_language() {
        let body = |name: &str| {
            let mut s = format!("{name}() {{\n");
            for i in 0..8 {
                s.push_str(&format!(
                    "  echo \"{name} step {i} of the long running job\"\n"
                ));
            }
            s.push_str("}\n\n");
            s
        };
        let script = format!(
            "#!/usr/bin/env bash\n{}{}{}small_one() {{\n  echo \"short but over the minimum size\"\n  true\n}}\n",
            body("alpha"),
            body("beta"),
            body("gamma"),
        );
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "bin/tool", &script);

        let opts = ChunkOptions {
            max_chunk_bytes: 200,
            min_chunk_bytes: 50,
            ..ChunkOptions::default()
        };
        let chunks = chunk_directory(dir.path(), "test", &opts).await.unwrap();

        let parts: Vec<_> = chunks
            .iter()
            .filter(|c| c.location.path.contains("/part"))
            .collect();
        assert!(
            !parts.is_empty(),
            "oversized functions should be split into /partN chunks, got {:?}",
            paths(&chunks)
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.kind != "file" && !c.location.path.contains("/part")),
            "the small function should remain a plain item chunk, got {:?}",
            paths(&chunks)
        );
        for c in &chunks {
            assert_eq!(c.language.as_deref(), Some("bash"), "{}", c.location.path);
            let resolved = languages::language_of_chunk(c)
                .unwrap_or_else(|| panic!("language_of_chunk is None for {}", c.location.path));
            assert_eq!(resolved.label, "bash", "{}", c.location.path);
            assert!(
                matches!(resolved.kind, languages::DetectionKind::Grammar(l) if l.name == "bash"),
                "{} should resolve to the bash grammar",
                c.location.path
            );
        }
    }
}
