# Phase 9 — Wiki adapter + embeddings

## Context

Phase 8 delivered the HTTP server and distribution pipeline. The system has two adapters (book, code) and a complete query surface. This phase adds the third planned adapter — `callimachus-adapter-wiki` for Markdown directory trees — and wires up the embeddings subsystem that enables semantic search mode.

After this phase:
- Obsidian vaults, GitHub wikis, and MkDocs documentation sites can be indexed.
- `search(mode="semantic")` uses vector similarity instead of FTS5 keyword matching.
- `search(mode="hybrid")` combines both signals with a configurable blend weight.

Reference: `docs/plans/callimachus-standalone.md §3, §6.1 (embed pass)`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 8 commit

## Files to change

---

### `crates/adapters/callimachus-adapter-wiki/Cargo.toml`

Replace the stub.

```toml
[package]
name = "callimachus-adapter-wiki"
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
pulldown-cmark = "0.12"
pulldown-cmark-to-cmark = "15"
regex = "1"
sha2 = "0.10"
hex = "0.4"
```

---

### `crates/adapters/callimachus-adapter-wiki/src/lib.rs`

```rust
pub mod adapter;
pub mod chunker;
pub mod extractor;
pub mod links;
pub mod summarizer;
pub use adapter::WikiAdapter;
pub fn create() -> WikiAdapter { WikiAdapter::new() }
```

---

### `src/links.rs`

Wikilink and Markdown link extraction.

```rust
/// A resolved link found in a wiki page.
pub struct WikiLink {
    pub from_page: String,     // relative path of source page
    pub to_page: String,       // resolved relative path of target page (or bare title)
    pub anchor: Option<String>,
    pub display_text: Option<String>,
    pub kind: WikiLinkKind,
}

pub enum WikiLinkKind {
    Wikilink,       // [[Target Page]] or [[Target Page|Display text]]
    Markdown,       // [Display text](../path/to/page.md)
    External,       // [text](https://...)  — not followed, but recorded
}

/// Extract all links from a Markdown source string.
pub fn extract_links(
    source_page: &str,
    content: &str,
) -> Vec<WikiLink>
```

**Wikilink regex**: `\[\[([^\]|#]+)(?:#([^\]|]*))?(?:\|([^\]]*))?\]\]`
- Group 1: target page title
- Group 2: anchor (optional)
- Group 3: display text (optional)

**Markdown link**: Use `pulldown-cmark` events (`Event::Start(Tag::Link(...))`). Classify as `External` if href starts with `https?://`.

**Resolution**: Wikilink targets are bare page titles. Resolve them to relative paths by scanning the corpus directory for a file whose stem (case-insensitive, spaces→underscores) matches. If no match, store the bare title as `to_page` (unresolved). Unresolved links are still recorded as edges — they may match an entity name.

---

### `src/chunker.rs`

```rust
pub struct WikiChunkOptions {
    pub max_section_bytes: usize,   // default 3000
    pub min_section_bytes: usize,   // default 50
    pub exclude_globs: Vec<String>,
}

impl Default for WikiChunkOptions {
    fn default() -> Self {
        Self {
            max_section_bytes: 3000,
            min_section_bytes: 50,
            exclude_globs: vec![".git/**".into()],
        }
    }
}

pub async fn chunk_wiki_directory(
    source_path: &Path,
    opts: &WikiChunkOptions,
) -> anyhow::Result<Vec<Chunk>>
```

**Chunking strategy:**

1. Walk the directory with `walkdir`. Skip non-`.md` files and excluded globs.
2. For each `.md` file:
   a. Parse with `pulldown-cmark`.
   b. Emit a `kind = "page"` chunk for the whole file (full content). Location: `wiki/<relative-path-without-extension>`.
   c. Split at `##`+ headings. Each heading section = a `kind = "section"` chunk with `parent_path` = the page's location URI.
   d. Top-level (`#`) heading: if present, treat the text before the first `##` as the page's introductory section (kind = "section", also child of the page chunk).
3. Extract front-matter: YAML between `---` fences at file top (use regex; no YAML parser dependency). Treat `title`, `tags`, `aliases` front-matter fields as entity metadata.

**Location URI scheme for wiki:** `calli://<corpus_id>/wiki/<path>/<to>/<page>#<heading-slug>`. Examples:
- `calli://mywiki/wiki/authentication` — a page
- `calli://mywiki/wiki/authentication#oauth-flow` — a section
- `calli://mywiki/wiki/guides/getting-started` — a nested page

Heading slugs follow GitHub's anchor convention: lowercase, spaces→hyphens, strip non-alphanumeric except hyphens.

---

### `src/extractor.rs`

Structural extraction from wiki pages. No LLM.

```rust
pub struct WikiStructure {
    pub page_title: Option<String>,  // from front-matter or first H1
    pub front_matter: serde_json::Value,
    pub tags: Vec<String>,           // from front-matter tags field
    pub aliases: Vec<String>,        // from front-matter aliases field
    pub links: Vec<WikiLink>,
    pub mentioned_entities: Vec<String>,  // bare capitalized terms extracted from prose
}

pub fn extract_structure(chunk: &Chunk, source_path: &Path) -> anyhow::Result<WikiStructure>
```

For `kind = "page"` chunks: extract front-matter, page title, all links. Emit:
- One `Entity` per page (kind = `"topic"` or `"concept"` depending on tag classification — see below).
- `links` → `references` edges (`from` = this page entity, `to` = target page entity, `kind = "references"`).

For `kind = "section"` chunks: extract heading text as entity kind `"section"`. Links within the section → `references` edges at section level.

**Entity kind classification for wiki topics**: If the page has a `type` or `category` front-matter field, use its value. Otherwise default to `"topic"`. Common values: `"person"`, `"organization"`, `"project"`, `"concept"`, `"place"`, `"event"`.

**Mentioned entities**: Extract capitalized multi-word terms (regex: `\b[A-Z][a-z]+(?:\s[A-Z][a-z]+)+\b`) as candidate entity mentions. These become `mentions` edges (low confidence: 0.4). The LLM pass refines them.

---

### `src/summarizer.rs`

LLM summarization for wiki sections and pages.

Prompt for sections:

```
You are summarizing a section of a wiki for a searchable index.

Page: {page_title}
Section: {section_heading}

<content>
{section_content}
</content>

Write a 1-2 sentence summary of this section. Focus on the key concept or information.
Return ONLY the summary text.
```

Prompt for corpus-level summary:

```
You are summarizing a wiki (knowledge base) for a searchable index.

The wiki contains the following pages:
{page_list_with_titles}

Write a 3-5 sentence overview of what this wiki covers.
Return ONLY the summary text.
```

---

### `src/adapter.rs`

`WikiAdapter` implements `SourceAdapter`.

**`kind()`** → `"wiki"`

**`discover(source)`**: Return one `DiscoveredSource` per `.md` file (unlike book/code which return the directory as a single source). This allows per-file change detection in Phase 6 watcher.

**`chunk(source)`**: Delegate to `chunker::chunk_wiki_directory` scoped to the discovered source path. (Since each source is a single file, this produces the page chunk + section chunks for that file.)

**`extract_structure(chunk)`**: Call `extractor::extract_structure`. Map `WikiStructure` to `ExtractedStructure`.

**`extract_with_llm(chunk, llm)`**: For `kind = "section"` chunks: call `summarizer::summarize_section`. For `kind = "page"` chunks: skip (page summary is generated in `summarize_pass` from its section summaries). Return `ExtractedSemantic`.

**`summarize(chunk, llm, depth)`**:
- `"section"`: already handled in `extract_with_llm`; return `None` here.
- `"page"`: collect this page's section summaries, ask LLM for 2-3 sentence page summary.
- `"corpus"`: collect all page titles + summaries, ask LLM for corpus overview.

**`resolve_aliases(entities, llm)`**: For wiki corpora, aliases often exist in front-matter. Combine front-matter aliases (high confidence) with LLM alias suggestions (same prompt as book adapter). Return merges.

**`format_location(chunk)`** / **`parse_location(uri)`**: Implement `wiki/<path>#<slug>` scheme.

---

### Wire wiki adapter into CLI

#### `src/commands/index.rs`

Add `"wiki"` branch to `build_adapter`:

```rust
"wiki" => Ok(Arc::new(callimachus_adapter_wiki::create())),
```

#### `crates/callimachus-cli/Cargo.toml`

```toml
callimachus-adapter-wiki = { path = "../adapters/callimachus-adapter-wiki" }
```

---

### `crates/callimachus-core` — embed pass

New migration: `migrations/003_embeddings.sql`.

```sql
CREATE TABLE embeddings (
    id TEXT PRIMARY KEY,
    corpus_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL,
    model TEXT NOT NULL,
    vector BLOB NOT NULL,        -- raw f32 bytes, little-endian
    dimensions INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE,
    FOREIGN KEY (chunk_id) REFERENCES chunks(id) ON DELETE CASCADE
);

CREATE INDEX idx_embeddings_corpus ON embeddings(corpus_id);
CREATE INDEX idx_embeddings_chunk ON embeddings(chunk_id);
```

Note: This phase does not use `sqlite-vec` (an external C extension). Vectors are stored as raw BLOB. Similarity search is done in Rust (load all vectors for a corpus into memory, compute cosine similarity in a loop). This is adequate for corpora up to ~50k chunks. A future phase can switch to `sqlite-vec` for larger corpora.

#### `src/storage/embedding_store.rs` (new)

```rust
pub struct StoredEmbedding {
    pub id: String,
    pub corpus_id: String,
    pub chunk_id: String,
    pub model: String,
    pub vector: Vec<f32>,
    pub dimensions: usize,
}

pub fn upsert(db: &Database, embedding: &StoredEmbedding) -> Result<()>
pub fn get_for_chunk(db: &Database, chunk_id: &str) -> Result<Option<StoredEmbedding>>
pub fn list_for_corpus(db: &Database, corpus_id: &str) -> Result<Vec<StoredEmbedding>>
pub fn count(db: &Database, corpus_id: &str) -> Result<u64>
```

`upsert`: `INSERT OR REPLACE`. Vector stored as `f32` little-endian bytes (`bytemuck::cast_slice`).

`list_for_corpus`: Returns all embeddings. Caller is responsible for memory — document that this loads all vectors for the corpus.

Add `bytemuck = "1"` to `callimachus-core/Cargo.toml`.

#### `src/indexing/embed_pass.rs`

```rust
pub async fn run(
    db: &Mutex<Database>,
    corpus: &Corpus,
    llm: &dyn LlmProvider,
    opts: &IndexOptions,
) -> anyhow::Result<PassStats>
```

- Iterate chunks that don't have a stored embedding (`embedding_store::get_for_chunk` returns None).
- For each chunk: call `llm.embed(chunk.content)` (new method on `LlmProvider` — see below).
- Store result via `embedding_store::upsert`.
- Skip already-embedded chunks (idempotent).
- Progress: `tracing::info!` every 25 chunks.

The embed pass is not in `IndexOptions::default().passes` — it must be explicitly requested via `--pass=embed` or `--pass=all`.

#### `LlmProvider` trait extension

Add `embed` to the `LlmProvider` trait in `callimachus-llm/src/provider.rs`:

```rust
/// Generate an embedding vector for a single text input.
/// Returns Err if the provider does not support embeddings.
async fn embed(&self, text: &str) -> Result<Vec<f32>> {
    Err(LlmError::Other("embeddings not supported by this provider".into()))
}

/// Whether this provider supports embedding generation.
fn supports_embeddings(&self) -> bool { false }
```

Default implementation returns an error — only providers that support embeddings override it.

**`AnthropicApiProvider`**: Anthropic does not currently offer an embeddings API. Return `LlmError::Other("Anthropic API does not support embeddings; configure an embedding provider")`. `supports_embeddings() → false`.

**Embedding provider**: Add a new struct `OpenAiEmbeddingProvider` in `callimachus-llm/src/openai_embed.rs`:

```rust
pub struct OpenAiEmbeddingProvider {
    api_key: String,
    model: String,   // default: "text-embedding-3-small"
}

impl OpenAiEmbeddingProvider {
    pub fn new(api_key: String, model: Option<String>) -> Self { ... }
    pub fn from_env() -> Result<Self> { ... }  // reads OPENAI_API_KEY
}
```

Implements only `embed` + `supports_embeddings` (returns `true`). Does not implement `complete` (panics with "OpenAiEmbeddingProvider is for embeddings only"). This is an unusual trait impl — document it clearly.

The embed pass uses a **separate** provider instance for embeddings. The pipeline now takes two providers:

```rust
pub struct IndexPipeline {
    pub db: Arc<Mutex<Database>>,
    pub adapter: Arc<dyn SourceAdapter>,
    pub llm: Arc<dyn LlmProvider>,
    pub embedder: Option<Arc<dyn LlmProvider>>,  // None = skip embed pass
}
```

If `embedder` is `None` and `Pass::Embed` is requested, print a warning and skip.

**Config addition** (per-corpus):
```toml
[embedding]
enabled = true
provider = "openai"              # only supported embedding provider in v1
model = "text-embedding-3-small"
dimensions = 1536
```

Global config:
```toml
[embedding]
openai_api_key = "sk-..."        # or use OPENAI_API_KEY env var
```

---

### `crates/callimachus-core/query` — semantic search

Update `src/query/search.rs`.

```rust
pub fn semantic_search(
    db: &Database,
    corpus_id: &str,
    query_vector: &[f32],
    scope: Option<&Scope>,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>>
```

1. Load all embeddings for corpus via `embedding_store::list_for_corpus`.
2. Compute cosine similarity between `query_vector` and each stored vector.
3. Sort by similarity descending. Apply `scope` filtering (same position-based exclude as keyword search).
4. Return top `limit` results. `relevance = cosine_similarity` (already in 0..1 range for unit vectors).
5. Snippets: load the chunk for each result and truncate to 500 chars.

**Hybrid search**: Compute both keyword scores and semantic scores, normalize each to 0..1, blend with configurable weight `α`:

```
hybrid_score = α * semantic_score + (1 - α) * keyword_score
```

Default `α = 0.5`. Configurable per request via `SearchInput.semantic_weight: Option<f32>`.

For the query vector: `QueryService.search` now takes an optional `embedder` Arc. If `mode = Semantic` or `Hybrid` and no embedder → fall back to keyword search with a `tracing::warn!`.

Update `QueryService` to optionally hold an embedder:

```rust
pub struct QueryService {
    db: Arc<Mutex<Database>>,
    corrections: Option<CorrectionsEngine>,
    embedder: Option<Arc<dyn LlmProvider>>,
}
```

---

## Tests

### `callimachus-adapter-wiki`

Create `tests/wiki_adapter.rs` with fixture: `tests/fixtures/sample_wiki/` containing:

```
tests/fixtures/sample_wiki/
  index.md          — home page with links to two others
  characters.md     — [[Eisenhorn]] and [[Bequin]] stubs
  places.md         — Gudrun, Thracian Primaris
  _images/          — should be ignored (no .md files)
```

`characters.md` has YAML front-matter: `type: character-index`, `tags: [characters, wh40k]`.

Tests:
- **`discover`**: Returns 3 `DiscoveredSource` entries (one per `.md` file; `_images/` excluded).
- **`chunk`**: 3 pages produce ≥ 3 `kind=page` chunks + section chunks. Location URIs use `wiki/` prefix.
- **`extract_structure`**: `characters.md` chunk produces entity with `kind = "topic"`. Wikilinks produce `references` edges.
- **`extract_with_llm` with DryRunProvider**: Returns `ExtractedSemantic` for section chunks; None for page chunks.
- **`resolve_aliases`**: Front-matter aliases are returned without LLM call.
- **`format_location` / `parse_location`**: Round-trip for all fixture locations.
- **Heading slug**: `## OAuth Flow` → slug `oauth-flow`.
- **Wikilink extraction**: `[[Eisenhorn|the Inquisitor]]` → `WikiLink { to_page: "Eisenhorn", display_text: Some("the Inquisitor") }`.

### `callimachus-core/embed_pass`

`src/indexing/embed_pass.rs` `#[cfg(test)]`:

- Seed 3 chunks. Run embed_pass with a mock embedder that returns `vec![0.1f32; 8]`. Assert 3 embeddings stored.
- Run embed_pass again on same corpus → `processed = 0, skipped = 3` (idempotent).
- Run embed_pass with `embedder = None` → no embeddings stored, no error.

### `callimachus-core/semantic_search`

`src/query/search.rs` `#[cfg(test)]`:

- Seed 3 chunks with embeddings: chunk A = `[1.0, 0.0]`, B = `[0.7, 0.7]`, C = `[0.0, 1.0]`.
- Query vector `[1.0, 0.0]` → result order: A (1.0), B (0.99), C (0.0).
- Hybrid search: seed FTS matching only chunk C, query vector close to A. With `α=0.8` (heavy semantic), A ranks higher. With `α=0.2` (heavy keyword), C ranks higher.
- Scope filtering: exclude chunks after a position URI → semantic search obeys the same exclusion.

### `callimachus-adapter-wiki` + embed pass integration

`tests/wiki_embed_integration.rs`:

- Index the sample_wiki fixture end-to-end with DryRunProvider for LLM + a mock embedder.
- Assert all `kind = "section"` chunks have embeddings after embed pass.
- Assert `kind = "page"` chunks also have embeddings (embed pass covers all chunks).
- Assert semantic search returns results (mock embedder vectors are deterministic — seeded as `[float(i) for i in range(dims)]`).

## Acceptance criteria

- `calli corpus add wiki mywiki /path/to/vault` registers a wiki corpus
- `calli index mywiki` completes without errors on a real Obsidian vault (manual test)
- `calli mcp` + `search(corpus_id="mywiki", query="authentication")` returns relevant pages
- `calli mcp` + `search(corpus_id="mywiki", query="authentication", mode="semantic")` falls back to keyword search with a log warning when no embedder is configured
- With `OPENAI_API_KEY` and `embedding.enabled = true`: embed pass runs, semantic search returns results
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes

## Post-phase notes

With Phase 9 complete, all 9 planned phases are done. The system is:

- Fully installable (`cargo install`, Homebrew, binary releases)
- Three adapters: book, code, wiki
- Two query modes: keyword (FTS5), semantic (in-memory cosine)
- Full corrections overlay
- Live watcher + incremental reindex
- HTTP REST API + MCP stdio server
- Claude Desktop configured with no API key (Claude Code subscription mode) or with `ANTHROPIC_API_KEY`

Suggested v2 priorities (per §10 requirements gaps):
- PDF adapter (§10.4)
- Cross-corpus entity linking — `entity_link` tool (§10.14)
- `sqlite-vec` extension for large-corpus semantic search
- Opt-in telemetry (§10.12)
- `--cost-aware` mode (§1.6)
