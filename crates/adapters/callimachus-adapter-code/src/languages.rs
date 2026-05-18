/// Configuration for a supported programming language.
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
];

/// Extensions that are not tree-sitter parsed but should be captured as single
/// file-level chunks (text passthrough mode).  Do NOT add these to
/// `all_extensions()` — they are not grammar-backed languages.
pub const TEXT_EXTENSIONS: &[&str] = &[
    // Config / data
    "json", "yaml", "yml",
    // Infrastructure
    "tf", "tfvars", "hcl",
    // Database
    "sql",
    // Templates
    "ftl", "html",
    // Scripts
    "sh",
    // Styles
    "css", "scss",
    // Docs
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
