# Callimachus MCP Tools Reference

25 of the 27 tools are available on both the MCP stdio server (`calli mcp`) and the HTTP REST API
(`calli serve`). The two exceptions are `list_scholia` and `apply_scholion` — scholia are write
operations and remain MCP-only to keep mutating paths off the web surface.

Every tool returns a `ToolResult` envelope:

```json
// Success
{
  "ok": true,
  "data": { ... },
  "scope_applied": { "corpus_ids": null, "location_prefix": null },
  "indexed_at": "2025-01-01T00:00:00Z",
  "cost_metadata": { "cached": true, "tokens_used": null }
}

// Error
{
  "ok": false,
  "kind": "not_found",        // or "ambiguous" | "invalid_input" | "error"
  "suggestions": ["..."]      // present for not_found
}
```

---

## Corpus tools

### 1. `corpus_list`

List all indexed corpora.

#### Input
```json
{}
```

#### Output (`data` field)
```json
[
  {
    "id": "xenos",
    "name": "Xenos (Dan Abnett)",
    "kind": "book",
    "last_indexed": "2025-01-15T10:00:00Z",
    "chunk_count": 842,
    "entity_count": 134
  }
]
```

#### HTTP
```
GET /corpora
```

---

### 2. `corpus_overview`

Full metadata and top entities for one corpus.

#### Input
```json
{ "corpus_id": "xenos" }
```

#### Output (`data` field)
```json
{
  "id": "xenos",
  "name": "Xenos",
  "kind": "book",
  "status": "indexed",
  "chunk_count": 842,
  "entity_count": 134,
  "last_indexed": "2025-01-15T10:00:00Z",
  "top_entities": [ { "id": "...", "canonical_name": "Eisenhorn", ... } ],
  "summary": "Inquisitor Eisenhorn investigates a conspiracy..."
}
```

#### HTTP
```
GET /corpora/:id
```

#### Errors
- `not_found` (404) if corpus does not exist

---

### 3. `corpus_themes`

List corpus-level architectural themes and invariants extracted during the `themes` indexing pass.

#### Input
```json
{
  "corpus_id": "my-project",
  "include_edges": false    // optional; also return upheld_by / violated_by edge sets
}
```

#### Output (`data` field)
```json
{
  "themes": [
    {
      "id": "theme-uuid",
      "corpus_id": "my-project",
      "name": "Error propagation via Result",
      "description": "All fallible functions return Result<T, E>..."
    }
  ],
  "upheld_by": [],   // only present when include_edges=true
  "violated_by": []  // only present when include_edges=true
}
```

#### HTTP
```
GET /corpora/:id/themes
GET /corpora/:id/themes?include_edges=true
```

#### Notes

Themes are produced by default as part of the standard nine-pass run. They can also be run standalone via `calli index <id> --pass theme`. Currently implemented for code and wiki adapters.

---

## Search & retrieval

### 4. `search`

Full-text, semantic, or hybrid search over chunks.

#### Input
```json
{
  "corpus_id": "xenos",
  "query": "Bequin",
  "mode": "keyword",      // "keyword" | "semantic" | "hybrid" (default: keyword)
  "limit": 20,            // optional, default 20
  "scope": null           // optional scope filter
}
```

#### Output (`data` field)
```json
{
  "results": [
    {
      "location": { "corpus_id": "xenos", "path": "ch/3", "uri": "calli://xenos/ch/3" },
      "snippet": "...Alizebeth Bequin stood in the doorway...",
      "relevance": 0.87,
      "kind": "section"
    }
  ],
  "total": 3
}
```

#### HTTP
```
POST /corpora/:id/search
Content-Type: application/json

{ "query": "Bequin", "mode": "keyword", "limit": 20 }
```

Note: `corpus_id` in the request body is overridden by the `:id` path parameter.

#### Errors
- `invalid_input` (400) if `query` is missing

---

### 5. `read`

Read the content of a chunk at a location URI.

#### Input
```json
{
  "corpus_id": "xenos",        // optional — inferred from location URI if omitted
  "location": "calli://xenos/ch/3",
  "depth": "full"              // "summary" | "scenes" | "full" (default: full)
}
```

#### Output (`data` field)
```json
{
  "location": { "corpus_id": "xenos", "path": "ch/3", "uri": "calli://xenos/ch/3" },
  "kind": "chapter",
  "summary": "Eisenhorn meets Bequin for the first time...",
  "content": "Full text of the chunk...",
  "entities_present": [ { "id": "...", "canonical_name": "Bequin", ... } ],
  "child_locations": [ { "uri": "calli://xenos/ch/3/scene/1", ... } ]
}
```

#### HTTP
```
GET /corpora/:id/read?location=calli://xenos/ch/3&depth=full
```

#### Errors
- `not_found` (404) if no chunk exists at the location
- `invalid_input` (400) if `location` query param is missing

---

### 6. `related`

Find chunks related to a location by entity co-occurrence.

#### Input
```json
{
  "corpus_id": "xenos",
  "location": "calli://xenos/ch/3",
  "limit": 10    // optional, default 10
}
```

#### Output (`data` field)
```json
{
  "items": [
    {
      "location": { "corpus_id": "xenos", "path": "ch/7", "uri": "..." },
      "relationship": "shares_entities",
      "score": 0.72
    }
  ]
}
```

#### HTTP
```
GET /corpora/:id/related?location=calli://xenos/ch/3&limit=10
```

#### Errors
- `invalid_input` (400) if `location` query param is missing

---

### 7. `summarize`

Get or generate a summary for a corpus, entity, location, or range.

#### Input
```json
{
  "corpus_id": "xenos",
  "target": { "kind": "corpus" }
}
// or entity:
{ "corpus_id": "xenos", "target": { "kind": "entity", "entity_id": "entity-uuid" } }
// or location:
{ "corpus_id": "xenos", "target": { "kind": "location", "location": "calli://xenos/ch/3" } }
// or range:
{ "corpus_id": "xenos", "target": { "kind": "range", "from": "calli://xenos/ch/3", "to": "calli://xenos/ch/5" } }
```

#### Output (`data` field)
```json
{
  "text": "Summary text...",
  "generated_at": "2025-01-15T10:00:00Z",
  "model": "claude-3-5-haiku-20241022"
}
```

#### HTTP
```
GET /corpora/:id/summary
GET /corpora/:id/summary?entity_id=entity-uuid
GET /corpora/:id/summary?location=calli://xenos/ch/3
GET /corpora/:id/summary?from=calli://xenos/ch/3&to=calli://xenos/ch/5
```

#### Errors
- `not_found` (404) if no summary exists for the target

---

## Entity tools

### 8. `entity`

Look up a named entity by name or ID.

#### Input
```json
{ "corpus_id": "xenos", "name_or_id": "Eisenhorn" }
```

#### Output (`data` field)
```json
{
  "id": "entity-uuid",
  "corpus_id": "xenos",
  "canonical_name": "Eisenhorn",
  "kind": "character",
  "abstract_kind": "person",
  "aliases": ["Gregor Eisenhorn", "Inquisitor Eisenhorn"],
  "appearances": 187,
  "confidence": 0.95
}
```

#### HTTP
```
GET /corpora/:id/entity/:name
```

#### Errors
- `not_found` (404) with fuzzy name suggestions
- `ambiguous` (422) if multiple entities match the name

---

### 9. `entity_edges`

Get relationships for an entity.

#### Input
```json
{
  "corpus_id": "xenos",
  "entity_id": "entity-uuid",
  "direction": "both",      // "inbound" | "outbound" | "both"
  "kind": null,             // optional edge kind filter
  "limit": 50               // optional, default 50
}
```

#### Output (`data` field)
```json
{
  "entity_id": "entity-uuid",
  "edges": [
    {
      "id": "edge-uuid",
      "from_entity_id": "entity-uuid",
      "to_entity_id": "other-uuid",
      "kind": "ally",
      "location_uri": "calli://xenos/ch/3",
      "weight": 1.0
    }
  ],
  "count": 12
}
```

#### HTTP
```
GET /corpora/:id/entity/:id/edges?direction=both&kind=ally&limit=20
```

---

### 10. `entity_meet`

Find locations where two entities co-occur.

#### Input
```json
{
  "corpus_id": "xenos",
  "entity_a": "Eisenhorn",
  "entity_b": "Bequin"
}
```

#### Output (`data` field)
```json
{
  "first_co_occurrence": { "corpus_id": "xenos", "path": "ch/3", "uri": "calli://xenos/ch/3" },
  "all": [ { "corpus_id": "xenos", "path": "ch/3", "uri": "..." }, ... ],
  "count": 7
}
```

#### HTTP
```
POST /corpora/:id/meet
Content-Type: application/json

{ "entity_a": "Eisenhorn", "entity_b": "Bequin" }
```

#### Errors
- `not_found` (404) if either entity is not found or they never co-occur

---

### 11. `entity_contracts`

Retrieve the pre-indexed contract (risks, assumptions, static signals) and purpose for a specific code entity. Useful for understanding what a function promises, what it assumes, and what static analysis has detected.

#### Input
```json
{
  "corpus_id": "my-project",
  "entity_id": "entity-uuid"
}
```

#### Output (`data` field)
```json
{
  "entity_id": "entity-uuid",
  "contract": {
    "entity_id": "entity-uuid",
    "is_public": true,
    "is_fallible": true,
    "is_must_use": false,
    "is_deprecated": false,
    "has_panic_risk": false,
    "has_unsafe": false,
    "panic_call_count": 0,
    "assumptions": "Input slice must be non-empty...",
    "risks": "Returns Err if the underlying store is unavailable.",
    "caller_notes": "Always check the returned Result.",
    "intent_gap": null
  },
  "purpose": {
    "entity_id": "entity-uuid",
    "statement": "Validates incoming requests before they reach the handler layer.",
    "block_descriptions": []
  },
  "verified_by": [
    { "from_entity_id": "test-uuid", "kind": "verified_by", ... }
  ]
}
```

#### HTTP
```
GET /corpora/:id/entity/:entity/contracts
```

#### Errors
- `not_found` (404) if the entity does not exist

---

### 12. `entity_search_by_abstract_kind`

Find entities across one or more corpora filtered by their abstract taxonomy kind (e.g. `process`, `person`, `component`). Enables cross-corpus queries like "all processes in this codebase and its documentation".

#### Input
```json
{
  "corpus_ids": ["my-project", "my-docs"],
  "abstract_kind": "process",    // process | component | person | place | organization | concept
  "limit": 50                    // optional, default 50
}
```

#### Output (`data` field)
```json
{
  "entities": [
    {
      "id": "entity-uuid",
      "corpus_id": "my-project",
      "canonical_name": "IndexPipeline",
      "kind": "struct",
      "abstract_kind": "process",
      ...
    }
  ],
  "count": 14
}
```

#### HTTP
```
POST /search/by-abstract-kind
Content-Type: application/json

{ "corpus_ids": ["my-project", "my-docs"], "abstract_kind": "process", "limit": 50 }
```

---

### 13. `list_abstract_kinds`

Return the full kind taxonomy table: each row maps a concrete adapter kind (e.g. `function`, `character`, `workflow`) to its cross-corpus abstract kind (e.g. `process`, `person`). Use this to discover what abstract kinds are available before calling `entity_search_by_abstract_kind`.

#### Input
```json
{}
```

#### Output (`data` field)
```json
{
  "rows": [
    { "concrete_kind": "function", "corpus_kind": "code", "abstract_kind": "process" },
    { "concrete_kind": "character", "corpus_kind": "book", "abstract_kind": "person" },
    { "concrete_kind": "workflow", "corpus_kind": "wiki", "abstract_kind": "process" }
  ],
  "count": 24
}
```

#### HTTP
```
GET /abstract-kinds
```

---

## Code analysis tools

These tools are designed for code corpora and depend on the `contract` and `purpose` indexing passes having run.

### 14. `explain_component`

Assemble a **diegesis** — a multi-paragraph narrative explaining a component — via BFS over `calls` edges, drawing on pre-indexed purposes, summaries, and block descriptions. **Zero LLM calls at query time.**

#### Input
```json
{
  "corpus_id": "my-project",
  "entity_id": "entity-uuid",      // seed entity (use this OR module_prefix)
  "module_prefix": "IndexPipeline", // name prefix to seed the BFS (use this OR entity_id)
  "max_depth": 3                    // BFS depth limit, default 3
}
```

#### Output (`data` field)

A `DiegesisParagraph` for each BFS node — entity name, purpose statement, summary, and block-level descriptions — assembled into a prose narrative.

#### HTTP
```
POST /corpora/:id/explain
Content-Type: application/json

{ "entity_id": "entity-uuid" }
// or
{ "module_prefix": "IndexPipeline", "max_depth": 3 }
```

Note: `corpus_id` is taken from the `:id` path parameter and need not be in the body.

#### Errors
- `not_found` (404) if no seed entity is found
- `invalid_input` (400) if neither `entity_id` nor `module_prefix` is provided

#### CLI equivalent
```
calli inspect diegesis <corpus_id> <entity_name>
```

---

### 15. `find_unreachable`

Find code entities that have no inbound `calls` edges — potentially dead or unreachable code.

#### Input
```json
{ "corpus_id": "my-project" }
```

#### Output (`data` field)
```json
{
  "entities": [
    { "id": "entity-uuid", "canonical_name": "legacy_migrate_v1", "kind": "function", ... }
  ],
  "count": 3
}
```

#### HTTP
```
GET /corpora/:id/unreachable
```

---

### 16. `entities_without_tests`

Find code entities that have no inbound `verified_by` edges, indicating no test coverage as recorded in the index.

#### Input
```json
{ "corpus_id": "my-project" }
```

#### Output (`data` field)
```json
{
  "entities": [
    { "id": "entity-uuid", "canonical_name": "parse_config", "kind": "function", ... }
  ],
  "count": 7
}
```

#### HTTP
```
GET /corpora/:id/untested
```

---

### 17. `find_inconsistencies`

Find code entities whose contracts signal inconsistency or technical debt: incomplete implementations (`is_incomplete`), non-empty risks on infallible functions, or a non-null `intent_gap`.

#### Input
```json
{ "corpus_id": "my-project" }
```

#### Output (`data` field)
```json
{
  "contracts": [
    {
      "entity_id": "entity-uuid",
      "is_incomplete": true,
      "intent_gap": "Function is documented as pure but calls a mutating helper.",
      ...
    }
  ],
  "count": 2
}
```

#### HTTP
```
GET /corpora/:id/inconsistencies
```

---

## Composite tools (book / narrative)

### 18. `chapter_summary`

High-level summary for a chapter: overview + entities present + child scenes.

#### Input
```json
{ "corpus_id": "xenos", "chapter": "3" }
```

`chapter` accepts: numeric string (`"3"`), ordinal word (`"Three"`), or chapter title substring.

#### Output

Same envelope as `read` (`ReadOutput`).

#### HTTP
```
GET /corpora/:id/chapter/:ch
```

#### Errors
- `not_found` (404) if the chapter cannot be resolved

---

### 19. `character_profile`

Full profile for a character: entity metadata + relationships + summary.

#### Input
```json
{ "corpus_id": "xenos", "name": "Eisenhorn" }
```

#### Output (`data` field)
```json
{
  "entity": { "id": "...", "canonical_name": "Eisenhorn", ... },
  "edges": [ { "kind": "ally", "to_entity_id": "...", ... } ],
  "summary": "Gregor Eisenhorn is an Inquisitor of the Ordo Xenos..."
}
```

#### HTTP
```
GET /corpora/:id/character/:name
```

#### Errors
- `not_found` (404) if entity not found
- `ambiguous` (422) if name matches multiple entities

---

### 20. `find_scene`

Find the scene (chunk) where two entities first appear together.

#### Input
```json
{
  "corpus_id": "xenos",
  "entity_a": "Eisenhorn",
  "entity_b": "Bequin"
}
```

#### Output (`data` field)
```json
{
  "location": { "corpus_id": "xenos", "path": "ch/3/scene/2", "uri": "..." },
  "content": "Full text of the scene...",
  "entities_present": [ ... ]
}
```

#### HTTP
```
POST /corpora/:id/scene
Content-Type: application/json

{ "entity_a": "Eisenhorn", "entity_b": "Bequin" }
```

#### Errors
- `not_found` (404) if either entity is not found or they never co-occur

---

## Scholia tools

Scholia are non-destructive corrections applied at query time. They sit alongside the index without altering it, and can be listed or applied without reindexing. See `calli scholion` in the CLI.

> **MCP-only.** Both scholia tools are available on the MCP stdio surface only (`calli mcp`).
> They are intentionally absent from the HTTP REST API — write operations are kept off the web path.

### 21. `list_scholia`

List all scholia for a corpus, ordered by application time.

#### Input
```json
{ "corpus_id": "xenos" }
```

#### Output (`data` field)
```json
{
  "scholia": [
    {
      "id": "scholion-uuid",
      "corpus_id": "xenos",
      "kind": "merge",
      "applied_at": "2025-01-20T14:30:00Z"
    }
  ]
}
```

#### CLI equivalent
```
calli scholion list <corpus_id>
```

---

### 22. `apply_scholion`

Apply a scholion (correction) to the index. All operations are non-destructive and take effect at query time.

#### Input

The required fields depend on the `kind`:

```json
// merge — canonicalise two entities as one
{
  "corpus_id": "xenos",
  "kind": "merge",
  "entity_a_id": "uuid-a",
  "entity_b_id": "uuid-b",
  "canonical_id": "uuid-a"    // which entity to keep
}

// unmerge — split a previously merged entity back into separate occurrences
{
  "corpus_id": "xenos",
  "kind": "unmerge",
  "entity_id": "uuid",
  "split_by": "scene"         // "scene" or "chapter"
}

// rename — override the canonical display name
{
  "corpus_id": "xenos",
  "kind": "rename",
  "entity_id": "uuid",
  "new_name": "Alizebeth Bequin"
}

// alias — add or remove lookup aliases
{
  "corpus_id": "xenos",
  "kind": "alias",
  "entity_id": "uuid",
  "add": ["Bequin"],
  "remove": []
}

// edit_summary — replace an LLM-generated summary with manual text
{
  "corpus_id": "xenos",
  "kind": "edit_summary",
  "target_kind": "entity",    // "entity" | "chunk" | "corpus"
  "target_id": "uuid",
  "text": "Alizebeth Bequin is Eisenhorn's most trusted untouchable..."
}

// entity_link — link two entities across corpora within a collection
{
  "collection_id": "eisenhorn-trilogy",
  "kind": "entity_link",
  "corpus_a_id": "xenos",
  "entity_a_id": "uuid-a",
  "corpus_b_id": "malleus",
  "entity_b_id": "uuid-b",
  "link_kind": "same_as",     // "same_as" | "implements" | "exemplifies" | "references" | "contrasts"
  "note": "Same character across books"
}
```

#### Output (`data` field)
```json
{ "ok": true, "scholion_id": "new-scholion-uuid" }
```

#### CLI equivalent
```
calli scholion apply <corpus_id> merge <entity_a> <entity_b>
calli scholion apply <corpus_id> rename <entity_id> <new_name>
```

---

## Collection tools

Collections group multiple corpora for cross-corpus queries. Create collections with `calli collection add` and add members with `calli collection add-member`.

### 23. `collection_list`

List all collections with member counts and kinds.

#### Input
```json
{}
```

#### Output (`data` field)
```json
{
  "collections": [
    {
      "id": "eisenhorn-trilogy",
      "name": "Eisenhorn Trilogy",
      "kind": "flat",
      "member_count": 3,
      "corpus_count": 3
    }
  ]
}
```

#### HTTP
```
GET /collections
```

---

### 24. `collection_overview`

Overview of a collection: member corpora, nested collections, total entity counts, and cross-corpus link counts by kind.

#### Input
```json
{ "collection_id": "eisenhorn-trilogy" }
```

#### Output (`data` field)
```json
{
  "collection": { "id": "eisenhorn-trilogy", "name": "Eisenhorn Trilogy", ... },
  "corpora": [ { "id": "xenos", "name": "Xenos", ... }, ... ],
  "nested_collections": [],
  "total_chunks": 2450,
  "total_entities": 312,
  "cross_corpus_links_by_kind": { "same_as": 14, "references": 7 }
}
```

#### HTTP
```
GET /collections/:id
```

#### Errors
- `not_found` (404) if collection does not exist

---

### 25. `collection_search`

Keyword search across all corpora reachable from the collection. Results are ranked and labelled by source corpus.

#### Input
```json
{
  "collection_id": "eisenhorn-trilogy",
  "query": "Bequin untouchable",
  "mode": "keyword",   // "keyword" | "semantic" | "hybrid"
  "limit": 20
}
```

#### Output (`data` field)
```json
{
  "results": [
    {
      "corpus_id": "xenos",
      "corpus_name": "Xenos",
      "location": { "uri": "calli://xenos/ch/3", ... },
      "snippet": "...Bequin was an untouchable, a blank...",
      "relevance": 0.91
    }
  ]
}
```

#### HTTP
```
POST /collections/:id/search
Content-Type: application/json

{ "query": "Bequin untouchable", "mode": "keyword", "limit": 20 }
```

Note: `collection_id` is taken from the `:id` path parameter and need not be in the body.

#### Errors
- `not_found` (404) if collection does not exist

---

### 26. `collection_entity_resolve`

Resolve an entity name across all corpora in the collection. Returns direct matches with `SameAs` metadata and related entities via typed links. Useful when the same person or concept appears in multiple corpora under slightly different names.

#### Input
```json
{
  "collection_id": "eisenhorn-trilogy",
  "name": "Bequin"
}
```

#### Output (`data` field)
```json
{
  "matches": [
    {
      "corpus_id": "xenos",
      "entity": { "id": "uuid-a", "canonical_name": "Bequin", ... },
      "same_as": [ { "corpus_id": "malleus", "entity_id": "uuid-b" } ]
    }
  ],
  "related": [
    { "corpus_id": "xenos", "entity": { ... }, "link_kind": "references" }
  ]
}
```

#### HTTP
```
POST /collections/:id/entity/resolve
Content-Type: application/json

{ "name": "Bequin" }
```

Note: `collection_id` is taken from the `:id` path parameter and need not be in the body.

#### Errors
- `not_found` (404) if collection does not exist

---

### 27. `collection_entity_meet`

Find all co-occurrences of two entities across the collection. `SameAs`-aware: equivalent entities across corpora are treated as the same, so you can ask "where do Eisenhorn and Bequin appear together across the whole trilogy."

#### Input
```json
{
  "collection_id": "eisenhorn-trilogy",
  "entity_a": "Eisenhorn",
  "entity_b": "Bequin"
}
```

#### Output (`data` field)
```json
{
  "first_co_occurrence": {
    "corpus_id": "xenos",
    "location": { "uri": "calli://xenos/ch/3", ... }
  },
  "all": [
    { "corpus_id": "xenos", "location": { "uri": "calli://xenos/ch/3", ... } },
    { "corpus_id": "malleus", "location": { "uri": "calli://malleus/ch/7", ... } }
  ],
  "count": 15
}
```

#### HTTP
```
POST /collections/:id/meet
Content-Type: application/json

{ "entity_a": "Eisenhorn", "entity_b": "Bequin" }
```

Note: `collection_id` is taken from the `:id` path parameter and need not be in the body.

#### Errors
- `not_found` (404) if collection does not exist, either entity is not found, or they never co-occur

---

## Tool summary

| # | Tool | Scope | HTTP | LLM at query time |
|---|------|-------|------|-------------------|
| 1 | `corpus_list` | global | `GET /corpora` | No |
| 2 | `corpus_overview` | corpus | `GET /corpora/:id` | No |
| 3 | `corpus_themes` | corpus | `GET /corpora/:id/themes` | No |
| 4 | `search` | corpus | `POST /corpora/:id/search` | No |
| 5 | `read` | corpus | `GET /corpora/:id/read` | No |
| 6 | `related` | corpus | `GET /corpora/:id/related` | No |
| 7 | `summarize` | corpus | `GET /corpora/:id/summary` | No |
| 8 | `entity` | corpus | `GET /corpora/:id/entity/:name` | No |
| 9 | `entity_edges` | corpus | `GET /corpora/:id/entity/:id/edges` | No |
| 10 | `entity_meet` | corpus | `POST /corpora/:id/meet` | No |
| 11 | `entity_contracts` | corpus | `GET /corpora/:id/entity/:entity/contracts` | No |
| 12 | `entity_search_by_abstract_kind` | multi-corpus | `POST /search/by-abstract-kind` | No |
| 13 | `list_abstract_kinds` | global | `GET /abstract-kinds` | No |
| 14 | `explain_component` | corpus | `POST /corpora/:id/explain` | No |
| 15 | `find_unreachable` | corpus | `GET /corpora/:id/unreachable` | No |
| 16 | `entities_without_tests` | corpus | `GET /corpora/:id/untested` | No |
| 17 | `find_inconsistencies` | corpus | `GET /corpora/:id/inconsistencies` | No |
| 18 | `chapter_summary` *(composite)* | corpus | `GET /corpora/:id/chapter/:ch` | No |
| 19 | `character_profile` *(composite)* | corpus | `GET /corpora/:id/character/:name` | No |
| 20 | `find_scene` *(composite)* | corpus | `POST /corpora/:id/scene` | No |
| 21 | `list_scholia` | corpus | MCP-only | No |
| 22 | `apply_scholion` | corpus/collection | MCP-only | No |
| 23 | `collection_list` | global | `GET /collections` | No |
| 24 | `collection_overview` | collection | `GET /collections/:id` | No |
| 25 | `collection_search` | collection | `POST /collections/:id/search` | No |
| 26 | `collection_entity_resolve` | collection | `POST /collections/:id/entity/resolve` | No |
| 27 | `collection_entity_meet` | collection | `POST /collections/:id/meet` | No |

All query-time responses are assembled from pre-indexed artifacts. No tool calls an LLM at query time.
