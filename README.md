# Callimachus

> A queryable index over arbitrary corpora — books, codebases, wikis — exposed to LLMs as a structured tool surface.

Named for the scholar at the Library of Alexandria who built the *Pinakes*, the first structured catalog of a library's holdings. His task was making the contents of a library *findable*. That is this system's task too.

---

## What it does

LLMs reason powerfully over text but are constrained by context windows. Callimachus solves this by ingesting any corpus, building a multi-level index at index time (entities, relationships, summaries, purposes, contracts), and exposing a uniform tool surface at query time. The LLM retrieves pre-built understanding rather than re-comprehending raw source.

The same agent, with the same query patterns, can interrogate a novel or a million-line codebase:

| Corpus | Example queries |
|---|---|
| **Books** | Who is Bequin? Where does Eisenhorn first meet her? How does her role change across the trilogy? |
| **Code** | What does `IndexPipeline` do? What calls it? Which functions have panic risk? Is this codebase well-factored? |
| **Wikis** | What concepts are linked from the Authentication page? Summarise the deployment section. |

### How indexing works

Callimachus runs a pipeline of passes over your corpus:

```
History → Chunk → Structure → Semantic → Aliases → Summarize → Purpose → Contract → Themes
```

Each pass enriches the index with pre-built understanding:

- **History** — compare current source state against the last indexed version; produce a change manifest so downstream passes skip unchanged files (fast incremental updates)
- **Chunk** — split source into navigable units (chapters, functions, files)
- **Structure** — parser-driven entity and edge extraction (tree-sitter for code, section headings for books/wikis); no LLM
- **Semantic** — LLM-powered entity extraction with typed edges
- **Aliases** — resolve name variants to canonical entities
- **Summarize** — 1–3 sentence behavioral summary per entity, bottom-up (function → file → corpus)
- **Purpose** — architectural intent: *why* this entity exists
- **Contract** — static signals (is_public, is_fallible, has_panic_risk, …) merged with LLM-inferred assumptions, risks, and caller notes
- **Themes** — corpus-level architectural invariants (opt-in; code and wiki adapters)

At query time, responses are assembled from these pre-built pieces — zero new LLM comprehension required.

---

## Try it now

This repo ships with a pre-built index of **itself**. Clone and query immediately:

```bash
git clone https://github.com/thehammer/callimachus
cd callimachus
cargo build --release
export PATH="$PWD/target/release:$PATH"

# Query the bundled index
calli --pinakes data/callimachus.pinakes inspect entities callimachus
calli --pinakes data/callimachus.pinakes inspect runs callimachus
```

Or open the project in Claude Code — `.mcp.json` wires `calli mcp` as an MCP server automatically, giving Claude tools to query the entity graph, retrieve chunks, and call `explain_component`.

---

## Installation

```bash
cargo install --path crates/callimachus-cli
```

Or via Homebrew (macOS):

```bash
brew install thehammer/tap/callimachus
```

Or build from source:

```bash
git clone https://github.com/thehammer/callimachus
cd callimachus
cargo build --release
# binary at target/release/calli
```

---

## Quick start

### Index a codebase

```bash
calli corpus add code "My Project" /path/to/project --id my-project
calli index my-project
calli mcp  # start the MCP server
```

### Index a book

```bash
calli corpus add book "Xenos" /path/to/xenos.epub --id xenos
calli index xenos
```

### Index a wiki

```bash
calli corpus add wiki "Docs" /path/to/docs --id docs
calli index docs
```

---

## LLM provider setup

Callimachus uses an LLM to extract entities, summarize, and generate purposes and contracts. Two providers are supported — configure once, used for all index operations.

### Option A — Anthropic API key (recommended for large corpora)

Create `~/Library/Application Support/callimachus/config.toml` (macOS) or `~/.config/callimachus/config.toml` (Linux):

```toml
[llm]
api_key = "sk-ant-..."
model = "claude-haiku-4-5-20251001"   # optional, defaults to haiku
```

Callimachus reads this file at startup. The key is not read from the shell environment and is not visible to other tools that read `ANTHROPIC_API_KEY`.

### Option B — Claude Code (subscription, no API key needed)

If `claude` is on your PATH, Callimachus will use it automatically via subprocess. No configuration needed. Slower than the API (~10–20 chunks/min vs ~50–100/min) but covered by your Claude subscription.

Auto-detection priority: `--provider` flag → config file api_key → `claude` on PATH.

### Tiered model routing (optional)

For large corpora, Callimachus can route each entity to the cheapest model capable of handling its complexity — Haiku for simple entities, Sonnet for moderately complex ones, Opus for high-risk or highly-connected components. Disabled by default; enable in `config.toml`:

```toml
[model_tiers]
enabled = true
# Thresholds are tunable; these are the defaults
opus_in_degree_threshold = 10
sonnet_body_lines_threshold = 80
```

When enabled, each indexing pass (Purpose, Contract, Summarize) routes per-entity and logs the tier distribution. Multi-model artifact storage means running the pipeline again with a different model adds new rows rather than overwriting existing ones — the highest-tier artifact wins at query time.

---

## Claude Code integration

### Automatic (project-level)

`.mcp.json` is included in this repo. When you open this project in Claude Code, the `callimachus` MCP server starts automatically and points at `data/callimachus.pinakes`.

For your own project, after indexing, add an `.mcp.json`:

```json
{
  "mcpServers": {
    "callimachus": {
      "command": "calli",
      "args": ["--pinakes", "/path/to/your/index.pinakes", "mcp"]
    }
  }
}
```

### Claude Desktop

```json
{
  "mcpServers": {
    "callimachus": {
      "type": "stdio",
      "command": "calli",
      "args": ["--pinakes", "/path/to/index.pinakes", "mcp"]
    }
  }
}
```

### Available MCP tools

Once connected, Claude has access to 27 tools. Key tools:

| Tool | Description |
|---|---|
| `entity` | Look up a named entity by name or ID |
| `entity_contracts` | Full contract (risks, assumptions, static signals) and purpose for an entity |
| `entity_search_by_abstract_kind` | Find entities by abstract kind across corpora (e.g. `process`, `person`) |
| `list_abstract_kinds` | List all abstract kinds registered in the taxonomy |
| `read` | Retrieve raw source for a location URI |
| `entity_edges` | Graph edges in/out of an entity |
| `explain_component` | BFS call graph assembled into a diegesis — zero new LLM calls |
| `search` | Full-text search across all chunks |
| `corpus_list`, `corpus_overview`, `corpus_themes` | Corpus metadata and architectural themes |
| `list_scholia`, `apply_scholion` | List and apply non-destructive index corrections |
| `find_unreachable`, `entities_without_tests`, `find_inconsistencies` | Code health queries |
| Collection tools (`collection_list`, `collection_search`, `collection_entity_resolve`, …) | Cross-corpus entity queries |

---

## CLI reference

```
# Corpus management
calli corpus add <kind> <name> <source> [--id <id>]
calli corpus list
calli corpus show <id>
calli corpus remove <id>

# Indexing
calli index <id>                    Run all default passes
calli index <id> --pass chunk       Run a single pass
calli index <id> --full             Force full reindex (bypass skip guards)
calli reindex <id>                  Incremental reindex since last run
calli watch <id>                    Watch source and reindex on changes

# Pipeline versioning
calli status                        Show pipeline version status for all corpora
calli upgrade [<id>]                Upgrade corpus artifacts to current pipeline version

# Inspection
calli inspect entities <id>         Browse entities
calli inspect chunk <location>      Show a chunk by URI
calli inspect runs <id>             Indexing run history
calli inspect diegesis <id> <name>  BFS narrative for an entity (zero LLM calls)
calli inspect scholia <id>          Applied scholia (corrections)

# Scholia (non-destructive corrections)
calli scholion list <id>            List all scholia for a corpus
calli scholion apply <id> merge <a> <b>     Merge two entities
calli scholion apply <id> rename <id> <n>   Rename an entity
calli correct …                     (deprecated alias for calli scholion apply)

# Cross-corpus
calli link candidates <a> <b>       Suggest entity links between two corpora
calli collection add <name>
calli collection add-member <coll> <corpus>

# Pinakes file
calli pinakes info [<path>]         Show schema version, corpus count, file size

# Servers
calli mcp                           Start MCP stdio server
calli serve                         Start HTTP API server

# Export
calli export <id>                   Export index as JSONL

# Config
calli config show
calli config path
```

The index file uses the `.pinakes` extension (`--pinakes` flag or `CALLIMACHUS_PINAKES` env var). `--db` is accepted as a deprecated alias.

---

## Architecture

```
callimachus/
  crates/
    callimachus-core/           Domain types, SQLite storage, indexing pipeline
    callimachus-llm/            LLM provider abstraction (Anthropic API, Claude Code)
    callimachus-mcp/            MCP stdio server — 27 tools
    callimachus-http/           HTTP API server
    callimachus-cli/            calli binary
    adapters/
      callimachus-adapter-book/ EPUB + plain text
      callimachus-adapter-code/ Rust, Python, JS/TS, Go, PHP, Vue (tree-sitter)
      callimachus-adapter-wiki/ Markdown wikis
  data/
    callimachus.pinakes          Pre-built index of this codebase
  docs/
    architecture.md             Full architecture notes
    mcp-tools.md                MCP tool reference
    codebase-analysis.md        Self-analysis — Callimachus indexing itself
```

The index is a single SQLite file. Key tables:

| Table | Contents |
|---|---|
| `chunks` | Raw source units with full-text search index |
| `entities` | Named things (functions, characters, concepts) with aliases and `abstract_kind` |
| `edges` | Typed relationships between entities |
| `summaries` | Per-chunk behavioral summaries — composite PK `(target_id, model)` |
| `entity_purposes` | Per-entity architectural intent — composite PK `(entity_id, model)` |
| `entity_contracts` | Per-entity assumptions, risks, caller notes — composite PK `(entity_id, model)` |
| `entity_blocks` | Sub-function block-level descriptions |
| `kind_taxonomy` | Maps concrete entity kinds to abstract kinds for cross-corpus queries |
| `scholia` | Non-destructive corrections applied at query time (entity merges, renames, overrides) |
| `themes` | Corpus-level architectural invariants (opt-in) |

---

## How it relates to LLMs

Callimachus is designed around a specific insight about how LLMs work: they acquire general knowledge at training time but have no knowledge of your specific corpus. Standard retrieval (RAG) bridges this by stuffing raw text into the context window at query time — asking the model to re-comprehend the source on every query. Callimachus bridges it differently: by running the comprehension once at index time and storing the results as structured, queryable artifacts.

The relationship is complementary. The LLM brings general reasoning. Callimachus brings specific knowledge, pre-built. Neither re-reads the source at query time.

**[How Callimachus Complements LLMs](docs/llm-complement.md)** — a longer explainer covering the training/index-time parallel, the partial application intuition, and why this architecture makes sense for large corpora.

---

## Design philosophy

Callimachus was built alongside a broader framework for thinking about skillful AI-assisted development — the idea that the same model produces leverage or liability depending on how it's wielded, and that good tooling should embody discipline structurally rather than relying on individual virtue alone.

Several of that framework's principles are directly expressed in Callimachus's architecture:

- **Right Mindfulness** — the index is a queryable ground truth about your corpus. When a model says something about your codebase or your novel, there's a structured artifact to verify against. Confabulation becomes detectable.
- **Right Action** — the scholia model is a structured review layer. You don't just accept the index; you apply corrections where it's wrong. Review is built into the workflow, not bolted on.
- **Right Effort** — pre-computed semantic artifacts redirect effort toward higher-leverage work. Comprehension runs once at index time; practitioners spend their attention on reasoning, not re-reading.

**[The AIghtfold Path](https://github.com/thehammer/aightfold-path)** — the underlying framework. A mental model for skillful AI-assisted software development, drawn from the Buddhist Eightfold Path. Callimachus is one concrete instantiation of what "tooling that embodies the discipline structurally" looks like.

---

## Language and concepts

Callimachus uses precise terminology borrowed from philosophy and linguistics — *corpus*, *epistemology*, *ontology*, *semantic* — with specific meanings in this context.

- **[Vocabulary Primer](docs/vocabulary-primer.md)** — A narrative introduction, stage by stage, from the problem being solved through full fluency. Start here if the terminology is new.
- **[Glossary](docs/glossary.md)** — Alphabetical reference for all terms. Each entry links back to the relevant primer stage. The two documents cross-reference each other throughout.

---

## Self-analysis

`docs/codebase-analysis.md` answers four structural quality questions about this
codebase using the index it produces of itself:

1. Which functions or files are outliers in size vs. their peers?
2. Is complexity concentrated at the leaves or distributed across the hierarchy?
3. Is the codebase well-factored, and where is the most significant structural debt?
4. If you were a new contributor, where would this codebase most frustrate you?

**[Query Patterns](docs/query-patterns.md)** documents how each question is answered — what MCP tools and queries surface the answer, and what the equivalent manual approach looks like without Callimachus.

---

## License

Callimachus is dual-licensed:

- **Open source** — [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0). Free for personal use, open-source projects, and internal use.
- **Commercial** — a separate license is available for proprietary products and hosted services. See [LICENSE-COMMERCIAL.md](LICENSE-COMMERCIAL.md) or contact hammer@carefeed.com.
