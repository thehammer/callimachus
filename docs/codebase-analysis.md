# Callimachus Codebase Analysis

A self-referential exercise: using Callimachus to answer questions about the
Callimachus codebase itself. Questions were devised before running the queries;
answers are drawn from the indexed entity graph, contract signals, and size
metrics in the index DB.

---

## Question Catalogue

The questions below fall into two groups: a general catalogue of things the
index is positioned to answer (not yet answered here), and four structural
quality questions that are answered in detail in the sections that follow.

### General catalogue

**Navigation**
- Where is `IndexPipeline` defined and what does it depend on?
- What calls `entity_upsert`? What are the callers' responsibilities?
- Find all places that handle the `Pass::Contract` variant.

**Understanding**
- Explain how the structure pass works end to end.
- What is the purpose of `StorageBackend` vs `Database` — why both abstractions?
- What does `WikiAdapter` do differently from `BookAdapter`?

**Contracts & risks**
- What are the known risks in the indexing pipeline?
- Which functions have panic risk or unsafe blocks?
- What assumptions does the semantic pass make about chunk state?

**Architecture**
- What are the key architectural invariants of this codebase?
- What is the dependency relationship between `callimachus-core` and the adapter crates?
- Which entities are only reachable from tests?

**Change impact**
- If I change the `Chunk` type, what else breaks?
- What are all the implementors of `SourceAdapter`?

---

## Structural Quality Questions

### Q1 — Outlier detection
*Which functions, modules, or files are statistical outliers in size or complexity
relative to their peers at the same level of the hierarchy?*

#### Function size distribution

| Bucket | Count | % of total |
|---|---|---|
| ≤ 10 lines | 115 | 33% |
| 11–30 lines | 135 | 39% |
| 31–60 lines | 51 | 15% |
| > 60 lines | 43 | 13% |

Mean: 26 lines. The distribution has a healthy core but a meaningful heavy tail:
43 functions exceed 60 lines, and the 10 largest all exceed 100 lines.

**Top outlier functions (> 100 lines):**

| Function | File | Lines |
|---|---|---|
| *(anonymous)* | `adapter-book/src/chunker.rs` | 118 |
| `run_to_writer` | `callimachus-cli/src/commands/export.rs` | 115 |
| `chunk_file` | `adapter-code/src/chunker.rs` | 114 |
| *(anonymous)* | `adapter-wiki/src/chunker.rs` | 108 |
| *(anonymous)* | `indexing/reindex_pass.rs` | 107 |
| *(anonymous)* | `adapter-wiki/src/chunker.rs` | 105 |
| `main` | `callimachus-cli/src/main.rs` | 105 |
| `run_scip_to_writer` | `callimachus-cli/src/commands/export.rs` | 105 |

The three chunkers (book, code, wiki) all have their core logic in a single large
function. These are the primary decomposition candidates.

#### File size outliers

| File | Lines |
|---|---|
| `query/service.rs` | 1,509 |
| `query/collection_service.rs` | 1,285 |
| `adapter-code/src/extractor.rs` | 751 |
| `callimachus-cli/src/main.rs` | 668 |
| `adapter-code/src/adapter.rs` | 587 |

`query/service.rs` at 1,509 lines is the single most significant outlier — more
than 3× the size of the next-largest non-query file. `collection_service.rs` at
1,285 lines is similarly oversized. Together these two files hold the bulk of the
query layer complexity.

#### Functions per file (hierarchy balance)

| File | Function count |
|---|---|
| `adapter-code/tests/contracts.rs` | 17 |
| `adapter-code/tests/code_adapter.rs` | 14 |
| `storage/chunk_store.rs` | 14 |
| `callimachus-http/src/handlers.rs` | 13 |
| `adapter-book/tests/book_adapter.rs` | 11 |
| `adapter-code/src/chunker.rs` | 11 |
| `adapter-code/src/extractor.rs` | 11 |

The test files appropriately concentrate many small test functions. Among
production code, `handlers.rs` (13), `chunker.rs` (11), and `extractor.rs` (11)
are the densest — all at reasonable levels. No extreme outliers here; function
count per file is well-distributed.

---

### Q2 — Hierarchy balance
*Is complexity concentrated at the leaves (deep, large functions) or distributed
across the hierarchy (many small modules, each with focused responsibilities)?*

#### Crate-level distribution

| Crate | Files | Total lines |
|---|---|---|
| `callimachus-core` | 64 | 12,186 |
| `adapter-code` | 16 | 4,012 |
| `callimachus-cli` | 14 | 3,151 |
| `adapter-wiki` | 8 | 1,756 |
| `callimachus-llm` | 10 | 1,352 |
| `adapter-book` | 7 | 1,045 |
| `callimachus-mcp` | 4 | 960 |
| `callimachus-http` | 4 | 470 |

`callimachus-core` is ~3× the size of the next-largest crate, which is expected:
it is the intentional home for storage, indexing, query, types, and corrections.
But the internal structure of `core` warrants scrutiny — the query subsystem alone
accounts for ~2,800 lines across two files (`service.rs` + `collection_service.rs`),
which is 23% of the entire core crate in two files.

#### Assessment

Complexity is concentrated at two specific nodes, not broadly distributed:

1. **`query/service.rs` (1,509 lines)** — a single class (`QueryService`) that
   appears to be the primary entry point for all query operations. Its purpose
   statement ("provides a unified query interface…") confirms it is doing
   everything intentionally, but the scale suggests it has accumulated
   responsibilities that could be separated (entity queries, corpus queries,
   search, graph traversal).

2. **`query/collection_service.rs` (1,285 lines)** — `CollectionService` and its
   in-memory `EntityLinkIndex`. The intent gap flagged in contracts: the
   `EntityLinkIndex` claims transitive closure but may only implement direct
   lookups, suggesting incomplete implementation driving size.

Elsewhere the hierarchy is reasonably balanced. The adapter crates each have a
clear chunker/extractor/adapter structure with focused files. The storage layer
in `core` has appropriately thin per-entity store files (chunk_store, edge_store,
entity_store each with 7–14 focused functions).

---

### Q3 — Overall verdict
*Is the callimachus codebase well-factored, and where is the most significant
structural debt?*

**Grade: B. Well-factored in the majority, with one concentrated problem.**

The storage layer, adapter layer, and LLM layer are genuinely well-factored.
The storage crate has thin, focused store files (one per entity type). Adapters
follow a consistent chunker/extractor/adapter pattern. The LLM abstraction is
clean with clear provider implementations.

The significant structural debt is localised to the **query layer**:

- `query/service.rs` at 1,509 lines is a god object. `QueryService` handles
  entity retrieval, corpus metadata, full-text search, graph traversal, and
  semantic search. These are separable concerns.
- `query/collection_service.rs` at 1,285 lines compounds this. The LLM-generated
  contract flagged a real gap: "the linked() method appears to return only direct
  links, not transitive chains" — suggesting the complexity isn't just size but
  unresolved design.

Secondary debt, also flagged by the contract pass:

- The three **chunker functions** (book, code, wiki) each encode their chunking
  logic in a single 100–118 line function. The pattern is consistent across
  adapters (probably intentional) but each is a prime candidate for extraction
  into sub-functions.
- `callimachus-cli/src/main.rs` at 668 lines doubles as a CLI definition and
  command wiring file. Typical Rust CLI practice would move subcommand
  implementations out of main.
- `CorrectionsEngine` has been flagged as incomplete — only Merge is implemented,
  Rename and alias correction are missing.
- `IndexPipeline` has an incomplete match statement that will panic on certain
  `Pass` variants.

---

### Q4 — New contributor frustration
*If you were a new contributor, where would this codebase most frustrate you?*

**The query layer is a labyrinth with no map.**

A new contributor trying to understand how a query actually executes would open
`query/service.rs` and face 1,509 lines with no obvious entry point hierarchy.
`QueryService`'s purpose statement is accurate but unhelpful at that scale —
"a unified query interface" describes the intent but not the shape.

Specific friction points the index surfaced:

1. **`StorageBackend` vs `Database` — why both?** The contract for `StorageBackend`
   promises pluggable backends (Postgres, Memory) but the implementation shows
   tight SQLite coupling. A new contributor would wonder whether the abstraction
   is real or aspirational, and would be unsure which interface to use when
   adding a new storage operation.

2. **`main.rs` is doing too much.** At 668 lines, the CLI entrypoint is where a
   new contributor would start, and it wires together subcommands in a way that
   makes the overall command surface hard to scan. Compare to the clean per-command
   files in `src/commands/` — that pattern is good, but `main.rs` hasn't kept up
   with it.

3. **Silent contract violations in the index.** The contract pass flagged several
   real issues that aren't surfaced as compile errors or tests: `delete()` marked
   `is_mutating=false` while performing a DELETE, `collection.run()` marked
   `is_mutating=false` while calling `db.collection_insert()`. These won't break
   anything today but signal that the contract metadata is unreliable, which erodes
   trust in the index itself when used as a tool.

4. **Incomplete implementations with no TODO markers.** `CorrectionsEngine` only
   implements Merge. `IndexPipeline` has an incomplete match. These are real
   gaps that a new contributor might not discover until attempting to use those
   paths, and there are no `todo!()` macro calls or tracking issues to signal the
   incompleteness.

**The good news for a new contributor:** everything outside the query layer is
approachable. The storage layer is a pleasure — thin, consistent, well-named
functions. The adapter pattern is obvious and well-executed. Adding a new adapter
or a new storage operation is genuinely straightforward.

---

*Generated using Callimachus self-index. Corpus: `callimachus` (733 chunks,
1,957 entities, 4,752 edges). Passes: chunk, structure, semantic, aliases,
summarize, purpose, contract.*
