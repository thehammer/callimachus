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

/// Labels that denote text-passthrough files (no grammar).  A persisted
/// `Chunk::language` is resolved against this list.
const TEXT_LABELS: &[&str] = &[
    "text",
    "ruby",
    "perl",
    "dockerfile",
    "caddyfile",
    "procfile",
];

/// Plain-text extensions beyond [`TEXT_EXTENSIONS`], checked last by [`detect`].
///
/// `env` is deliberately absent: `prod.env` / `secrets.env` files routinely hold
/// credentials and must never be indexed.
const PLAIN_TEXT_EXTENSIONS: &[&str] = &["conf", "tpl", "tmpl"];

fn grammar(name: &str) -> Option<Detection> {
    let lc = for_name(name)?;
    Some(Detection {
        kind: DetectionKind::Grammar(lc),
        label: lc.name,
    })
}

fn text(label: &'static str) -> Detection {
    Detection {
        kind: DetectionKind::Text,
        label,
    }
}

fn extension_of(rel_path: &str) -> Option<&str> {
    std::path::Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Step 1 of [`detect`]: decide by file extension alone (grammar extension,
/// `vue`, or a [`TEXT_EXTENSIONS`] entry).  Needs no file content, so callers
/// can use it to avoid sniffing files whose extension is already conclusive.
pub fn detect_by_extension(rel_path: &str) -> Option<Detection> {
    let ext = extension_of(rel_path)?;
    if let Some(lc) = for_extension(ext) {
        return grammar(lc.name);
    }
    if ext == "vue" {
        return Some(Detection {
            kind: DetectionKind::Vue,
            label: "vue",
        });
    }
    TEXT_EXTENSIONS.contains(&ext).then(|| text("text"))
}

/// Detect a file's language by extension, then shebang (when `head` — the
/// start of the file's content — is given), then well-known filename, then the
/// plain-text extension allow-list.  The first match wins; `None` means the
/// file is not indexed.
pub fn detect(rel_path: &str, head: Option<&str>) -> Option<Detection> {
    detect_by_extension(rel_path)
        .or_else(|| head.and_then(detect_by_shebang))
        .or_else(|| detect_by_filename(rel_path))
        .or_else(|| detect_plain_text(rel_path))
}

/// Resolve the language of an emitted chunk: the persisted `chunk.language`
/// when present, otherwise detection from the chunk's path (fragment
/// stripped) and content.  The fallback covers rows written before
/// migration 019, which all have extensions.
pub fn language_of_chunk(chunk: &Chunk) -> Option<Detection> {
    if let Some(label) = chunk.language.as_deref()
        && let Some(detection) = detection_for_label(label)
    {
        return Some(detection);
    }
    let path = chunk.location.path.split('#').next().unwrap_or("");
    detect(path, Some(&chunk.content))
}

fn detection_for_label(label: &str) -> Option<Detection> {
    if let Some(d) = grammar(label) {
        return Some(d);
    }
    if label == "vue" {
        return Some(Detection {
            kind: DetectionKind::Vue,
            label: "vue",
        });
    }
    TEXT_LABELS.iter().find(|l| **l == label).map(|l| text(l))
}

fn detect_by_shebang(head: &str) -> Option<Detection> {
    let interpreter = shebang_interpreter(head)?;
    // Strip a trailing version suffix: python3.12 -> python, php8.3 -> php.
    let name = interpreter.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    match name {
        "sh" | "bash" | "zsh" | "dash" | "ksh" => grammar("bash"),
        "python" | "pypy" => grammar("python"),
        "node" | "nodejs" | "bun" => grammar("javascript"),
        "deno" | "ts-node" | "tsx" => grammar("typescript"),
        "php" => grammar("php"),
        "ruby" => Some(text("ruby")),
        "perl" => Some(text("perl")),
        _ => None,
    }
}

/// Extract the interpreter from the first line of `head` when it is a
/// shebang, resolving `env` indirection.
fn shebang_interpreter(head: &str) -> Option<&str> {
    let head = head.strip_prefix('\u{feff}').unwrap_or(head);
    // `lines()` also strips a trailing `\r`.
    let rest = head.lines().next()?.strip_prefix("#!")?;
    let mut tokens = rest.split_whitespace();
    let program = basename(tokens.next()?);
    if program != "env" {
        return Some(program).filter(|p| !p.is_empty());
    }
    while let Some(token) = tokens.next() {
        match token {
            // `-u NAME` / `--unset NAME` take a separate value.
            "-u" | "--unset" => {
                tokens.next();
            }
            t if t.starts_with('-') || t.contains('=') => {}
            t => return Some(basename(t)),
        }
    }
    None
}

fn detect_by_filename(rel_path: &str) -> Option<Detection> {
    let name = basename(rel_path);
    match name {
        "Makefile" | "GNUmakefile" | "makefile" => grammar("make"),
        "Procfile" => Some(text("procfile")),
        _ if name == "Dockerfile" || name.starts_with("Dockerfile.") => Some(text("dockerfile")),
        _ if name == "Caddyfile" || name.starts_with("Caddyfile.") => Some(text("caddyfile")),
        _ => None,
    }
}

fn detect_plain_text(rel_path: &str) -> Option<Detection> {
    let ext = extension_of(rel_path)?;
    PLAIN_TEXT_EXTENSIONS.contains(&ext).then(|| text("text"))
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

#[cfg(test)]
mod detect_tests {
    use super::*;
    use callimachus_adapter_contract::{Chunk, Location};

    /// How a detection is processed, reduced to something comparable.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        /// Grammar-backed; the grammar name must equal the label.
        Grammar(&'static str),
        Text(&'static str),
        Vue,
        Undetected,
    }

    fn outcome(d: Option<Detection>) -> Outcome {
        match d {
            None => Outcome::Undetected,
            Some(Detection {
                kind: DetectionKind::Grammar(lc),
                label,
            }) => {
                assert_eq!(
                    lc.name, label,
                    "a grammar-backed detection's label must be the grammar name"
                );
                Outcome::Grammar(label)
            }
            Some(Detection {
                kind: DetectionKind::Text,
                label,
            }) => Outcome::Text(label),
            Some(Detection {
                kind: DetectionKind::Vue,
                label,
            }) => {
                assert_eq!(label, "vue");
                Outcome::Vue
            }
        }
    }

    fn detected(path: &str, head: Option<&str>) -> Outcome {
        outcome(detect(path, head))
    }

    fn chunk(path: &str, language: Option<&str>, content: &str) -> Chunk {
        let mut c = Chunk::new(
            "test".to_string(),
            None,
            "function".to_string(),
            Location::new("test", path),
            content.to_string(),
        );
        c.language = language.map(str::to_string);
        c
    }

    // ── Step 1: extension ────────────────────────────────────────────────────

    #[test]
    fn grammar_extensions_are_detected_with_stable_labels() {
        let cases = [
            ("a.rs", "rust"),
            ("a.ts", "typescript"),
            ("a.tsx", "typescript"),
            ("a.js", "javascript"),
            ("a.jsx", "javascript"),
            ("a.mjs", "javascript"),
            ("a.py", "python"),
            ("a.go", "go"),
            ("a.php", "php"),
            ("a.dart", "dart"),
            ("a.sh", "bash"),
            ("a.bash", "bash"),
            ("a.mk", "make"),
            ("deep/nested/dir/a.py", "python"),
        ];
        for (path, label) in cases {
            assert_eq!(detected(path, None), Outcome::Grammar(label), "{path}");
        }
    }

    #[test]
    fn vue_extension_is_detected_as_vue() {
        assert_eq!(detected("ui/Foo.vue", None), Outcome::Vue);
    }

    #[test]
    fn text_extensions_are_detected_as_text() {
        for path in [
            "a.json", "a.yaml", "a.yml", "a.tf", "a.sql", "a.md", "a.css",
        ] {
            assert_eq!(detected(path, None), Outcome::Text("text"), "{path}");
        }
    }

    #[test]
    fn known_extension_always_wins_over_shebang() {
        assert_eq!(
            detected("foo.py", Some("#!/usr/bin/env bash\nimport os\n")),
            Outcome::Grammar("python")
        );
        assert_eq!(
            detected("foo.sh", Some("#!/usr/bin/env python3\n")),
            Outcome::Grammar("bash")
        );
        assert_eq!(
            detected("data.json", Some("#!/bin/bash\n")),
            Outcome::Text("text")
        );
    }

    #[test]
    fn known_extension_wins_over_well_known_filename() {
        assert_eq!(detected("Dockerfile.json", None), Outcome::Text("text"));
        assert_eq!(detected("Caddyfile.md", None), Outcome::Text("text"));
    }

    // ── detect_by_extension ──────────────────────────────────────────────────

    #[test]
    fn detect_by_extension_decides_on_extension_alone() {
        assert_eq!(
            outcome(detect_by_extension("a.py")),
            Outcome::Grammar("python")
        );
        assert_eq!(
            outcome(detect_by_extension("a.sh")),
            Outcome::Grammar("bash")
        );
        assert_eq!(outcome(detect_by_extension("a.vue")), Outcome::Vue);
        assert_eq!(outcome(detect_by_extension("a.md")), Outcome::Text("text"));
    }

    #[test]
    fn detect_by_extension_does_not_use_filename_or_allow_list() {
        for path in [
            "bin/tool",
            "Makefile",
            "Dockerfile",
            "Dockerfile.dev",
            "Caddyfile.edge",
            "Procfile",
            "site.conf",
            "x.tmpl",
            "x.tpl",
            "weird.xyz",
            ".env",
        ] {
            assert_eq!(
                outcome(detect_by_extension(path)),
                Outcome::Undetected,
                "{path}"
            );
        }
    }

    // ── Step 2: shebang ──────────────────────────────────────────────────────

    #[test]
    fn shebang_shell_family_maps_to_bash_grammar() {
        let heads = [
            "#!/usr/bin/env bash\necho hi\n",
            "#!/bin/bash\n",
            "#!/bin/sh\n",
            "#! /bin/sh\n",
            "#!/usr/bin/env zsh\n",
            "#!/bin/zsh\n",
            "#!/bin/dash\n",
            "#!/bin/ksh\n",
            "#!/bin/bash -e\n",
            "#!/bin/sh -eu\n",
            "#!/usr/bin/env bash -e\n",
            "#!/usr/bin/env sh\n",
        ];
        for head in heads {
            assert_eq!(
                detected("bin/tool", Some(head)),
                Outcome::Grammar("bash"),
                "{head:?}"
            );
        }
    }

    #[test]
    fn shebang_python_family_maps_to_python_grammar() {
        let heads = [
            "#!/usr/bin/python3\n",
            "#!/usr/bin/python\n",
            "#!/usr/bin/python2\n",
            "#!/usr/bin/env python3\n",
            "#!/usr/bin/env python3.12\n",
            "#!/usr/bin/python3.12\n",
            "#!/usr/bin/env pypy\n",
            "#!/usr/bin/env pypy3\n",
            "#!/usr/bin/env -S python3.12 -u\n",
        ];
        for head in heads {
            assert_eq!(
                detected("bin/tool", Some(head)),
                Outcome::Grammar("python"),
                "{head:?}"
            );
        }
    }

    #[test]
    fn shebang_javascript_and_typescript_runtimes() {
        for head in [
            "#!/usr/bin/env node\n",
            "#!/usr/bin/node\n",
            "#!/usr/bin/env nodejs\n",
            "#!/usr/bin/env bun\n",
        ] {
            assert_eq!(
                detected("bin/tool", Some(head)),
                Outcome::Grammar("javascript"),
                "{head:?}"
            );
        }
        for head in [
            "#!/usr/bin/env deno\n",
            "#!/usr/bin/env ts-node\n",
            "#!/usr/bin/env tsx\n",
        ] {
            assert_eq!(
                detected("bin/tool", Some(head)),
                Outcome::Grammar("typescript"),
                "{head:?}"
            );
        }
    }

    #[test]
    fn shebang_php_with_and_without_version_suffix() {
        for head in [
            "#!/usr/bin/php\n",
            "#!/usr/bin/php8.3\n",
            "#!/usr/bin/env php\n",
        ] {
            assert_eq!(
                detected("bin/tool", Some(head)),
                Outcome::Grammar("php"),
                "{head:?}"
            );
        }
    }

    #[test]
    fn shebang_ruby_and_perl_are_text_with_their_own_label() {
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/env ruby\n")),
            Outcome::Text("ruby")
        );
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/ruby\n")),
            Outcome::Text("ruby")
        );
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/perl\n")),
            Outcome::Text("perl")
        );
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/env perl\n")),
            Outcome::Text("perl")
        );
    }

    #[test]
    fn env_options_and_assignments_are_skipped_to_find_the_interpreter() {
        let heads = [
            "#!/usr/bin/env -S python3.12 -u\n",
            "#!/usr/bin/env -u FOO bash\n",
            "#!/usr/bin/env FOO=1 bash\n",
            "#!/usr/bin/env -i FOO=1 BAR=2 bash\n",
            "#!/usr/bin/env -i -u FOO -u BAR FOO=1 bash\n",
        ];
        let expected = [
            Outcome::Grammar("python"),
            Outcome::Grammar("bash"),
            Outcome::Grammar("bash"),
            Outcome::Grammar("bash"),
            Outcome::Grammar("bash"),
        ];
        for (head, want) in heads.iter().zip(expected) {
            assert_eq!(detected("bin/tool", Some(head)), want, "{head:?}");
        }
    }

    #[test]
    fn env_dash_u_value_is_not_mistaken_for_the_interpreter() {
        // `-u python` unsets a variable named "python"; the interpreter is bash.
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/env -u python bash\n")),
            Outcome::Grammar("bash")
        );
    }

    #[test]
    fn shebang_tolerates_crlf_line_endings() {
        assert_eq!(
            detected("bin/tool", Some("#!/bin/bash\r\necho hi\r\n")),
            Outcome::Grammar("bash")
        );
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/env python3\r\n")),
            Outcome::Grammar("python")
        );
        // A head that ends mid-line with a stray carriage return.
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/env bash\r")),
            Outcome::Grammar("bash")
        );
    }

    #[test]
    fn shebang_tolerates_leading_utf8_bom() {
        assert_eq!(
            detected("bin/tool", Some("\u{feff}#!/bin/bash\necho hi\n")),
            Outcome::Grammar("bash")
        );
    }

    #[test]
    fn shebang_without_trailing_newline_is_detected() {
        assert_eq!(
            detected("bin/tool", Some("#!/bin/bash")),
            Outcome::Grammar("bash")
        );
    }

    #[test]
    fn unknown_interpreter_in_unnamed_file_is_not_detected() {
        assert_eq!(
            detected("LICENSE", Some("#!/usr/bin/awk -f\nBEGIN {}\n")),
            Outcome::Undetected
        );
        assert_eq!(
            detected("bin/tool", Some("#!/usr/bin/env lua\n")),
            Outcome::Undetected
        );
    }

    #[test]
    fn unknown_interpreter_falls_through_to_well_known_filename() {
        assert_eq!(
            detected("Makefile", Some("#!/usr/bin/awk -f\n")),
            Outcome::Grammar("make")
        );
        assert_eq!(
            detected("Dockerfile", Some("#!/usr/bin/awk -f\n")),
            Outcome::Text("dockerfile")
        );
    }

    #[test]
    fn degenerate_shebang_lines_do_not_panic_or_match() {
        for head in [
            "#!",
            "#!\n",
            "#!   \n",
            "#!/usr/bin/env\n",
            "#!/usr/bin/env -S\n",
            "#!/usr/bin/env -u\n",
            "#!/usr/bin/env FOO=1\n",
            "#!/\n",
        ] {
            assert_eq!(
                detected("LICENSE", Some(head)),
                Outcome::Undetected,
                "{head:?}"
            );
        }
    }

    #[test]
    fn only_the_first_line_is_a_shebang() {
        assert_eq!(
            detected("LICENSE", Some("echo hi\n#!/bin/bash\n")),
            Outcome::Undetected
        );
        assert_eq!(
            detected("LICENSE", Some("\n#!/bin/bash\n")),
            Outcome::Undetected
        );
        assert_eq!(
            detected("LICENSE", Some(" #!/bin/bash\n")),
            Outcome::Undetected
        );
    }

    #[test]
    fn empty_head_without_shebang_and_missing_head_are_not_detected() {
        assert_eq!(detected("LICENSE", Some("")), Outcome::Undetected);
        assert_eq!(detected("NOTES", Some("plain text\n")), Outcome::Undetected);
        assert_eq!(detected("bin/tool", None), Outcome::Undetected);
        assert_eq!(
            detected("bin/tool", Some("# just a comment\n")),
            Outcome::Undetected
        );
    }

    #[test]
    fn shebang_wins_over_filename_and_allow_list() {
        // Step 2 precedes steps 3 and 4.
        assert_eq!(
            detected("run.conf", Some("#!/bin/bash\n")),
            Outcome::Grammar("bash")
        );
        assert_eq!(
            detected("stack.env", Some("#!/usr/bin/env python3\n")),
            Outcome::Grammar("python")
        );
        assert_eq!(
            detected("Makefile", Some("#!/bin/bash\n")),
            Outcome::Grammar("bash")
        );
    }

    // ── Step 3: well-known filenames ─────────────────────────────────────────

    #[test]
    fn makefile_names_are_detected_as_make_grammar() {
        for path in [
            "Makefile",
            "GNUmakefile",
            "makefile",
            "a/b/Makefile",
            "sub/GNUmakefile",
        ] {
            assert_eq!(detected(path, None), Outcome::Grammar("make"), "{path}");
        }
    }

    #[test]
    fn dockerfile_names_are_detected_as_dockerfile_text() {
        for path in [
            "Dockerfile",
            "Dockerfile.dev",
            "Dockerfile.prod",
            "a/b/Dockerfile",
            "services/api/Dockerfile.test",
        ] {
            assert_eq!(detected(path, None), Outcome::Text("dockerfile"), "{path}");
        }
    }

    #[test]
    fn caddyfile_and_procfile_names_are_detected() {
        for path in ["Caddyfile", "Caddyfile.edge", "deploy/Caddyfile.local"] {
            assert_eq!(detected(path, None), Outcome::Text("caddyfile"), "{path}");
        }
        for path in ["Procfile", "app/Procfile"] {
            assert_eq!(detected(path, None), Outcome::Text("procfile"), "{path}");
        }
    }

    #[test]
    fn well_known_filename_is_matched_on_basename_not_directory() {
        // A directory called Makefile does not make its children Makefiles.
        assert_eq!(detected("Makefile/notes", None), Outcome::Undetected);
        assert_eq!(detected("Dockerfile/readme", None), Outcome::Undetected);
    }

    // ── Step 4: plain-text allow-list ────────────────────────────────────────

    #[test]
    fn allow_listed_extensions_are_text() {
        for path in ["site.conf", "x.tpl", "x.tmpl", "a/b/app.conf"] {
            assert_eq!(detected(path, None), Outcome::Text("text"), "{path}");
        }
    }

    #[test]
    fn env_files_are_never_detected() {
        for path in ["stack.env", "prod.env", "secrets.env", "a/b/local.env"] {
            assert_eq!(detected(path, None), Outcome::Undetected, "{path}");
            assert_eq!(
                detected(path, Some("SECRET=hunter2\n")),
                Outcome::Undetected,
                "{path}"
            );
        }
    }

    #[test]
    fn bare_dotfile_env_is_not_an_env_file() {
        assert_eq!(detected(".env", None), Outcome::Undetected);
        assert_eq!(detected("config/.env", None), Outcome::Undetected);
        assert_eq!(
            detected(".env", Some("SECRET=hunter2\n")),
            Outcome::Undetected
        );
    }

    // ── Nothing matched ──────────────────────────────────────────────────────

    #[test]
    fn unknown_files_are_not_detected() {
        assert_eq!(
            detected("weird.xyz", Some("some content")),
            Outcome::Undetected
        );
        assert_eq!(detected("weird.xyz", None), Outcome::Undetected);
        assert_eq!(detected("LICENSE", None), Outcome::Undetected);
        assert_eq!(detected("NOTES", Some("notes\n")), Outcome::Undetected);
    }

    // ── language_of_chunk ────────────────────────────────────────────────────

    #[test]
    fn persisted_grammar_language_is_used_for_extensionless_item_chunk() {
        let c = chunk("src/bin/tool#deploy", Some("bash"), "deploy() { :; }");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Grammar("bash"));
    }

    #[test]
    fn persisted_language_wins_over_path_detection() {
        let c = chunk("src/a.py", Some("bash"), "echo hi");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Grammar("bash"));
    }

    #[test]
    fn persisted_labels_resolve_to_their_kinds() {
        for name in [
            "rust",
            "typescript",
            "javascript",
            "python",
            "go",
            "php",
            "dart",
            "bash",
            "make",
        ] {
            let c = chunk("src/x#y", Some(name), "");
            assert_eq!(
                outcome(language_of_chunk(&c)),
                Outcome::Grammar(name),
                "{name}"
            );
        }
        let c = chunk("src/Foo.vue", Some("vue"), "<template/>");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Vue);
        for label in [
            "text",
            "ruby",
            "perl",
            "dockerfile",
            "caddyfile",
            "procfile",
        ] {
            let c = chunk("src/x", Some(label), "");
            assert_eq!(
                outcome(language_of_chunk(&c)),
                Outcome::Text(label),
                "{label}"
            );
        }
    }

    #[test]
    fn missing_language_falls_back_to_extension() {
        let c = chunk("src/a.py", None, "def f(): pass");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Grammar("python"));
    }

    #[test]
    fn missing_language_fallback_ignores_the_path_fragment() {
        let c = chunk("src/a.py#f", None, "def f(): pass");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Grammar("python"));
    }

    #[test]
    fn missing_language_falls_back_to_shebang_in_chunk_content() {
        let c = chunk(
            "src/bin/tool",
            None,
            "#!/usr/bin/env bash\ndeploy() { :; }\n",
        );
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Grammar("bash"));
    }

    #[test]
    fn missing_language_falls_back_to_well_known_filename() {
        let c = chunk("src/build/Makefile", None, "all:\n\techo hi\n");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Grammar("make"));
    }

    #[test]
    fn extensionless_item_chunk_without_language_or_shebang_is_undetected() {
        let c = chunk("src/bin/tool#fn", None, "deploy() { :; }");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Undetected);
    }

    #[test]
    fn unknown_persisted_label_falls_back_to_path_detection() {
        let c = chunk("src/a.py", Some("klingon"), "def f(): pass");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Grammar("python"));
        let c = chunk("src/bin/tool#fn", Some("klingon"), "deploy() { :; }");
        assert_eq!(outcome(language_of_chunk(&c)), Outcome::Undetected);
    }
}
