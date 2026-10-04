use callimachus_adapter_contract::Chunk;


/// Configuration for a supported programming language.
#[derive(Debug)]
pub struct LangConfig {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    /// Returns the tree-sitter `Language` for this grammar.
    pub language_fn: fn() -> tree_sitter::Language,
    /// tree-sitter S-expression query for top-level items.
    pub top_level_query: &'static str,
    /// Query for call expressions within a chunk.
    pub call_query: &'static str,
}

// Thin wrappers that convert `LanguageFn` constants to `tree_sitter::Language`.
fn rust_language() -> tree_sitter::Language {
    tree_sitter_rust::LANGUAGE.into()
}
fn typescript_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}
fn tsx_language() -> tree_sitter::Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}
fn python_language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}
fn go_language() -> tree_sitter::Language {
    tree_sitter_go::LANGUAGE.into()
}
fn php_language() -> tree_sitter::Language {
    tree_sitter_php::LANGUAGE_PHP.into()
}
fn dart_language() -> tree_sitter::Language {
    tree_sitter_dart_orchard::LANGUAGE.into()
}
fn bash_language() -> tree_sitter::Language {
    tree_sitter_bash::LANGUAGE.into()
}
fn make_language() -> tree_sitter::Language {
    tree_sitter_make::LANGUAGE.into()
}

// ── Rust ────────────────────────────────────────────────────────────────────

const RUST_TOP_LEVEL_QUERY: &str = r#"
(source_file
  [(function_item) (impl_item) (struct_item) (trait_item) (mod_item)] @item)
"#;

const RUST_CALL_QUERY: &str = r#"
(call_expression
  function: [(identifier) (field_expression) (scoped_identifier)] @callee)
"#;

// ── TypeScript/JavaScript ────────────────────────────────────────────────────

const TS_TOP_LEVEL_QUERY: &str = r#"
(program
  [(function_declaration) (class_declaration) (export_statement)] @item)
"#;

const TS_CALL_QUERY: &str = r#"
(call_expression
  function: [(identifier) (member_expression)] @callee)
"#;

// ── Python ──────────────────────────────────────────────────────────────────

const PYTHON_TOP_LEVEL_QUERY: &str = r#"
(module
  [(function_definition) (class_definition)] @item)
"#;

const PYTHON_CALL_QUERY: &str = r#"
(call
  function: [(identifier) (attribute)] @callee)
"#;

// ── Go ──────────────────────────────────────────────────────────────────────

const GO_TOP_LEVEL_QUERY: &str = r#"
(source_file
  [(function_declaration) (method_declaration) (type_declaration)] @item)
"#;

const GO_CALL_QUERY: &str = r#"
(call_expression
  function: [(identifier) (selector_expression)] @callee)
"#;

// ── PHP ─────────────────────────────────────────────────────────────────────

const PHP_TOP_LEVEL_QUERY: &str = r#"
(program
  [(function_definition) (class_declaration) (interface_declaration) (trait_declaration) (enum_declaration) (namespace_definition)] @item)
"#;

const PHP_CALL_QUERY: &str = r#"
[
  (function_call_expression function: (_) @callee)
  (member_call_expression name: (_) @callee)
  (scoped_call_expression name: (_) @callee)
]
"#;

// ── Dart ─────────────────────────────────────────────────────────────────────

/// Top-level Dart constructs captured by the chunker.
///
/// `function_signature` matches top-level Dart functions whose signature and
/// body are separate sibling nodes under `program` (unlike Rust/PHP where the
/// body is nested inside the item node).
const DART_TOP_LEVEL_QUERY: &str = r#"
(program
  [(class_definition) (mixin_declaration) (enum_declaration) (extension_declaration) (function_signature)] @item)
"#;

/// Call query for Dart.
///
/// Dart call expressions are expressed as `(identifier) (selector (argument_part))`
/// sibling pairs rather than a single `call_expression` wrapper node.  Capturing
/// the leading identifier of each expression_statement gives an approximate callee
/// set suitable for the `calls` edge graph.
const DART_CALL_QUERY: &str = r#"
(expression_statement . (identifier) @callee)
"#;

// ── Bash ────────────────────────────────────────────────────────────────────

/// Top-level shell functions (`foo() { … }`, `function foo { … }`).
const BASH_TOP_LEVEL_QUERY: &str = r#"
(program (function_definition) @item)
"#;

/// Every command invocation; the callee set is filtered against known
/// function names by the extractor, so external commands (`ls`, `git`) drop out.
const BASH_CALL_QUERY: &str = r#"
(command name: (command_name) @callee)
"#;

// ── Make ────────────────────────────────────────────────────────────────────

/// Top-level rules (`target: prereqs` plus recipe).
const MAKE_TOP_LEVEL_QUERY: &str = r#"
(makefile (rule) @item)
"#;

/// Makefiles have no call syntax worth modelling; `$(call …)` is the closest
/// construct and is rare.  Kept as a valid query that seldom matches.
const MAKE_CALL_QUERY: &str = r#"
(function_call function: (_) @callee)
"#;

// ── Registry ────────────────────────────────────────────────────────────────

static SUPPORTED_LANGUAGES: &[LangConfig] = &[
    LangConfig {
        name: "rust",
        extensions: &["rs"],
        language_fn: rust_language,
        top_level_query: RUST_TOP_LEVEL_QUERY,
        call_query: RUST_CALL_QUERY,
    },
    LangConfig {
        name: "typescript",
        extensions: &["ts", "tsx"],
        language_fn: typescript_language,
        top_level_query: TS_TOP_LEVEL_QUERY,
        call_query: TS_CALL_QUERY,
    },
    LangConfig {
        name: "javascript",
        extensions: &["js", "jsx", "mjs"],
        // TS grammar covers JS well
        language_fn: tsx_language,
        top_level_query: TS_TOP_LEVEL_QUERY,
        call_query: TS_CALL_QUERY,
    },
    LangConfig {
        name: "python",
        extensions: &["py"],
        language_fn: python_language,
        top_level_query: PYTHON_TOP_LEVEL_QUERY,
        call_query: PYTHON_CALL_QUERY,
    },
    LangConfig {
        name: "go",
        extensions: &["go"],
        language_fn: go_language,
        top_level_query: GO_TOP_LEVEL_QUERY,
        call_query: GO_CALL_QUERY,
    },
    LangConfig {
        name: "php",
        extensions: &["php"],
        language_fn: php_language,
        top_level_query: PHP_TOP_LEVEL_QUERY,
        call_query: PHP_CALL_QUERY,
    },
    LangConfig {
        name: "dart",
        extensions: &["dart"],
        language_fn: dart_language,
        top_level_query: DART_TOP_LEVEL_QUERY,
        call_query: DART_CALL_QUERY,
    },
    LangConfig {
        name: "bash",
        extensions: &["sh", "bash"],
        language_fn: bash_language,
        top_level_query: BASH_TOP_LEVEL_QUERY,
        call_query: BASH_CALL_QUERY,
    },
    LangConfig {
        name: "make",
        extensions: &["mk"],
        language_fn: make_language,
        top_level_query: MAKE_TOP_LEVEL_QUERY,
        call_query: MAKE_CALL_QUERY,
    },
];

/// Extensions that are not tree-sitter parsed but should be captured as single
/// file-level chunks (text passthrough mode).  Do NOT add these to
/// `all_extensions()` — they are not grammar-backed languages.
pub const TEXT_EXTENSIONS: &[&str] = &[
    // Config / data
    "json", "yaml", "yml", // Infrastructure
    "tf", "tfvars", "hcl", // Database
    "sql", // Templates
    "ftl", "html", // Styles
    "css", "scss", // Docs
    "md",
];

/// Look up language config by file extension (e.g., "rs", "ts").
pub fn for_extension(ext: &str) -> Option<&'static LangConfig> {
    SUPPORTED_LANGUAGES
        .iter()
        .find(|lc| lc.extensions.contains(&ext))
}

/// Look up language config by name (e.g., "rust", "typescript").
pub fn for_name(name: &str) -> Option<&'static LangConfig> {
    SUPPORTED_LANGUAGES.iter().find(|lc| lc.name == name)
}

/// List all supported extensions.
pub fn all_extensions() -> impl Iterator<Item = &'static str> {
    SUPPORTED_LANGUAGES
        .iter()
        .flat_map(|lc| lc.extensions.iter().copied())
}

// ── Detection ───────────────────────────────────────────────────────────────

/// How a detected file is processed by the chunker.
#[derive(Debug, Clone, Copy)]
pub enum DetectionKind {
    /// Parsed with a tree-sitter grammar into item chunks.
    Grammar(&'static LangConfig),
    /// Captured as a single file-level chunk (text passthrough).
    Text,
    /// Vue single-file component (script block parsed as TypeScript).
    Vue,
}

/// The outcome of language detection: how to process the file plus a stable
/// language label that is persisted on every chunk (`Chunk::language`).
#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub kind: DetectionKind,
    pub label: &'static str,
}

/// Step 1 of [`detect`]: decide by file extension alone (grammar extension,
/// `vue`, or a [`TEXT_EXTENSIONS`] entry).  Needs no file content, so callers
/// can use it to avoid sniffing files whose extension is already conclusive.
pub fn detect_by_extension(rel_path: &str) -> Option<Detection> {
    let _ = rel_path;
    unimplemented!("detect_by_extension")
}

/// Detect a file's language by extension, then shebang (when `head` — the
/// start of the file's content — is given), then well-known filename, then the
/// plain-text extension allow-list.  The first match wins; `None` means the
/// file is not indexed.
pub fn detect(rel_path: &str, head: Option<&str>) -> Option<Detection> {
    let _ = (rel_path, head);
    unimplemented!("detect")
}

/// Resolve the language of an emitted chunk: the persisted `chunk.language`
/// when present, otherwise detection from the chunk's path (fragment
/// stripped) and content.  The fallback covers rows written before
/// migration 019, which all have extensions.
pub fn language_of_chunk(chunk: &Chunk) -> Option<Detection> {
    let _ = chunk;
    unimplemented!("language_of_chunk")
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    /// Every registered grammar must load under the workspace's tree-sitter ABI
    /// and its queries must compile against that grammar.
    #[test]
    fn all_grammars_load_and_queries_compile() {
        for lc in SUPPORTED_LANGUAGES {
            let language = (lc.language_fn)();
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&language)
                .unwrap_or_else(|e| panic!("{}: set_language failed: {e}", lc.name));
            tree_sitter::Query::new(&language, lc.top_level_query)
                .unwrap_or_else(|e| panic!("{}: top-level query: {e:?}", lc.name));
            tree_sitter::Query::new(&language, lc.call_query)
                .unwrap_or_else(|e| panic!("{}: call query: {e:?}", lc.name));
        }
    }

    #[test]
    fn sh_is_grammar_backed_not_text() {
        assert_eq!(for_extension("sh").map(|l| l.name), Some("bash"));
        assert_eq!(for_extension("bash").map(|l| l.name), Some("bash"));
        assert_eq!(for_extension("mk").map(|l| l.name), Some("make"));
        assert!(!TEXT_EXTENSIONS.contains(&"sh"));
    }
}
