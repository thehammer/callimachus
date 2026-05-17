# Phase 3 — Indexing pipeline + book adapter

## Context

Phase 2 delivered `callimachus-llm` with `AnthropicApiProvider`, `ClaudeCodeProvider`, and
`DryRunProvider`. The `callimachus-adapter-book` and `callimachus-core` indexing modules are stubs.

This phase wires everything together end-to-end: `calli index <corpus_id>` reads an EPUB (or
Markdown, or plain text), chunks it, extracts entities and edges with an LLM, summarizes, and
writes everything into the SQLite index. The result is a fully queryable corpus — ready for the
MCP tool surface in Phase 4.

Reference: `docs/plans/callimachus-standalone.md §3, §6`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 2 commit

## Files to change

---

### `crates/callimachus-core` — adapter contract

#### `src/adapter/contract.rs` (new)

Define the `SourceAdapter` trait. This is the extension point every adapter implements.

```rust
#[async_trait::async_trait]
pub trait SourceAdapter: Send + Sync {
    fn kind(&self) -> &str;
    fn version(&self) -> &str;

    // Discovery: expand a source path/URL into concrete inputs.
    async fn discover(&self, source: &str) -> anyhow::Result<Vec<DiscoveredSource>>;

    // Chunking: yield chunks from a discovered source.
    // Returns an async stream; use `async_stream` or a Vec for simplicity in v1.
    async fn chunk(&self, source: &DiscoveredSource) -> anyhow::Result<Vec<Chunk>>;

    // Structural extraction (parser-driven, no LLM). May return empty.
    async fn extract_structure(&self, chunk: &Chunk) -> anyhow::Result<ExtractedStructure>;

    // LLM-driven semantic extraction. Optional — adapters may return None to skip.
    async fn extract_with_llm(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Option<ExtractedSemantic>>;

    // Summarize a chunk. Optional — adapters may return None to skip.
    async fn summarize(
        &self,
        chunk: &Chunk,
        llm: &dyn LlmProvider,
        depth: &str,
    ) -> anyhow::Result<Option<String>>;

    // Alias resolution across the full entity set for a corpus.
    // Called once after all chunks are semantically processed.
    async fn resolve_aliases(
        &self,
        entities: &[Entity],
        llm: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<EntityMerge>>;

    // Canonical location URI for a chunk (e.g. "ch/3/sc/7").
    fn format_location(&self, chunk: &Chunk) -> String;
    fn parse_location(&self, uri: &str) -> anyhow::Result<LocationRef>;
}

pub struct DiscoveredSource {
    pub path: String,
    pub kind: String,   // "epub", "markdown", "text"
    pub meta: serde_json::Value,
}

pub struct ExtractedStructure {
    pub parent_path: Option<String>,
    pub child_paths: Vec<String>,
    pub structural_entities: Vec<Entity>,
    pub structural_edges: Vec<Edge>,
}

pub struct ExtractedSemantic {
    pub entities: Vec<Entity>,
    pub edges: Vec<Edge>,
    pub summary_text: Option<String>,
}

pub struct EntityMerge {
    pub keep_id: String,
    pub absorb_id: String,
    pub reason: String,
}

pub struct LocationRef {
    pub corpus_id: String,
    pub path: String,
}
```

#### `src/adapter/registry.rs` (new)

`AdapterRegistry`: a `HashMap<String, Arc<dyn SourceAdapter>>`. Methods: `register`, `get`, `list`. Used by the pipeline to look up the right adapter for a corpus.

#### `src/adapter/mod.rs` (new)

```rust
pub mod contract;
pub mod registry;
pub use contract::*;
pub use registry::AdapterRegistry;
```

---

### `crates/callimachus-core` — indexing pipeline

New module: `src/indexing/`. Add `pub mod indexing;` to `src/lib.rs`.

#### `src/indexing/mod.rs`

```rust
pub mod chunk_pass;
pub mod structure_pass;
pub mod semantic_pass;
pub mod summarize_pass;
pub mod pipeline;
pub use pipeline::{IndexPipeline, IndexOptions, IndexResult};
```

#### `src/indexing/pipeline.rs`

`IndexPipeline` is the orchestrator.

```rust
pub struct IndexOptions {
    pub passes: Vec<Pass>,          // default: all except Embed
    pub from_chunk: Option<String>, // resume from this chunk ID
    pub dry_run: bool,
    pub concurrency: Option<usize>, // None = provider decides
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            passes: vec![Pass::Chunk, Pass::Structure, Pass::Semantic, Pass::Summarize],
            from_chunk: None,
            dry_run: false,
            concurrency: None,
        }
    }
}

pub struct IndexResult {
    pub total_chunks: u64,
    pub total_entities: u64,
    pub total_edges: u64,
    pub cost_usd: f64,
    pub runs: Vec<RunRecord>,
}

pub struct IndexPipeline {
    pub db: Arc<Mutex<Database>>,
    pub adapter: Arc<dyn SourceAdapter>,
    pub llm: Arc<dyn LlmProvider>,
}

impl IndexPipeline {
    pub async fn run(&self, corpus: &Corpus, opts: IndexOptions) -> anyhow::Result<IndexResult> {
        // Run each requested pass in order, respecting resumability.
        // Emit progress to a callback or tracing::info!.
    }
}
```

Pipeline runs passes in order. Each pass:
1. Calls `run_log::start_run(db, corpus_id, pass, provider_name)`
2. Executes the pass
3. Calls `run_log::finish_run(db, run_id, status, stats)`

Progress: emit `tracing::info!` every 25 chunks: `"[{pass}] {processed}/{total} chunks"`.

#### `src/indexing/chunk_pass.rs`

```rust
pub async fn run(
    db: &Mutex<Database>,
    corpus: &Corpus,
    adapter: &dyn SourceAdapter,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats>
```

- Call `adapter.discover(corpus.source)` to get sources.
- For each source, call `adapter.chunk(source)`.
- For each chunk: if `opts.dry_run`, count but don't write. Otherwise call `chunk_store::upsert`. Skip (`skipped++`) if `chunk_store::has` returns true.
- If `opts.from_chunk` is set, skip chunks until the matching ID is seen, then start processing.
- Return `PassStats` with processed/skipped/failed counts.

#### `src/indexing/structure_pass.rs`

```rust
pub async fn run(...) -> anyhow::Result<PassStats>
```

- Iterate all chunks for the corpus via `chunk_store::list`.
- Call `adapter.extract_structure(chunk)` for each.
- Backfill `parent_path` on the chunk row if the structural result provides one.
- Upsert structural entities and edges.
- Idempotent: skip chunks whose `parent_path` is already populated.

#### `src/indexing/semantic_pass.rs`

```rust
pub async fn run(...) -> anyhow::Result<PassStats>
```

- Iterate chunks that have not been semantically processed (`semantic_processed = 0` column).
- Call `adapter.extract_with_llm(chunk, llm)`.
- If `LlmProvider::supports_parallel()` is true, process in batches of `opts.concurrency.unwrap_or(5)` using `tokio::spawn` + `JoinSet`.
- If false (Claude Code), process sequentially.
- On success: upsert entities and edges, set `semantic_processed = 1` on the chunk row.
- On `LlmError::RateLimited`: backoff and retry (up to 3 times).
- On permanent failure: log to `stats.errors`, mark `failed++`, continue.
- After all chunks: call `adapter.resolve_aliases(all_entities, llm)` and apply merges via `entity_store::merge`.

The `semantic_processed` column already exists in the schema (`001_initial.sql`).

#### `src/indexing/summarize_pass.rs`

```rust
pub async fn run(...) -> anyhow::Result<PassStats>
```

Generate summaries bottom-up:
1. Scene-level: chunks of kind `"scene"` that don't have a stored summary.
2. Chapter-level: for each chapter chunk, collect its scene summaries and pass them to `adapter.summarize(chunk, llm, "chapter")`. The prompt includes scene summaries as context — don't re-read full content.
3. Corpus-level: collect all chapter summaries, call `adapter.summarize(corpus_chunk, llm, "corpus")`.

Skip any level that already has a stored summary (idempotent). Store via `summary_store::upsert`.

---

### `crates/callimachus-adapter-book`

Replace the placeholder with a full implementation.

#### `Cargo.toml`

Add:
- `epub = "2"` — EPUB parsing
- `html2text = "0.6"` — strip HTML tags from EPUB content
- `pulldown-cmark = "0.12"` — Markdown parsing
- `regex = "1"` — scene splitting
- `serde_json.workspace = true`
- `async-trait = "0.1"`

#### `src/lib.rs`

```rust
pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod summarizer;
pub mod resolver;
pub use adapter::BookAdapter;
pub fn create() -> BookAdapter { BookAdapter::new() }
```

#### `src/adapter.rs`

`BookAdapter` implements `SourceAdapter`. A zero-field struct (all config comes from the corpus config JSON).

**`discover`**: Inspect the source path extension. Return one `DiscoveredSource` with `kind = "epub" | "markdown" | "text"`.

**`chunk`**: Delegate to `chunker::chunk_epub`, `chunker::chunk_markdown`, or `chunker::chunk_text` based on source kind.

**`extract_structure`**: For EPUB/Markdown, extract chapter→scene containment. Return `parent_path` for scene chunks. No LLM call.

**`extract_with_llm`**: Build a prompt asking the LLM to return JSON with `entities`, `edges`, and `summary_text`. Parse the JSON response. Return `ExtractedSemantic`. Prompt template:

```
You are indexing a passage from a book for a searchable database.

Extract the following from the passage below:
1. Named entities (characters, places, organizations, objects) with their kind and a brief description.
2. Relationships (edges) between entities observed in this passage.
3. A 1-3 sentence summary of what happens in this passage.

Return ONLY valid JSON in this exact shape:
{
  "entities": [{"name": "...", "kind": "character|place|organization|object", "description": "..."}],
  "edges": [{"from": "...", "to": "...", "kind": "meets|located_in|allied_with|mentions"}],
  "summary_text": "..."
}

Passage:
<passage>
{CONTENT}
</passage>
```

Wrap content in XML tags to reduce prompt injection risk (addresses §10.11 of the standalone plan).

**`summarize`**: Build a depth-appropriate prompt. For `"chapter"`: pass scene summaries as bullet points, ask for a 2-4 sentence chapter summary. For `"corpus"`: pass chapter summaries, ask for a 3-5 sentence overview.

**`resolve_aliases`**: Build a prompt listing all entity canonical names + aliases. Ask the LLM to identify which names refer to the same entity (e.g. "Eisenhorn", "Gregor", "the Inquisitor"). Return merge suggestions.

**`format_location`**: `ch/{chapter_num}` for chapters, `ch/{chapter_num}/sc/{scene_num}` for scenes.

**`parse_location`**: Parse `ch/N` and `ch/N/sc/M` patterns.

#### `src/chunker.rs`

**`chunk_epub(source) -> anyhow::Result<Vec<Chunk>>`**

Use the `epub` crate to read the EPUB spine. For each spine item (chapter):
- Extract text content using `html2text`.
- Split into scenes using a blank-line heuristic: 2+ consecutive blank lines = scene boundary.
- Minimum scene length: 200 chars. Merge short scenes with the next.
- Maximum scene length: 4000 chars. Split at sentence boundaries.
- Assign `kind = "chapter"` to the parent, `kind = "scene"` to children.
- `parent_path` for scenes = their chapter's path.

**`chunk_markdown(source) -> anyhow::Result<Vec<Chunk>>`**

Use `pulldown-cmark`. Split at `#` and `##` headings. Top-level headings = chapters; second-level = scenes.

**`chunk_text(source) -> anyhow::Result<Vec<Chunk>>`**

Read file, split on 3+ consecutive blank lines (part boundaries) then 2 blank lines (section boundaries).

---

### `crates/callimachus-cli` — `calli index`

Replace the `not_yet("index", ...)` stub with a full implementation.

#### `src/commands/index.rs` (new)

```rust
pub async fn run(
    corpus_id: &str,
    pass: Option<String>,
    from_chunk: Option<String>,
    dry_run: bool,
    provider_override: Option<String>,
    db: &Database,
    config: &GlobalConfig,
) -> anyhow::Result<()>
```

- Load corpus from DB (`corpus_store::require`).
- Build `LlmProvider` from config/env (using `callimachus_llm::resolve::auto_detect` or the `--provider` flag).
- Build `BookAdapter` (for now, only "book" kind; other kinds → "adapter not yet available" error).
- Build `IndexOptions` from CLI flags.
- Construct `IndexPipeline` and call `run`.
- Print progress as runs complete; print final summary: chunks indexed, entities found, cost.

Add `callimachus-llm = { path = "../callimachus-llm" }` and `callimachus-adapter-book = { path = "../adapters/callimachus-adapter-book" }` to CLI's `Cargo.toml`.

Update `src/main.rs`: wire `Command::Index` to `commands::index::run`. Remove `not_yet("index", ...)`.

---

### `src/lib.rs` updates

Add `pub mod adapter;` and `pub mod indexing;` to `crates/callimachus-core/src/lib.rs`. Re-export `AdapterRegistry`, `IndexPipeline`, `IndexOptions`, `IndexResult`.

---

## Tests

### `callimachus-adapter-book`

Create `tests/book_adapter.rs` with an integration test:
- Use the Project Gutenberg plaintext of a short public-domain work (e.g. a chapter of _A Tale of Two Cities_). Include a small `.txt` fixture in `tests/fixtures/`.
- Test `discover` returns one source.
- Test `chunk` returns >0 chunks with valid location URIs.
- Test `extract_structure` returns scene chunks with correct `parent_path`.
- Test `extract_with_llm` with `DryRunProvider` returns valid `ExtractedSemantic`.
- Test `summarize` with `DryRunProvider` returns non-empty string.
- Test `resolve_aliases` with a fixture entity list returns without error.

### `callimachus-core/indexing`

In `src/indexing/pipeline.rs` under `#[cfg(test)]`:
- Open in-memory DB, create a corpus, run all passes with `DryRunProvider` and a small fixture.
- Assert chunk count > 0, entity count ≥ 0, no panics, run log shows 4 completed runs.
- Test resumability: run chunk pass, then run again — `skipped` count should equal previous `processed`.

### `callimachus-cli`

In `src/commands/index.rs` under `#[cfg(test)]`:
- Test that `--dry-run` completes without writing any chunks.
- Test that invalid corpus ID returns a clear error.

## Acceptance criteria

- `calli index xenos` on the Xenos EPUB completes without errors (manual test, DryRun or real LLM)
- `calli corpus status xenos` shows non-zero chunk count after indexing
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes
