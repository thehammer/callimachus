# Callimachus Architecture

## 1. Overview

```
 ┌──────────────────────────────────────────────────────────────┐
 │                         calli CLI                            │
 │  corpus add/list │ index │ reindex │ watch │ mcp │ serve     │
 └──────┬───────────┴───────┴─────────┴───────┴──┬──┴────┬──────┘
        │                                         │       │
        ▼                                         ▼       ▼
 ┌──────────────┐                    ┌──────────────┐  ┌──────────────┐
 │  Index       │                    │  MCP server  │  │  HTTP server │
 │  Pipeline    │                    │  (stdio)     │  │  (REST)      │
 └──────┬───────┘                    └──────┬───────┘  └──────┬───────┘
        │                                   │                 │
        ▼                                   └────────┬────────┘
 ┌──────────────────────┐                            ▼
 │  Source Adapters     │               ┌────────────────────────┐
 │  book │ code │ wiki  │               │     QueryService       │
 └──────┬───────────────┘               │  12 tools over SQLite  │
        │                               └────────────┬───────────┘
        ▼                                            │
 ┌──────────────────────────────────────────────────▼───────────┐
 │                     SQLite Database                           │
 │  corpora │ chunks │ entities │ edges │ summaries │ FTS5       │
 └───────────────────────────────────────────────────────────────┘
```

## 2. Crate Dependency Graph

```
callimachus-cli
  ├── callimachus-core       (domain types, storage, query, indexing)
  ├── callimachus-llm        (LLM provider abstraction)
  ├── callimachus-mcp        (JSON-RPC stdio server)
  ├── callimachus-http       (Axum REST server)
  ├── callimachus-adapter-book
  ├── callimachus-adapter-code
  └── callimachus-adapter-wiki  (stub)

callimachus-mcp
  └── callimachus-core

callimachus-http
  └── callimachus-core

callimachus-adapter-{book,code,wiki}
  └── callimachus-core
```

## 3. Storage Schema

All data lives in a single SQLite file (default: `~/.local/share/callimachus/index.pinakes`).

### Tables

| Table | Purpose |
|-------|---------|
| `corpora` | Corpus metadata (id, name, kind, source path, status) |
| `chunks` | Indexed content units (location URI, content, kind, corpus_id) |
| `entities` | Named entities (id, canonical_name, kind, corpus_id, appearances) |
| `entity_locations` | Many-to-many: entity ↔ location URIs |
| `edges` | Relationships between entities (from_id, to_id, kind, location_uri) |
| `summaries` | LLM-generated summaries (target_kind, target_id, text, model) |
| `corrections` | Manual correction overrides (kind, payload JSON) |
| `index_runs` | Indexing run history (corpus_id, pass, status, started_at) |

### FTS5 Index

```sql
CREATE VIRTUAL TABLE chunks_fts USING fts5(
    location_uri UNINDEXED,
    content,
    content='chunks',
    content_rowid='rowid'
);
```

Full-text search uses the `bm25` ranking function. Snippet extraction provides context windows around matches.

## 4. Indexing Pipeline

The pipeline runs passes sequentially. All passes except `History` can be run individually with `--pass <name>`. The `--full` flag bypasses "skip if already processed" guards in every pass. The `--dry-run` flag counts without writing.

### Passes (in execution order)

| # | Pass | LLM | Description |
|---|------|-----|-------------|
| 0 | **history** | No | Compare current source state against the `last_indexed_version` anchor in the DB. Produces a `ChangeManifest` — a set of dirty paths — that all downstream passes consult to skip unchanged files. On first run, all sources are dirty. The version anchor is written back only after the full pipeline completes. |
| 1 | **chunk** | No | Walk the source (git-tracked files for code; EPUB spine for books; Markdown files for wikis) and emit `Chunk` records with location URIs (`calli://corpus_id/path/to/file#section`). The code adapter applies default exclusion globs (`.git/**`, `.claude/**`, `vendor/**`, `node_modules/**`, etc.) on top of git filtering. |
| 2 | **structure** | No | Parser-driven entity and edge extraction — no LLM. For code corpora, runs tree-sitter over each chunk to extract named entities (functions, classes, methods, interfaces) and typed edges (calls, defines, imports, extends, implements, instantiates). For book/wiki corpora, extracts section hierarchy and cross-references. Skipped for chunks whose path is not dirty. |
| 3 | **semantic** | Yes | LLM-powered entity extraction and relationship identification over chunk content. Produces additional entities and edges beyond what the parser can see (e.g. thematic connections in books, inferred dependencies in code). Skipped for non-dirty chunks. |
| 4 | **aliases** | Yes | Alias resolution: given the full entity set, asks the LLM (or uses heuristics) to suggest entity merges — `Eisenhorn` and `Gregor Eisenhorn` are the same entity. Applies merges via the entity store. Skipped entirely when no dirty sources exist. |
| 5 | **summarize** | Yes | Hierarchical LLM summarization, bottom-up. For code: function-level first, then file-level; each file summary receives its function summaries as context. For books: scene → chapter → corpus. Finally, a corpus-level summary is always attempted. Skipped for non-dirty paths. |
| 6 | **purpose** | Yes | For each entity with a source location, asks the LLM: *why does this entity exist?* Stores an `entity_purpose` with a one-sentence purpose statement and optional block-level descriptions for complex functions. Skipped for non-dirty entities. |
| 7 | **contract** | Yes | Two-phase per entity. First, static analysis (tree-sitter / regex) populates boolean signals — `is_public`, `is_fallible`, `is_must_use`, `is_deprecated`, `has_panic_risk`, `has_unsafe`, etc. — without any LLM call. Then the LLM infers semantic fields: `assumptions`, `risks`, `intent_gap`, `caller_notes`, `verified_by_names`, `discards_result_callees`. If the LLM parse fails, the static signals are stored anyway. Skipped for non-dirty entities. |
| 8 | **themes** *(opt-in)* | Yes | Corpus-level architectural invariants. Asks the LLM to identify 3–7 cross-cutting patterns or invariants across a sample of entities. Skipped entirely when no dirty sources exist. Not in the default pass list — add `--pass theme` explicitly or enable via `IndexOptions`. Currently implemented for code and wiki adapters. |

### Resumability

Re-running `calli index <corpus_id>` is safe. Each pass skips entities or chunks it has already processed (unless `--full` is set). The `--from-chunk <id>` flag forces the chunk pass to start from a specific chunk ID, useful after an interrupted run.

### Change detection

`Pass::History` runs before all other passes and produces a `ChangeManifest`. For git-backed corpora (code adapter) the version anchor is a full commit OID; for others it is a SHA-256 hash-of-hashes over all chunk content. All downstream passes call `manifest.is_dirty(path)` before processing any source. A second consecutive run with no source changes produces zero processed items in every pass.

## 5. Query Service

`QueryService` is the single entry point for all read operations. It holds a shared `Arc<Mutex<Database>>` and an optional `CorrectionsEngine` overlay.

All methods take `&self` and return `ToolResult<T>` — either `ToolSuccess<T>` (with data + metadata) or `ToolResultError` (with a typed error kind).

### 12 Tools

See [mcp-tools.md](mcp-tools.md) for complete input/output schemas.

| Tool | Description |
|------|-------------|
| `corpus_list` | List all indexed corpora |
| `corpus_overview` | Metadata + top entities for one corpus |
| `search` | Full-text / hybrid search over chunks |
| `entity` | Look up a named entity by name or ID |
| `entity_edges` | Get relationships for an entity |
| `entity_meet` | Find co-occurrence locations for two entities |
| `read` | Read a chunk at a location URI |
| `summarize` | Get or generate a summary |
| `related` | Find chunks related to a location |
| `chapter_summary` | Composite: chapter overview + entities |
| `character_profile` | Composite: entity + edges + summary |
| `find_scene` | Composite: find scene where two entities meet |

### Corrections Overlay

`CorrectionsEngine` applies in-memory corrections on top of raw storage:
- **merge**: Redirect entity_id lookups to a canonical entity
- **rename**: Override `canonical_name` for display
- **alias**: Add additional lookup aliases
- **edit_summary**: Replace LLM-generated summaries with manual text

Corrections are stored in the `corrections` table and loaded at query time.

## 6. MCP Transport

The MCP server (`calli mcp`) implements the [Model Context Protocol](https://modelcontextprotocol.io/) over stdin/stdout using JSON-RPC 2.0.

### Protocol Flow

```
client → {"jsonrpc":"2.0","method":"initialize","params":{...},"id":1}
server → {"jsonrpc":"2.0","result":{"capabilities":{"tools":{}}},"id":1}

client → {"jsonrpc":"2.0","method":"tools/list","id":2}
server → {"jsonrpc":"2.0","result":{"tools":[...12 tools...]},"id":2}

client → {"jsonrpc":"2.0","method":"tools/call","params":{"name":"search","arguments":{...}},"id":3}
server → {"jsonrpc":"2.0","result":{"content":[{"type":"text","text":"..."}]},"id":3}
```

### Tool Registration

Each of the 12 QueryService tools is registered with a JSON Schema input definition. The dispatcher (`dispatch.rs`) routes `tools/call` requests by name and serializes results as MCP `TextContent`.

## 7. HTTP API

The HTTP server (`calli serve`) mirrors the MCP tool surface as a REST API using [Axum](https://github.com/tokio-rs/axum).

### Default Binding

`127.0.0.1:7700` — local only. **Do not expose to untrusted networks** without adding authentication. The CORS policy allows `*` origins because the server is local-only by default.

### Routes

| Method | Path | Tool |
|--------|------|------|
| `GET` | `/corpora` | `corpus_list` |
| `GET` | `/corpora/:id` | `corpus_overview` |
| `POST` | `/corpora/:id/search` | `search` |
| `GET` | `/corpora/:id/entity/:name` | `entity` |
| `GET` | `/corpora/:id/entity/:id/edges` | `entity_edges` |
| `POST` | `/corpora/:id/meet` | `entity_meet` |
| `GET` | `/corpora/:id/read?location=...` | `read` |
| `GET` | `/corpora/:id/summary` | `summarize` |
| `GET` | `/corpora/:id/related?location=...` | `related` |
| `GET` | `/corpora/:id/chapter/:ch` | `chapter_summary` |
| `GET` | `/corpora/:id/character/:name` | `character_profile` |
| `POST` | `/corpora/:id/scene` | `find_scene` |
| `GET` | `/health` | — |

### Error Model

All errors return JSON: `{"error": "message"}`.

| ToolError kind | HTTP status |
|----------------|-------------|
| `not_found` | 404 |
| `ambiguous` | 422 |
| `invalid_input` | 400 |
| `error` | 500 |

Success responses wrap the tool output in the `ToolSuccess` envelope:
```json
{"ok": true, "data": {...}, "scope_applied": {...}, "indexed_at": "..."}
```

## 8. Provider Selection

LLM providers are resolved in this priority order:

1. `--provider` CLI flag
2. `CALLIMACHUS_LLM_PROVIDER` environment variable
3. `callimachus.llm.provider` in `~/.config/callimachus/config.toml`
4. Auto-detect: Claude Code (via `~/.claude/` presence) → Anthropic API (`ANTHROPIC_API_KEY`) → dry-run

Supported providers: `claude-code`, `anthropic`, `dry-run`.

## 9. Stage 0 — History-Aware Change Detection

Before any indexing pass runs, `Pass::History` (Stage 0) compares the corpus's current source state against the `last_indexed_version` anchor stored in the `corpora` table.  It produces a `ChangeManifest` that all downstream passes consult to decide which sources (and therefore which chunks and entities) need processing.

### Version Anchoring

Each corpus tracks a `last_indexed_version` string.  For git-backed corpora (code adapter) this is a full commit OID prefixed `git:<sha>`.  For non-git corpora (book, wiki) it is a SHA-256 hash of the concatenated chunk hashes — a "hash-of-hashes".

The version is written back to the DB **only after** all passes in the pipeline complete successfully.  A partial failure leaves the anchor unchanged, so the next run replays from the last known-good baseline.

### ChangeManifest

`ChangeManifest` carries:
- `current_version` — the just-computed version string
- `all_dirty` — true on first run, `--full`, or when History pass is skipped
- `dirty_paths` — set of relative source paths that changed since the anchor
- `commit_metadata` — optional per-path `CommitMeta` (message, author) for git corpora

`ChangeManifest::is_dirty(path)` is the single predicate all passes use.  For chunk-level filtering, `is_dirty_for_chunk(chunk)` extracts the path from the `calli://` URI.

### Pass Behavior Under a Manifest

| Pass | Behavior |
|------|----------|
| Chunk | Sources not in `dirty_paths` are skipped entirely |
| Structure / Semantic / Summarize / Embed | Chunks whose URI path is not dirty are skipped |
| Purpose / Contract | Entities whose location URI path is not dirty are skipped |
| Aliases / Theme | Skipped entirely when manifest is non-dirty and no dirty sources exist |

The net effect: a second consecutive `calli index` run with no source changes produces **zero processed** items in every downstream pass.
