# Callimachus MCP Tools Reference

All 12 tools are available via both the MCP stdio server (`calli mcp`) and the HTTP REST API (`calli serve`).

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

## 1. `corpus_list`

List all indexed corpora.

### Input
```json
{}
```

### Output (`data` field)
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

### HTTP
```
GET /corpora
```

---

## 2. `corpus_overview`

Full metadata and top entities for one corpus.

### Input
```json
{ "corpus_id": "xenos" }
```

### Output (`data` field)
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

### HTTP
```
GET /corpora/:id
```

### Errors
- `not_found` (404) if corpus does not exist

---

## 3. `search`

Full-text, semantic, or hybrid search over chunks.

### Input
```json
{
  "corpus_id": "xenos",
  "query": "Bequin",
  "mode": "keyword",      // "keyword" | "semantic" | "hybrid" (default: keyword)
  "limit": 20,            // optional, default 20
  "scope": null           // optional scope filter
}
```

### Output (`data` field)
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

### HTTP
```
POST /corpora/:id/search
Content-Type: application/json

{ "query": "Bequin", "mode": "keyword", "limit": 20 }
```

Note: `corpus_id` in the request body is overridden by the `:id` path parameter.

### Errors
- `invalid_input` (400) if `query` is missing

---

## 4. `entity`

Look up a named entity by name or ID.

### Input
```json
{ "corpus_id": "xenos", "name_or_id": "Eisenhorn" }
```

### Output (`data` field)
```json
{
  "id": "entity-uuid",
  "corpus_id": "xenos",
  "canonical_name": "Eisenhorn",
  "kind": "character",
  "aliases": ["Gregor Eisenhorn", "Inquisitor Eisenhorn"],
  "appearances": 187,
  "confidence": 0.95
}
```

### HTTP
```
GET /corpora/:id/entity/:name
```

### Errors
- `not_found` (404) with fuzzy name suggestions
- `ambiguous` (422) if multiple entities match the name

---

## 5. `entity_edges`

Get relationships for an entity.

### Input
```json
{
  "corpus_id": "xenos",
  "entity_id": "entity-uuid",
  "direction": "both",      // "inbound" | "outbound" | "both"
  "kind": null,             // optional edge kind filter
  "limit": 50               // optional, default 50
}
```

### Output (`data` field)
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

### HTTP
```
GET /corpora/:id/entity/:id/edges?direction=both&kind=ally&limit=20
```

---

## 6. `entity_meet`

Find locations where two entities co-occur.

### Input
```json
{
  "corpus_id": "xenos",
  "entity_a": "Eisenhorn",
  "entity_b": "Bequin"
}
```

### Output (`data` field)
```json
{
  "first_co_occurrence": { "corpus_id": "xenos", "path": "ch/3", "uri": "calli://xenos/ch/3" },
  "all": [ { "corpus_id": "xenos", "path": "ch/3", "uri": "..." }, ... ],
  "count": 7
}
```

### HTTP
```
POST /corpora/:id/meet
Content-Type: application/json

{ "entity_a": "Eisenhorn", "entity_b": "Bequin" }
```

### Errors
- `not_found` (404) if either entity is not found or they never co-occur

---

## 7. `read`

Read the content of a chunk at a location URI.

### Input
```json
{
  "corpus_id": "xenos",        // optional — inferred from location URI if omitted
  "location": "calli://xenos/ch/3",
  "depth": "full"              // "summary" | "scenes" | "full" (default: full)
}
```

### Output (`data` field)
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

### HTTP
```
GET /corpora/:id/read?location=calli://xenos/ch/3&depth=full
```

### Errors
- `not_found` (404) if no chunk exists at the location
- `invalid_input` (400) if `location` query param is missing

---

## 8. `summarize`

Get or generate a summary for a corpus, entity, location, or range.

### Input
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

### Output (`data` field)
```json
{
  "text": "Summary text...",
  "generated_at": "2025-01-15T10:00:00Z",
  "model": "claude-3-5-haiku-20241022"
}
```

### HTTP
```
GET /corpora/:id/summary
GET /corpora/:id/summary?entity_id=entity-uuid
GET /corpora/:id/summary?location=calli://xenos/ch/3
GET /corpora/:id/summary?from=calli://xenos/ch/3&to=calli://xenos/ch/5
```

### Errors
- `not_found` (404) if no summary exists for the target

---

## 9. `related`

Find chunks related to a location by entity co-occurrence.

### Input
```json
{
  "corpus_id": "xenos",
  "location": "calli://xenos/ch/3",
  "limit": 10    // optional, default 10
}
```

### Output (`data` field)
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

### HTTP
```
GET /corpora/:id/related?location=calli://xenos/ch/3&limit=10
```

### Errors
- `invalid_input` (400) if `location` query param is missing

---

## 10. `chapter_summary` (composite)

High-level summary for a chapter: overview + entities present + child scenes.

### Input
```json
{ "corpus_id": "xenos", "chapter": "3" }
```

`chapter` accepts: numeric string (`"3"`), ordinal word (`"Three"`), or chapter title substring.

### Output

Same envelope as `read` (`ReadOutput`).

### HTTP
```
GET /corpora/:id/chapter/:ch
```

### Errors
- `not_found` (404) if the chapter cannot be resolved

---

## 11. `character_profile` (composite)

Full profile for a character: entity metadata + relationships + summary.

### Input
```json
{ "corpus_id": "xenos", "name": "Eisenhorn" }
```

### Output (`data` field)
```json
{
  "entity": { "id": "...", "canonical_name": "Eisenhorn", ... },
  "edges": [ { "kind": "ally", "to_entity_id": "...", ... } ],
  "summary": "Gregor Eisenhorn is an Inquisitor of the Ordo Xenos..."
}
```

### HTTP
```
GET /corpora/:id/character/:name
```

### Errors
- `not_found` (404) if entity not found
- `ambiguous` (422) if name matches multiple entities

---

## 12. `find_scene` (composite)

Find the scene (chunk) where two entities first appear together.

### Input
```json
{
  "corpus_id": "xenos",
  "entity_a": "Eisenhorn",
  "entity_b": "Bequin"
}
```

### Output (`data` field)
```json
{
  "location": { "corpus_id": "xenos", "path": "ch/3/scene/2", "uri": "..." },
  "content": "Full text of the scene...",
  "entities_present": [ ... ]
}
```

### HTTP
```
POST /corpora/:id/scene
Content-Type: application/json

{ "entity_a": "Eisenhorn", "entity_b": "Bequin" }
```

### Errors
- `not_found` (404) if either entity is not found or they never co-occur
