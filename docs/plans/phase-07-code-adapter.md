# Phase 7 — Code adapter

## Context

Phase 6 delivered reindex and watch. The system can now maintain a live index of any corpus that implements `SourceAdapter`. The only adapter so far is `callimachus-adapter-book`.

This phase implements `callimachus-adapter-code`: a tree-sitter–based adapter that indexes source code repositories. The result: `calli index myrepo` indexes a codebase, and the MCP tools answer questions like "where is `processOrder` called?", "what functions does `AuthService` expose?", and "find me all the places `User` and `Permission` appear together."

Reference: `docs/plans/callimachus-standalone.md §3`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 6 commit

## Files to change

---

### `crates/adapters/callimachus-adapter-code/Cargo.toml`

Replace the stub.

```toml
[package]
name = "callimachus-adapter-code"
version = "0.1.0"
edition = "2024"

[dependencies]
callimachus-core = { path = "../../callimachus-core" }
callimachus-llm = { path = "../../callimachus-llm" }
anyhow = "1"
serde = { workspace = true }
serde_json = { workspace = true }
async-trait = "0.1"
tokio = { workspace = true }
tracing = { workspace = true }
walkdir = "2"
tree-sitter = "0.23"
tree-sitter-rust = "0.23"
tree-sitter-typescript = "0.23"
tree-sitter-python = "0.23"
tree-sitter-go = "0.23"
sha2 = "0.10"
hex = "0.4"
git2 = { version = "0.19", default-features = false }
```

---

### `crates/adapters/callimachus-adapter-code/src/lib.rs`

```rust
pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod git;
pub mod languages;
pub mod summarizer;
pub use adapter::CodeAdapter;
pub fn create() -> CodeAdapter { CodeAdapter::new() }
```

---

### `src/languages.rs`

Language registry: maps file extension → tree-sitter `Language` + language name.

```rust
pub struct LangConfig {
    pub name: &'static str,                 // "rust", "typescript", "python", "go"
    pub extensions: &'static [&'static str],
    pub language_fn: fn() -> tree_sitter::Language,
}

static SUPPORTED_LANGUAGES: &[LangConfig] = &[
    LangConfig { name: "rust", extensions: &["rs"], language_fn: tree_sitter_rust::language },
    LangConfig { name: "typescript", extensions: &["ts", "tsx"], language_fn: tree_sitter_typescript::language_typescript },
    LangConfig { name: "javascript", extensions: &["js", "jsx", "mjs"], language_fn: tree_sitter_typescript::language_tsx },  // TS grammar covers JS
    LangConfig { name: "python", extensions: &["py"], language_fn: tree_sitter_python::language },
    LangConfig { name: "go", extensions: &["go"], language_fn: tree_sitter_go::language },
];

pub fn for_extension(ext: &str) -> Option<&'static LangConfig>
pub fn for_name(name: &str) -> Option<&'static LangConfig>
```

---

### `src/git.rs`

Thin wrapper over `git2` for reading repository metadata.

```rust
pub struct GitInfo {
    pub branch: String,
    pub commit: String,   // short SHA
    pub dirty: bool,      // uncommitted changes present
}

pub fn read_git_info(repo_path: &Path) -> anyhow::Result<Option<GitInfo>>
```

Uses `git2::Repository::open`. Returns `None` if path is not a git repo (no error). `dirty` = any files in the index or working tree differ from HEAD. Used to populate `corpus.config.git` at index time.

---

### `src/chunker.rs`

The chunker walks the repository directory tree and emits `Chunk` objects using tree-sitter for structural boundaries.

```rust
pub struct ChunkOptions {
    pub max_chunk_bytes: usize,   // default 4000
    pub min_chunk_bytes: usize,   // default 100
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    /// If Some, restrict to files changed since this git ref.
    pub since_ref: Option<String>,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_chunk_bytes: 4000,
            min_chunk_bytes: 100,
            include_globs: vec![],
            // Always exclude common non-code directories
            exclude_globs: vec![
                "target/**".into(), "node_modules/**".into(),
                ".git/**".into(), "dist/**".into(), "build/**".into(),
            ],
            since_ref: None,
        }
    }
}

pub async fn chunk_directory(
    source_path: &Path,
    opts: &ChunkOptions,
) -> anyhow::Result<Vec<Chunk>>
```

**Chunking strategy:**

1. Walk `source_path` with `walkdir`. Skip excluded globs. If `include_globs` are set, skip files not matching any include.
2. For each file: detect language from extension (skip unknown extensions silently).
3. Parse with tree-sitter.
4. Extract top-level items using language-specific queries (see below). Each top-level item = one chunk.
5. The file itself also gets a `kind = "file"` chunk (full content). Top-level item chunks get `parent_path` = the file's location URI.
6. If a top-level item exceeds `max_chunk_bytes`: split at inner function boundaries (same tree-sitter query, one level deeper). If still too large, split at line count boundaries (aim for 100-line splits).
7. If a file has no tree-sitter top-level items (e.g. a config file): the file chunk is the only chunk for that file.

**Location URI scheme for code:** `calli://<corpus_id>/src/<relative-file-path>#<symbol>`. Examples:
- `calli://myrepo/src/api/auth.ts#AuthService` — a class
- `calli://myrepo/src/api/auth.ts#AuthService.login` — a method (nested)
- `calli://myrepo/src/api/auth.ts` — the file chunk

`adapter.format_location` and `adapter.parse_location` implement this scheme.

---

### `src/extractor.rs`

Parser-driven structural extraction. No LLM calls.

```rust
pub struct ExtractedCodeStructure {
    /// Entities: functions, classes, interfaces, types, modules.
    pub entities: Vec<Entity>,
    /// Edges: calls, imports, extends, implements.
    pub edges: Vec<Edge>,
    /// Docstring or leading comment for the chunk's root symbol.
    pub doc_comment: Option<String>,
}

pub fn extract_structure(
    chunk: &Chunk,
    lang_config: &LangConfig,
) -> anyhow::Result<ExtractedCodeStructure>
```

**Entities extracted (by kind):**

| Entity kind | What it represents |
|-------------|-------------------|
| `function` | Standalone function or method |
| `class` | Class, struct (Rust), impl block |
| `interface` | Interface, trait (Rust), type alias |
| `module` | File-level module, namespace |
| `import` | Imported name (for graph edges) |

For each chunk, extract entities only within that chunk's AST node (not the whole file).

**Edges extracted:**

| Edge kind | What it represents |
|-----------|-------------------|
| `calls` | Function A calls function B (by name, unresolved) |
| `imports` | File A imports name B |
| `extends` | Class A extends class B |
| `implements` | Class A implements interface B |
| `defines` | File A defines symbol B (structural containment) |

Call extraction: collect all `call_expression` AST nodes within the chunk, extract the callee name, emit a `calls` edge with `to_entity_id` = the callee name (as a string, not a resolved ID — `entity_store::find_by_name` resolves it at query time).

**Language-specific tree-sitter queries:**

Rust top-level items:
```scheme
(source_file
  [(function_item) (impl_item) (struct_item) (trait_item) (mod_item)] @item)
```

TypeScript/JavaScript top-level items:
```scheme
(program
  [(function_declaration) (class_declaration) (export_statement)] @item)
```

Python top-level items:
```scheme
(module
  [(function_definition) (class_definition)] @item)
```

Go top-level items:
```scheme
(source_file
  [(function_declaration) (method_declaration) (type_declaration)] @item)
```

These are embedded as `const` string literals in `src/languages.rs` next to each `LangConfig`.

---

### `src/summarizer.rs`

LLM-driven summarization for code chunks.

```rust
pub async fn summarize_chunk(
    chunk: &Chunk,
    structure: &ExtractedCodeStructure,
    llm: &dyn LlmProvider,
) -> anyhow::Result<Option<String>>
```

Prompt template for code:

```
You are summarizing a code chunk for a searchable index.

Language: {language}
Symbol: {symbol_name}
Kind: {kind} (function|class|interface|module)

{doc_comment if present}

Code:
<code>
{content}
</code>

Write a 1-3 sentence summary of what this {kind} does. Focus on purpose and behavior,
not implementation details. If the code is self-explanatory from its name, a single
sentence is sufficient.

Return ONLY the summary text, no JSON, no preamble.
```

Called from `adapter.extract_with_llm`. For `kind = "file"` chunks: skip (no LLM summary; the file is summarized implicitly by its symbols).

---

### `src/adapter.rs`

`CodeAdapter` implements `SourceAdapter`.

```rust
pub struct CodeAdapter;

impl CodeAdapter {
    pub fn new() -> Self { Self }
}
```

**`kind()`** → `"code"`

**`discover(source)`**: Return one `DiscoveredSource` with `kind = "directory"` for the source path. No expansion — the entire directory is one source.

**`chunk(source)`**: Delegate to `chunker::chunk_directory`. Pass `ChunkOptions` derived from the corpus config JSON (parse `include_globs`, `exclude_globs`, `max_chunk_bytes`).

**`extract_structure(chunk)`**: If chunk `kind == "file"` or is a recognized language chunk → call `extractor::extract_structure`. Return the `ExtractedCodeStructure` mapped to `ExtractedStructure`. For unrecognized file types (should not occur after chunker filter): return empty.

**`extract_with_llm(chunk, llm)`**: If `chunk.kind == "function" || "class" || "interface"` → call `summarizer::summarize_chunk`. Wrap result in `ExtractedSemantic { entities: [], edges: [], summary_text }`. Return `None` for `kind == "file"` or `kind == "module"`.

**`summarize(chunk, llm, depth)`**: For `depth == "corpus"`: collect all file-level `doc_comment` entities (the module-level doc strings), ask LLM for a 3-5 sentence repository overview. For other depths: return `None` (file/function summaries are handled in `extract_with_llm`).

**`resolve_aliases(entities, llm)`**: For code corpora, aliases are typically not needed (symbol names are canonical). Return an empty `Vec<EntityMerge>` without calling the LLM. (Can be wired later for documentation corpora with informal references.)

**`format_location(chunk)`**: `src/<relative_path>#<symbol>` or `src/<relative_path>` for file chunks.

**`parse_location(uri)`**: Parse `src/...#...` pattern. Return `LocationRef { corpus_id, path }`.

---

### `crates/callimachus-cli` — wire code adapter

#### `src/commands/index.rs`

Update the `build_adapter` helper to detect `corpus.kind == "code"` and return `Arc::new(CodeAdapter::new())`.

```rust
fn build_adapter(corpus: &Corpus) -> anyhow::Result<Arc<dyn SourceAdapter>> {
    match corpus.kind.as_str() {
        "book" => Ok(Arc::new(callimachus_adapter_book::create())),
        "code" => Ok(Arc::new(callimachus_adapter_code::create())),
        other => anyhow::bail!("adapter not yet available for corpus kind '{other}'"),
    }
}
```

#### `crates/callimachus-cli/Cargo.toml`

Add:
```toml
callimachus-adapter-code = { path = "../adapters/callimachus-adapter-code" }
```

---

### Export format extension

#### `src/commands/export.rs`

For `ExportFormat::Scip` (reserved in Phase 5): now implement it.

**SCIP (Stack graphs Index Protocol)** is a standard code intelligence format. Emit a SCIP index for code corpora.

For non-code corpora: print "SCIP export is only supported for code corpora (kind=code)." and exit 1.

SCIP output:
- One `Document` per file chunk.
- One `Occurrence` per function/class entity (pointing to its start line).
- Relationships as `Relationship` entries.

Use `serde_json` to emit the SCIP protobuf-JSON format. The full SCIP spec is at <https://github.com/sourcegraph/scip>; implement enough for basic consumption (documents + occurrences, no full symbol table).

This is a best-effort implementation. Document limitations in `--help`.

---

## Tests

### `callimachus-adapter-code`

Create `tests/code_adapter.rs` with integration tests using a small fixture directory.

Fixture: `tests/fixtures/sample_project/` with:
- `src/lib.rs` — 2 functions, 1 struct
- `src/main.rs` — 1 main function, 2 calls
- `src/utils.rs` — 1 helper function

Tests:
- **`discover`**: Returns one `DiscoveredSource` for the fixture directory.
- **`chunk`**: Returns > 0 chunks. All Rust files produce chunks. `parent_path` on function chunks equals the file chunk URI.
- **`extract_structure`**: Function chunks produce entities of `kind = "function"`. Call sites produce `calls` edges. File chunks produce `imports` edges.
- **`extract_with_llm` with DryRunProvider**: Returns valid `ExtractedSemantic`. No panic on any chunk kind.
- **`format_location`** / **`parse_location`**: Round-trip test for all fixture chunk locations.
- **Unsupported extension**: A `.yaml` file in the fixture directory is silently skipped (no error, no chunk).
- **Max chunk size**: A file exceeding `max_chunk_bytes` is split into multiple chunks.

### Unit tests

`src/extractor.rs` `#[cfg(test)]`:
- Parse a small Rust snippet, assert entities and edges extracted correctly.
- Parse a TypeScript class with method, assert `class` entity + `function` entity + `defines` edge.
- Parse a Python file with `import os` statement, assert `imports` edge.

`src/git.rs` `#[cfg(test)]`:
- Test `read_git_info` on the callimachus repo itself (integration test, skipped if `.git` not present): returns Some with valid branch and commit.
- Test on a non-git directory: returns None, no error.

`src/chunker.rs` `#[cfg(test)]`:
- Test exclude globs: `target/**` in exclude list → no chunks from `target/` directory.
- Test include globs: `src/**` in include list → only `src/` files chunked.
- Test that `min_chunk_bytes` drops tiny chunks.

### `callimachus-cli/index` — code corpus

Add to `src/commands/index.rs` `#[cfg(test)]`:
- Test that `corpus.kind == "code"` selects `CodeAdapter` without error.
- Test that `corpus.kind == "wiki"` returns "adapter not yet available" error.

## Acceptance criteria

- `calli corpus add code callimachus /path/to/callimachus` registers a code corpus
- `calli index callimachus --dry-run` runs without errors, prints chunk count
- `calli index callimachus` (with DryRunProvider or real LLM) completes and `calli corpus status callimachus` shows non-zero entity count
- `calli mcp` + `search(corpus_id="callimachus", query="LlmProvider")` returns relevant chunks
- `calli mcp` + `entity(corpus_id="callimachus", name_or_id="QueryService")` resolves the class
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes
