# Phase 4 — MCP tool surface

## Context

Phase 3 delivered the indexing pipeline and book adapter. A corpus can now be fully indexed into
SQLite. This phase exposes that index to LLMs via the Model Context Protocol — implementing all
12 tools in `callimachus-core/query`, wiring them into the `callimachus-mcp` stdio server, and
connecting `calli mcp` in the CLI.

After this phase, Claude Desktop can be pointed at `calli mcp` and query any indexed corpus.

Reference: `docs/plans/callimachus-standalone.md §1.5, §5`.

## Target

- **Repo:** `callimachus`
- **Branch:** `main` (trunk-based)
- **Base:** Phase 3 commit

## Files to change

---

### `crates/callimachus-core` — QueryService

New module: `src/query/`. Add `pub mod query;` to `src/lib.rs`.

#### `src/query/mod.rs`

```rust
pub mod service;
pub mod search;
pub mod types;
pub use service::QueryService;
```

#### `src/query/types.rs`

Input and output types for all 12 tools. These are the wire types — separate from the domain
types in `src/types/`. Use `serde::Serialize`/`Deserialize` throughout.

One struct per tool input/output. Follow the shapes in `docs/plans/callimachus-standalone.md §5`
exactly. Key shapes:

```rust
// corpus_list
pub struct CorpusListEntry { pub id, pub name, pub kind, pub last_indexed, pub chunk_count, pub entity_count }

// search
pub struct SearchInput { pub corpus_id, pub query, pub mode: SearchMode, pub scope: Option<Scope>, pub limit: Option<u32> }
pub enum SearchMode { Keyword, Semantic, Hybrid }
pub struct SearchResult { pub location: Location, pub snippet: String, pub relevance: f32, pub kind: String }

// entity
pub struct EntityInput { pub corpus_id, pub name_or_id }

// entity_edges
pub struct EntityEdgesInput { pub corpus_id, pub entity_id, pub direction: String, pub kind: Option<String>, pub limit: Option<u32> }

// entity_meet
pub struct EntityMeetInput { pub corpus_id, pub entity_a, pub entity_b }
pub struct EntityMeetOutput { pub first_co_occurrence: Location, pub all: Vec<Location>, pub count: u32 }

// read
pub struct ReadInput { pub corpus_id: Option<String>, pub location: String, pub depth: ReadDepth }
pub enum ReadDepth { Summary, Scenes, Full }
pub struct ReadOutput { pub location: Location, pub summary: Option<String>, pub content: Option<String>, pub entities_present: Vec<Entity>, pub child_locations: Vec<Location> }

// summarize
pub struct SummarizeInput { pub corpus_id, pub target: SummarizeTarget }
pub enum SummarizeTarget { Corpus, Entity { entity_id }, Location { location }, Range { from, to } }

// related
pub struct RelatedInput { pub corpus_id, pub location, pub limit: Option<u32> }
pub struct RelatedItem { pub location: Location, pub relationship: String, pub score: f32 }

// chapter_summary (composite)
pub struct ChapterSummaryInput { pub corpus_id, pub chapter: String }  // "3" or "Three" or chapter title

// character_profile (composite)
pub struct CharacterProfileInput { pub corpus_id, pub name }
pub struct CharacterProfileOutput { pub entity: Entity, pub edges: Vec<Edge>, pub summary: Option<String> }

// find_scene (composite)
pub struct FindSceneInput { pub corpus_id, pub entity_a, pub entity_b }
pub struct FindSceneOutput { pub location: Location, pub content: String, pub entities_present: Vec<Entity> }
```

#### `src/query/search.rs`

Keyword search implementation over FTS5.

```rust
pub fn keyword_search(
    db: &Database,
    corpus_id: &str,
    query: &str,
    scope: Option<&Scope>,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>>
```

Uses `storage::fts::search`. Maps `FtsResult` → `SearchResult`. Relevance is normalized from FTS5 rank (rank is negative in SQLite FTS5; invert and normalize to 0..1).

Semantic search (`SearchMode::Semantic`): returns empty results with a note in Phase 4 (embeddings are Phase 9). Hybrid = keyword only for now.

#### `src/query/service.rs`

`QueryService` holds an `Arc<Mutex<Database>>`. All 12 methods take `&self` and return
`ToolResult<T>`.

```rust
pub struct QueryService {
    db: Arc<Mutex<Database>>,
}

impl QueryService {
    pub fn new(db: Arc<Mutex<Database>>) -> Self { ... }

    pub fn corpus_list(&self, _input: CorpusListInput) -> ToolResult<Vec<CorpusListEntry>> { ... }
    pub fn corpus_overview(&self, input: CorpusOverviewInput) -> ToolResult<CorpusOverviewOutput> { ... }
    pub fn search(&self, input: SearchInput) -> ToolResult<SearchOutput> { ... }
    pub fn entity(&self, input: EntityInput) -> ToolResult<Entity> { ... }
    pub fn entity_edges(&self, input: EntityEdgesInput) -> ToolResult<EntityEdgesOutput> { ... }
    pub fn entity_meet(&self, input: EntityMeetInput) -> ToolResult<EntityMeetOutput> { ... }
    pub fn read(&self, input: ReadInput) -> ToolResult<ReadOutput> { ... }
    pub fn summarize(&self, input: SummarizeInput) -> ToolResult<SummarizeOutput> { ... }
    pub fn related(&self, input: RelatedInput) -> ToolResult<RelatedOutput> { ... }
    pub fn chapter_summary(&self, input: ChapterSummaryInput) -> ToolResult<ReadOutput> { ... }
    pub fn character_profile(&self, input: CharacterProfileInput) -> ToolResult<CharacterProfileOutput> { ... }
    pub fn find_scene(&self, input: FindSceneInput) -> ToolResult<FindSceneOutput> { ... }
}
```

Implementation notes per tool:

**`corpus_list`**: `corpus_store::list` + counts from `chunk_store::count` and `entity_store::count`.

**`corpus_overview`**: corpus row + top 10 entities by appearance_count + corpus-level summary from `summary_store`.

**`search`**: delegate to `search::keyword_search`. Scope filtering: if `scope.position` is set, exclude chunks whose location_uri sorts after the position URI (lexicographic — works for `ch/N/sc/M` paths).

**`entity`**: Try `entity_store::get_by_id` first, then `entity_store::find_by_name`. If multiple matches → `ToolResult::ambiguous(candidate_names)`. If zero → `ToolResult::not_found(Some(suggestions))` where suggestions come from a fuzzy name search (LIKE `%query%` on canonical_name).

**`entity_edges`**: `edge_store::get_for_entity` with direction + kind filter.

**`entity_meet`**: Find all chunks where both entity_a and entity_b appear. Query: join `entities` table to find both entity IDs, then find chunks where both appear by searching `edges` for edges involving both, and cross-referencing. First occurrence = minimum location URI (lexicographic order for `ch/N/sc/M`).

**`read`**: Fetch chunk by location URI from `chunk_store::get`. For `depth=Summary`: return only `summary` field (from `summary_store`). For `depth=Scenes`: return child chunk locations only. For `depth=Full`: return full `content`. Always include `entities_present` (entities whose `first_location_uri` or `last_location_uri` is this chunk's URI) and `child_locations`.

**`summarize`**: Look up in `summary_store`. If not found → `ToolResult::not_found(Some(vec!["Run `calli index <corpus_id> --pass=summarize` to generate summaries"]))`.

**`related`**: Score relatedness as Jaccard similarity of entity sets between the target chunk and all other chunks in the corpus. Return top N. For Phase 4, precompute entity sets from `edges` table. This is O(n) but acceptable for corpus sizes we're targeting.

**`chapter_summary`**: Parse input as chapter number or fuzzy title match. Delegate to `read` with `depth=Summary`.

**`character_profile`**: Call `entity` + `entity_edges(both)` + `summarize(entity)`. If entity lookup returns ambiguous → surface that.

**`find_scene`**: Call `entity_meet` → take `first_co_occurrence` → call `read(full)`.

---

### `crates/callimachus-mcp`

Replace the placeholder entirely.

#### `Cargo.toml`

Add:
- `callimachus-core = { path = "../callimachus-core" }`
- `serde.workspace = true`, `serde_json.workspace = true`
- `tokio.workspace = true`
- `tracing.workspace = true`

#### `src/lib.rs`

```rust
pub mod server;
pub mod dispatch;
pub mod tools;
pub use server::McpServer;
```

#### `src/tools.rs`

Tool registry: name → description → JSON Schema for input. One entry per tool. JSON schemas are hand-written `serde_json::Value` objects matching the types in `query/types.rs`. All 12 tools listed.

Use a `const` array or `once_cell::Lazy` to avoid recomputing on every `tools/list` call.

#### `src/dispatch.rs`

```rust
pub async fn dispatch(
    qs: &QueryService,
    name: &str,
    args: serde_json::Value,
) -> serde_json::Value
```

Match on `name`, deserialize `args` into the appropriate input type, call the `QueryService` method, serialize the result. On deserialization failure → return `ToolResult::invalid_input(...)`.

#### `src/server.rs`

Stdio JSON-RPC loop — read newline-delimited JSON from stdin, write responses to stdout, diagnostics to stderr.

Implement the MCP protocol methods:
- `initialize` → return `{protocolVersion, capabilities: {tools: {}}, serverInfo: {name: "callimachus", version: "..."}}`
- `notifications/initialized` → no response (it's a notification)
- `ping` → `{}`
- `tools/list` → `{tools: TOOL_LIST}`
- `tools/call` → dispatch via `dispatch::dispatch`, wrap in `{content: [{type: "text", text: JSON}], isError: bool}`

Supported protocol versions: `["2024-11-05", "2025-03-26"]`. Negotiate to client's version if supported, else fall back to `2024-11-05`.

```rust
pub struct McpServer {
    qs: QueryService,
}

impl McpServer {
    pub fn new(qs: QueryService) -> Self { ... }
    pub async fn run(&self) -> anyhow::Result<()> { ... }
}
```

The `run` loop: read stdin with `BufReader`, process lines, write to stdout. Handle `SIGINT`/`SIGTERM` for graceful shutdown.

---

### `crates/callimachus-cli`

#### `src/commands/mcp.rs` (new)

```rust
pub async fn run(db_path: &Path) -> anyhow::Result<()> {
    let db = Arc::new(Mutex::new(Database::open(db_path)?));
    let qs = QueryService::new(db);
    let server = McpServer::new(qs);
    server.run().await
}
```

#### `src/main.rs`

- Wire `Command::Mcp` to `commands::mcp::run`. Remove `not_yet_plain("mcp")`.
- Add `callimachus-mcp = { path = "../callimachus-mcp" }` to CLI `Cargo.toml`.
- Main must be `#[tokio::main]` (if not already).

---

## Tests

### `callimachus-core/query`

`src/query/service.rs` `#[cfg(test)]`:
- For each tool: open in-memory DB, seed with fixture data (insert a corpus, chunks, entities, edges), call the tool, assert expected shape.
- `corpus_list`: 2 corpora → returns 2 entries with correct counts.
- `search`: seed 3 chunks, search for a term present in 2 → returns 2 results.
- `entity`: exact name match, alias match, not-found with suggestion, ambiguous.
- `entity_edges`: seed edges, test inbound/outbound/both directions.
- `entity_meet`: seed two entities in same chunk → first_co_occurrence correct.
- `read`: summary/scenes/full depth, entities_present populated.
- `summarize`: stored → returns it; not stored → not_found with suggestion.
- `related`: 3 chunks, 2 share an entity → top related includes the shared one.
- Composite tools: assert they compose correctly (no extra DB calls beyond what primitives do).

### `callimachus-mcp`

`src/server.rs` `#[cfg(test)]`:
- Drive the server with a fake stdin/stdout via `tokio::io::duplex`.
- Test `initialize` handshake returns correct capabilities.
- Test `tools/list` returns all 12 tools.
- Test `tools/call` with `corpus_list` on an empty DB returns empty list.
- Test `tools/call` with unknown tool name returns correct error shape.
- Test invalid JSON input returns parse error.

## Acceptance criteria

- `calli mcp` starts and responds to `tools/list` (manual test via `echo '...' | calli mcp`)
- Claude Desktop can be pointed at `calli mcp` and list corpora (manual smoke test)
- All 12 tools return valid responses against an indexed Xenos corpus
- `cargo test --all` passes
- `cargo clippy --all -- -D warnings` passes
