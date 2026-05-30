# Honest provenance: implement tagged-union version stamps, two-layer caching, and tombstones across Callimachus

## Context

Callimachus currently stamps every artifact (chunk, entity, edge, purpose,
contract, summary, theme, embedding) with a bare SHA in `derived_at_version`,
and reconstructs "state at SHA X" via exact-SHA-match. To keep that query
honest the diff-based walker shipped in PR #34 *copies unchanged artifacts
forward* at every commit, paying a measured 9× storage cost (703 MB vs
76.8 MB on the real callimachus corpus) and creating four open bugs that
share the same underlying defect:

- `.claude/bugs/open/middle-out-path-produces-divergent-duplicate-history.md`
- `.claude/bugs/open/history-backfill-resume-stuck-on-partial-shas.md`
- `.claude/bugs/open/head-mode-theme-archival-missing.md`
- `.claude/bugs/open/embeddings-no-history-archival.md`

The PRD at `docs/plans/honest-provenance.md` commits the project to a
tagged-union provenance model (`Concrete(sha)` vs `RangePredating(sha)`),
a Layer-1 (content-addressable) / Layer-2 (cache-keyed by Layer-1 identity
plus surrounding-context-hash plus model) split, file-shape as the
purpose-pass scope, per-commit theme re-derivation, tombstones for death
events, opt-in stable-sampling, and a fresh-start migration. PR #34 just
landed so its `copy_unchanged_artifacts` machinery is the load-bearing
piece this refactor deletes.

This plan converts the PRD into a concrete, ordered implementation
sequence: schema migration `013`, a new history-layer module that owns
provenance, walker/cascade unification, pipeline reshaping, Layer-2 cache
key plumbing, stable-sampling plumbing, a `migrate-fresh` CLI subcommand,
and a four-path convergence test against the toy fixture at
`/Users/hammer/Code/pinakes-toy/`.

## Target

- **Repo:** callimachus (this workspace, `/Users/hammer/Code/callimachus`)
- **Branch:** `feature/honest-provenance` (with per-PR sub-branches off of it)
- **Base:** `origin/main`

## Resolutions of Ada's deferred open questions

These are decisions Archie makes so the implementation is unambiguous.
Each is the position the rest of this plan assumes; revisit only with
explicit user buy-in.

### Q1. Entity identity: **name+kind, with content_hash as a refinable observation, not part of identity**

- Storage-layer entity identity = `(corpus_id, canonical_name, kind)`.
  The existing `entities.id` (an opaque deterministic hash of those three
  fields) stays the primary key and stays the value other tables foreign-
  key against.
- `content_hash` becomes a non-key column on the entity row (and on the
  history mirror). When an entity's body changes the row's `content_hash`
  advances and a new `Concrete(sha)` provenance tag is written; the entity
  is "the same entity, observed at a new substrate." Layer-2 cache lookups
  use `(entity_id, content_hash, file_shape_hash, model)` as the key, so a
  body change naturally invalidates Layer-2.
- Renames are handled out-of-band by an explicit `aliases_pass` rename-
  detection step (already exists; we wire it to emit a `renamed_from`
  marker on the new entity row). The old entity gets a tombstone with
  reason `renamed_to=<new_id>`. Purposes do not propagate automatically:
  the new entity's first Layer-2 lookup will miss and re-derive. This is
  acceptable — renames are rare and getting a fresh purpose against the
  new name is usually the right behaviour. A follow-up flag
  `--carry-purpose-on-rename` is left as future work.

Rationale: name+kind is the storage-cheapest identity that survives body
changes, and the empirical Layer-2 cache hit rate is dominated by
file-shape stability anyway. Name+content would explode entity rows by an
order of magnitude on real corpora and would not improve cache hit rates
in the experiments we have.

### Q2. Range-alias arity: **one-sided `RangePredating(sha)`, no lower bound**

- The tag union is exactly `Concrete(sha)` and `RangePredating(sha)`. No
  two-sided `Range(after_X, before_Y)`.
- Refinement during backfill *narrows* the upper bound only:
  `RangePredating(C20)` may become `RangePredating(C10)` (still uncertain
  but more recent), or `Concrete(C10)` (if C10's diff touched the
  substrate). It may never widen.
- Reason: the lower bound is information we rarely query against, and a
  two-sided range adds bookkeeping (two columns, more index work, more
  refinement-path branches) without buying us much. If a future query
  shape demands it we add it as a non-breaking schema extension.

### Q3. Tombstone lifecycle: **kept forever; one row per death event**

- A tombstone is a row in `<artifact>_tombstones` keyed by
  `(corpus_id, artifact_id, removed_at_sha)`. It carries the same
  tagged-union provenance as a normal artifact row:
  `derived_at_kind = 'concrete' | 'range_predating'` + `derived_at_sha`.
- Once written, tombstones stay; they are the audit trail for "when did
  X disappear?" and they are required for the death-aware query semantics
  in §3 of this plan. A `prune` subcommand can compact them later if
  storage becomes a concern, but no compaction logic ships in this PR
  sequence.
- Tombstones are written by the history layer (not by individual passes)
  whenever the walker's diff observes a previously-known artifact absent
  from the current commit's substrate.

### Q4. Theme ordering across walks

Themes are corpus-level and re-derived per commit. The plan asserts no
cross-commit ordering dependency: each commit's theme is a pure function
of `(corpus state at that commit, model, theme-pass version)`. Walking
backfill out of order is therefore safe; the only invariant is that the
theme row at SHA X carries `Concrete(X)` and supersedes any earlier
theme row carrying `Concrete(Y)` with `Y` an ancestor of X. The history
layer enforces this via the same uniqueness constraint as the other
artifact tables: `UNIQUE(corpus_id, theme_logical_id, derived_at_sha)`.

### Q5. Rename purpose-propagation

Covered above: **no automatic propagation.** Renames produce a tombstone
on the old entity and a fresh entity row; the fresh row will trigger a
new Layer-2 derivation on its first access.

## Schema changes (migration `013_honest_provenance.sql`)

The migration is forward-only, additive where possible, but it does
**drop and recreate** several tables because column-rename + uniqueness-
constraint changes are simpler that way under SQLite's ALTER TABLE
limits. Since the PRD commits to fresh-start migration (existing pinakes
get wiped, not converted), the destructive shape is fine — but the
migration still needs to run cleanly on a fresh-empty schema so the
test suite passes.

### Part 1 — Replace `derived_at_version: TEXT` with a tagged-union pair on every head table

For every head table that today has a `derived_at_version TEXT` column
(`entities`, `edges`, `entity_purposes`, `entity_contracts`,
`entity_blocks`, `summaries`, `themes`), and for `chunks` (which uses
`introduced_at_version` / `last_modified_at_version` instead), add:

```sql
ALTER TABLE <table> ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE <table> ADD COLUMN derived_at_sha  TEXT NOT NULL DEFAULT '';
```

The old `derived_at_version TEXT` column stays on the head table for one
more migration as a *deprecated mirror* (cody nulls it on new writes,
keeps it readable for tooling that hasn't been updated). It will be
dropped in a follow-up migration after the new query paths land. Old
indexes (`idx_entities_derived` etc.) are dropped and replaced:

```sql
DROP INDEX idx_entities_derived;
CREATE INDEX idx_entities_provenance ON entities (corpus_id, derived_at_sha, derived_at_kind);
```

`chunks` keeps both `introduced_at_version` (the SHA where the chunk's
content_hash first appeared in this corpus) and `last_modified_at_version`
(the SHA where its current substrate was last touched). Both now carry an
adjacent `_kind` column with the same domain (`concrete | range_predating`).

### Part 2 — Add `content_hash` to entities, add `file_shape_hash` and `entity_id_list` to chunks

- `entities` gains `content_hash TEXT NOT NULL DEFAULT ''`. Populated by
  the structure pass; it is a hash of the entity's canonical textual body
  (the substrate the Layer-2 passes will see). Two entities with the same
  `(canonical_name, kind, content_hash)` are interchangeable from the
  caching layer's point of view.
- `chunks` (file-grain) gains `file_shape_hash TEXT NOT NULL DEFAULT ''`
  and `entity_id_list TEXT NOT NULL DEFAULT '[]'` (a JSON array, ordered).
  `file_shape_hash = sha256(canonical(entity_id_list))` where
  `canonical` is the JSON array of the file's top-level entity ids in
  source order. This is the file-shape the PRD specifies for the
  purpose-pass cache key.

### Part 3 — Replace `*_history` tables with a unified `<artifact>_history` shape that carries the tagged union and a uniqueness constraint

Every history mirror table gets recreated (`DROP TABLE IF EXISTS … ;
CREATE TABLE …`) with this shape (entities shown; same pattern for the
other six):

```sql
CREATE TABLE entities_history (
    history_id            INTEGER PRIMARY KEY,
    id                    TEXT NOT NULL,
    corpus_id             TEXT NOT NULL,
    canonical_name        TEXT NOT NULL,
    kind                  TEXT NOT NULL,
    content_hash          TEXT NOT NULL DEFAULT '',
    aliases               TEXT NOT NULL,
    description           TEXT,
    first_location_uri    TEXT,
    last_location_uri     TEXT,
    appearance_count      INTEGER NOT NULL,
    confidence            REAL NOT NULL,
    derived_at_kind       TEXT NOT NULL CHECK (derived_at_kind IN ('concrete','range_predating')),
    derived_at_sha        TEXT NOT NULL,
    -- supersession columns retained for chronology / audit, but the *query
    -- semantics* now key off (derived_at_kind, derived_at_sha) plus
    -- tombstones, not off superseded_at_version.
    superseded_at_sha     TEXT,            -- nullable: a Concrete row may be terminal
    superseded_at         TEXT NOT NULL    -- wall-clock, audit only
);
CREATE UNIQUE INDEX uq_entities_history_identity
    ON entities_history (corpus_id, id, derived_at_kind, derived_at_sha);
CREATE INDEX idx_entities_history_id        ON entities_history (corpus_id, id);
CREATE INDEX idx_entities_history_at_sha    ON entities_history (corpus_id, derived_at_sha);
```

The `UNIQUE(corpus_id, id, derived_at_kind, derived_at_sha)` index is the
storage-side fix for the middle-out duplicate-row bug. Both the cascade
and the walker will `INSERT … ON CONFLICT (corpus_id, id,
derived_at_kind, derived_at_sha) DO UPDATE SET …` so first-writer wins
on identity but Layer-1 fields (which are deterministic) can be
re-asserted idempotently. Layer-2 fields (purposes/contracts/etc.) on a
duplicate insert *log a warning* and refuse the second write — the
divergent-content half of the bug is then loud, not silent.

Same shape (with kind+sha + unique index) applied to:
`edges_history`, `entity_purposes_history`, `entity_contracts_history`,
`entity_blocks_history`, `summaries_history`, `themes_history`, plus a
new `chunks_history` and a new `embeddings_history` (closing the
fourth bug).

### Part 4 — `embeddings` gains provenance + a history mirror

```sql
ALTER TABLE embeddings ADD COLUMN derived_at_kind TEXT NOT NULL DEFAULT 'concrete'
    CHECK (derived_at_kind IN ('concrete','range_predating'));
ALTER TABLE embeddings ADD COLUMN derived_at_sha TEXT NOT NULL DEFAULT '';
ALTER TABLE embeddings ADD COLUMN surrounding_context_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE embeddings ADD COLUMN model TEXT NOT NULL DEFAULT '';

CREATE TABLE embeddings_history ( … same shape as embeddings + history columns … );
CREATE UNIQUE INDEX uq_embeddings_history_identity
    ON embeddings_history (corpus_id, chunk_id, derived_at_kind, derived_at_sha, model);
```

### Part 5 — Layer-2 cache table

A single shared table for memoising Layer-2 derivations across SHAs:

```sql
CREATE TABLE layer2_cache (
    cache_key            TEXT PRIMARY KEY,    -- sha256(artifact_kind || entity_id || content_hash || file_shape_hash || model)
    artifact_kind        TEXT NOT NULL,       -- 'purpose' | 'contract' | 'summary' | 'embedding' | 'theme'
    entity_id            TEXT,                -- nullable for corpus-level artifacts
    content_hash         TEXT NOT NULL,
    file_shape_hash      TEXT NOT NULL,
    model                TEXT NOT NULL,
    stable_sampling      INTEGER NOT NULL DEFAULT 0,
    payload              TEXT NOT NULL,       -- JSON; pass-specific
    created_at           TEXT NOT NULL,
    first_seen_at_sha    TEXT NOT NULL,       -- audit only
    hit_count            INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_layer2_cache_lookup ON layer2_cache (artifact_kind, entity_id, content_hash, file_shape_hash, model);
```

The Layer-2 passes consult this table *before* calling the LLM. A hit
serves the cached payload; a miss derives, writes the cache row, then
writes the head/history rows with whatever provenance the history layer
hands the pass.

### Part 6 — Tombstones

```sql
CREATE TABLE artifact_tombstones (
    tombstone_id         INTEGER PRIMARY KEY,
    corpus_id            TEXT NOT NULL,
    artifact_kind        TEXT NOT NULL,        -- 'chunk' | 'entity' | 'edge' | 'embedding'
    artifact_id          TEXT NOT NULL,
    derived_at_kind      TEXT NOT NULL CHECK (derived_at_kind IN ('concrete','range_predating')),
    derived_at_sha       TEXT NOT NULL,
    reason               TEXT,                 -- 'removed' | 'renamed_to=<id>' | …
    created_at           TEXT NOT NULL
);
CREATE UNIQUE INDEX uq_artifact_tombstones_identity
    ON artifact_tombstones (corpus_id, artifact_kind, artifact_id, derived_at_kind, derived_at_sha);
CREATE INDEX idx_artifact_tombstones_lookup
    ON artifact_tombstones (corpus_id, artifact_kind, artifact_id);
```

Tombstones are *not* mirrored in a head table — they are write-once
records of "this artifact stopped existing at this SHA." Query semantics:
an artifact is present at SHA X iff it has at least one
`Concrete(Y≤X) | RangePredating(Y≥X)` row AND it has no tombstone with
`Concrete(Z)` for `root ≤ Z ≤ X` *or* the tombstone is itself
`RangePredating(Z)` with `Z > X` (i.e. removed strictly after X). This
combined predicate is implemented in a new SQL view, `entities_at_sha`,
parameterised by the target SHA at query construction.

### Part 7 — Drop `location_uri` from edge identity

`edges.id` (the deterministic hash) and `edges_history.id` are recomputed
from `(corpus_id, from_entity_id, to_entity_id, kind)` only. `location_uri`
stays as a non-key column on the head row, capturing the most recent
observation. Migration: `edges_history` is destructively recreated above;
the fresh-start migration will rebuild edges from scratch.

### Part 8 — Update pre-existing rows on migration

The migration also does:

```sql
UPDATE entities SET derived_at_kind = 'concrete',
                    derived_at_sha = COALESCE(derived_at_version, '')
              WHERE derived_at_sha = '';
-- same for the other head tables.
```

…with the explicit caveat in the migration's comment that this is only
honest if the pinakes was built without copy-forward. The recommended
path for any existing pinakes is `calli history migrate-fresh` (§8 of
this plan); the SQL fallback exists for the test fixtures only.

## Trait changes (`StorageBackend` in `crates/callimachus-core/src/storage/backend.rs`)

### Removed

- `copy_unchanged_artifacts` — gone. No replacement; the new walker
  never writes for unchanged substrate.
- `cascade_delete_dirty_subtree`'s **delete** phase — split into two
  methods so the walker can call the archive-only step without the
  destructive head delete (see below).
- All the per-artifact `*_history_insert` methods that take
  `(derived_at_version, superseded_at_version)` as bare strings — replaced
  with versions that take a `Provenance` enum (see types §).

### Added

```rust
/// Stage the head→history archive for a set of artifacts without touching
/// head rows. Used by both the walker and the cascade — the head delete
/// is a separate call so the walker can archive-then-update-tag without
/// dropping the head row.
fn archive_to_history(
    &self,
    corpus_id: &str,
    archive: &ArchiveSet,
    provenance: Provenance,
) -> Result<ArchiveStats>;

/// Refine the provenance tag on a head row. No-op if the row's current
/// tag is already at least as specific as `new`. Enforces monotonicity
/// (Q2): Concrete(_) is never overwritten by RangePredating(_), and a
/// RangePredating(X) is never overwritten by RangePredating(Y) with Y>X.
fn refine_provenance(
    &self,
    corpus_id: &str,
    artifact: ArtifactRef,
    new: Provenance,
) -> Result<RefineOutcome>;

/// Write a tombstone. Idempotent on (corpus_id, kind, id, kind+sha).
fn tombstone_insert(
    &self,
    corpus_id: &str,
    artifact: ArtifactRef,
    provenance: Provenance,
    reason: Option<&str>,
) -> Result<()>;

/// Layer-2 cache primitives.
fn layer2_cache_get(&self, key: &Layer2CacheKey) -> Result<Option<Layer2Payload>>;
fn layer2_cache_put(&self, key: &Layer2CacheKey, payload: &Layer2Payload, first_seen_at_sha: &str) -> Result<()>;

/// New query primitive: rows present at a given SHA under tagged-union
/// + tombstone semantics. Replaces the exact-match
/// `entity_list_at_version` for the new model; the old method is kept
/// for one release as a thin wrapper that returns the same rows when
/// called with a Concrete-only corpus.
fn entity_list_at_sha(&self, corpus_id: &str, sha: &str) -> Result<Vec<Entity>>;
fn entity_count_at_sha(&self, corpus_id: &str, sha: &str) -> Result<u64>;
// Equivalents added for chunks, edges, purposes, etc.
```

### New types (in `crates/callimachus-core/src/types/provenance.rs`, a new module)

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provenance {
    Concrete(String),       // SHA
    RangePredating(String), // SHA — known to predate this commit
}

#[derive(Clone, Debug)]
pub struct ArchiveSet {
    pub chunk_ids:    Vec<String>,
    pub entity_ids:   Vec<String>,
    pub edge_ids:     Vec<String>,
    pub purpose_ids:  Vec<String>,
    pub contract_ids: Vec<String>,
    pub block_ids:    Vec<String>,
    pub summary_ids:  Vec<String>,
    pub theme_ids:    Vec<String>,
    pub embedding_chunk_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum ArtifactRef { Chunk(String), Entity(String), Edge(String), /* … */ }

#[derive(Clone, Debug)]
pub enum RefineOutcome {
    Refined,            // tag tightened
    Unchanged,          // input was no more specific than current
    RejectedMonotonic,  // would have widened — refused
}

#[derive(Clone, Debug, Hash)]
pub struct Layer2CacheKey {
    pub artifact_kind: &'static str,
    pub entity_id: Option<String>,
    pub content_hash: String,
    pub file_shape_hash: String,
    pub model: String,
    pub stable_sampling: bool,
}
```

`Provenance` is the *single* abstraction passes and walker code use.
`derived_at_kind`/`derived_at_sha` are storage-encoding details; the
trait surface speaks `Provenance` everywhere.

## Walker rewrite (`crates/callimachus-core/src/indexing/history_walk.rs`)

### Conceptual shape

The walker no longer copies anything. Each iteration:

1. Compute the **substrate diff** for this SHA against the neighbour we
   came from (or against the empty 0-state for the root commit). This
   produces three sets per artifact kind: `Introduced`, `Modified`,
   `Removed`.
2. **Run the passes only on `Introduced ∪ Modified`.** The pipeline is
   given an explicit `ChangeManifest` listing only the dirty paths
   (existing behaviour), but with passes-are-provenance-agnostic so
   they only emit "here's the artifact" without stamping anything.
3. **Hand the pass output to the history layer.** A new module
   `crates/callimachus-core/src/indexing/history_layer.rs` takes
   `(commit_sha, walk_direction, pass_output, removed_set, virtual_head_state)`
   and is the *sole* writer of provenance + tombstones. It:
   - Stamps `Introduced ∪ Modified` artifacts with `Concrete(commit_sha)`.
   - For each artifact already in the substrate but *not* touched by the
     diff: calls `refine_provenance` to narrow its tag from
     `RangePredating(neighbour_sha)` to `RangePredating(commit_sha)` (on a
     backward walk; on a forward walk, no refinement happens — the
     existing `Concrete(older)` tag is already more specific).
   - For each artifact in `Removed`: writes a tombstone tagged
     `Concrete(commit_sha)`.

### `walk_history_forward`

Forward walks are now lighter: they only refine when they see a
substrate-affecting diff. No copy-forward, no `unchanged_entity_ids`
computation. The signature stays the same; the body becomes mostly the
substrate-diff → pass-run → history-layer-stamp sequence above. The
root commit's iteration runs the passes against everything (diff against
0-state is "introduced everything") and stamps `Concrete(root_sha)`
on every artifact. Subsequent commits stamp only their own diff;
artifacts untouched keep their existing `Concrete(older_sha)` tag,
which *is* the honest answer.

### `walk_history_backward`

Backward walks are where range-alias refinement does most of its work.
After indexing at HEAD:

- Every artifact at HEAD has `Concrete(HEAD)` (because HEAD was indexed
  in isolation as if the root, so every artifact's diff-touched check
  passed).

When backfilling from HEAD's parent down to `--from <root>`:

- For each older SHA in newest→oldest order:
  - Compute the diff against the next-newer SHA (which we just processed,
    or HEAD on the first step).
  - For artifacts touched by the diff: re-derive via the passes,
    stamp `Concrete(this_sha)`. The next-newer's row for the same
    artifact identity already carries `Concrete(next_newer_sha)` and is
    untouched.
  - For artifacts present in the next-newer state but not touched by the
    diff: refine their existing tag. If they were tagged
    `Concrete(next_newer_sha)`, leave them — that's still the honest
    answer ("untouched by this older commit's diff" doesn't move the
    most-recent-modification point). If they were tagged
    `RangePredating(next_newer_sha)`, refine to
    `RangePredating(this_sha)`.
  - For artifacts in the next-newer state but **absent from this commit's
    tree**: this is a birth-event boundary in reverse — they were
    introduced *between* this_sha and next_newer_sha. The natural
    representation is: they should not appear at this_sha. The history
    layer writes a tombstone tagged `Concrete(this_sha)` with
    reason=`absent_in_substrate`, so a forward query at this_sha
    correctly returns "not yet present."

### Resume-bug fix (the second bug report)

The walker's per-iteration loop now keys "what's the next SHA?" purely
off the in-memory `Vec<Oid>` produced by
`collect_first_parent_chronological`, never off the on-disk history
state. A new `corpus_set_backfill_cursor(corpus_id, next_sha_to_process)`
StorageBackend method persists the cursor at the end of each iteration,
and `walk_history_backward` reads it on entry to skip already-completed
SHAs. Partial-completion within a SHA is handled by the Layer-2 cache:
if the previous run wrote the purpose for entity E and crashed before
writing the contract, the resumed run's cache hit for the purpose makes
re-derivation cheap and the contract derivation completes.

### Middle-out fix (the first bug report)

With one writer (the history layer) and a hard `UNIQUE(corpus_id, id,
derived_at_kind, derived_at_sha)` index on every history table, the
walker and cascade can no longer both write divergent rows at the same
SHA. The cascade is also rewritten (next section) so it no longer writes
to `*_history` directly — only the history layer does. The duplicate-
row class of bug becomes structurally impossible.

## Cascade rewrite (`crates/callimachus-core/src/indexing/cascade.rs`)

Today the cascade is the HEAD-mode supersession path: it archives the
about-to-be-superseded rows to `*_history` and deletes them from head.
After the refactor:

- The cascade is invoked by the pipeline orchestrator (not by the
  walker; the walker does its own history-layer write).
- It calls `archive_to_history` for the substrate-affected artifacts,
  passing `Provenance::Concrete(previous_HEAD_sha)` so the archived rows
  carry honest provenance for the state they came from.
- It then deletes the head rows for the affected artifacts and lets the
  passes re-derive them under the new SHA. The new head rows are written
  by the passes (Layer 1) and stamped `Concrete(current_sha)` by the
  history layer at end-of-pipeline.

The cascade no longer touches themes directly. Theme archival is the
history layer's job and happens uniformly with the other artifacts
(closing the third bug report).

## Pipeline orchestrator changes (`crates/callimachus-core/src/indexing/pipeline.rs`)

### Provenance-agnostic passes

The pipeline's `IndexPipeline::run` is reshaped so the order is:

1. `History` pass / `ChangeManifest` construction (existing).
2. `cascade::run` (HEAD-mode only, no-op for backfill since the
   `BackfillStorageWrapper` skips head writes).
3. Layer-1 passes: `Chunk`, `Structure`, `Aliases`. These now write to
   head with `Provenance::Concrete(current_sha)` but they no longer
   directly stamp the field — they call `db.upsert_with_provenance(…)`
   helpers in the storage layer, which the storage layer implements as
   trivially as before.
4. Layer-2 passes: `Purpose`, `Contract`, `Summarize`, `Theme`,
   `Embed`. These consult `layer2_cache_get` before any LLM call. On
   miss they derive, write the cache, then write the head row.
5. **History-layer commit step** (new): the orchestrator calls
   `history_layer::commit(commit_sha, walk_direction, pass_outputs)`,
   which performs the refinement / tombstone writes for artifacts the
   passes did *not* touch.

### `IndexOptions` additions

```rust
pub struct IndexOptions {
    // existing fields…
    /// Opt-in deterministic sampling for Layer-2 passes.
    pub stable_sampling: bool,
    /// Set by the walker; passes treat the absent default as `HeadMode`.
    pub walk_direction: WalkDirection,  // HeadMode | Forward | Backward
}
```

`stable_sampling` is plumbed into every Layer-2 pass and through to the
LLM provider call site.

## Pass changes

### Layer-2 cache key plumbing

For each of the five Layer-2 passes (`purpose`, `contract`, `summarize`,
`theme`, `embed`):

1. Before calling the LLM, build a `Layer2CacheKey` from
   `(entity_id-or-None, content_hash, file_shape_hash-of-enclosing-chunk,
    model_id, stable_sampling)`.
2. Call `db.layer2_cache_get(&key)`. On hit, deserialise the payload and
   skip the LLM call. On miss, derive normally, then `layer2_cache_put`.
3. The pass-specific `payload` JSON contains exactly the fields the head
   row needs (e.g. for purpose: `{purpose, model, model_tier,
   generated_at}`). The pass deserialises and writes the head row as
   today.

### Stable-sampling opt-in (Pillar 3)

`callimachus_llm::LlmProvider`'s call methods grow optional
`temperature: Option<f32>` and `seed: Option<u64>` parameters
(adding to existing signatures rather than breaking them). The Anthropic
provider honours `temperature=Some(0.0)`; `seed` is ignored
(documented). The OpenAI embedding provider honours both. The
`DryRunProvider` echoes both parameters back deterministically. Passes
pass `Some(0.0)`/`Some(stable_seed)` iff
`opts.stable_sampling == true`.

### File-shape hash

Computed by the structure pass and stored on the chunks table at
file-grain. The hash function is exactly:

```
sha256( json_canonical( [ entity.id for entity in chunk.entities sorted by source_offset ] ) )
```

Two files with the same ordered list of top-level entity ids produce the
same `file_shape_hash`. Bodies are *not* part of this hash — that's the
point: a body edit that doesn't change which entities live in the file
keeps the file-shape stable and lets sibling entities' Layer-2 cache
entries survive.

## Migration: `calli history migrate-fresh`

A new CLI subcommand under `crates/callimachus-cli/src/commands/history.rs`:

```
calli history migrate-fresh <corpus> [--keep-config] [--yes]
```

Behaviour:

1. Loads the existing corpus configuration (name, kind, source path,
   adapter, model tier choices) from the pinakes.
2. Drops every artifact table's contents for this corpus (head + history +
   tombstones + Layer-2 cache rows).
3. Runs `calli index <corpus>` at HEAD to repopulate head tables under
   the new model.
4. Prompts the user to run `calli history backfill <corpus> --from <root>`
   as the follow-up step. Does *not* run backfill automatically because
   it is expensive — the user should know they're about to incur it.

The command refuses to run if the pinakes schema is already at the
post-`013` version unless `--force` is given (idempotency safety).

## Convergence test

### File

`crates/callimachus-core/tests/honest_provenance_convergence.rs`

### Fixture

Uses the toy repo at `/Users/hammer/Code/pinakes-toy/` directly. The
test reads the existing `run-three-commits.sh` semantics but executes
them as Rust subprocess calls so they run under `cargo test`. The toy
repo's three commits (C1 introduces `greet()`; C2 adds `farewell()`;
C3 modifies `greet()` and adds `shout()`) exercise modify / no-op /
introduce in one shape.

### Test structure

```rust
#[test]
#[ignore = "live-LLM convergence test; run via `cargo test --ignored convergence`"]
fn four_paths_converge_under_honest_provenance() {
    // 1. Build four pinakes: REF (single-shot at C3), A (backfill),
    //    B (forward HEAD walks), M (middle-out).
    // 2. For each, open as sqlite::Connection and run the four
    //    hard assertions below.
}
```

The cheap, CI-runnable counterpart uses `DryRunProvider`:

```rust
#[test]
fn four_paths_converge_dry_run() {
    // Same scaffolding but with DryRunProvider; Layer-2 passes produce
    // canned deterministic output. Asserts the Layer-1 invariants and
    // the no-duplicates invariant but skips the "honest provenance at
    // REF" check (DryRun doesn't represent the Concrete-vs-Range
    // distinction meaningfully without real LLM-induced churn). This
    // test runs in CI on every push.
}
```

### The four hard assertions (both tests)

1. **Layer 1 deterministic match across A, B, M, REF at HEAD.**
   `SELECT content_hash FROM chunks WHERE corpus_id=…`, sorted, must be
   identical across all four. Same for `SELECT id FROM entities` and
   `SELECT from_entity_id, to_entity_id, kind FROM edges`.
2. **No duplicate history rows.** For each of `entities_history`,
   `chunks_history`, `edges_history`, `entity_purposes_history`,
   `entity_contracts_history`, `entity_blocks_history`,
   `summaries_history`, `themes_history`, `embeddings_history`:
   `SELECT corpus_id, id, derived_at_kind, derived_at_sha, COUNT(*)
    GROUP BY 1,2,3,4 HAVING COUNT(*) > 1` must return zero rows in every
   pinakes. The uniqueness index makes this structural; the test
   asserts it explicitly anyway.
3. **Range-alias refinement is monotonic.** Build the four pinakes,
   snapshot every artifact's `(kind, sha)` before each backfill step,
   then assert that across the steps no artifact's tag ever transitions
   from `Concrete(X)` to `RangePredating(Y)` for any X, Y. (The check is
   per-artifact-identity across snapshots.)
4. **Honest provenance at REF.** In the REF pinakes (single-shot index
   at C3), `greet` — which the live toy repo introduced at C1 and
   modified at C3 — must carry `Concrete(C3)` (its substrate *was*
   touched by C3's diff against C2; REF doesn't see C2 so it diffs
   against 0-state, which still means "introduced+modified at C3"). But
   `farewell` — introduced at C2 and untouched at C3 — must carry
   `RangePredating(C3)` because REF only saw C3 in isolation and cannot
   honestly claim C3 is the most-recent-modification SHA for it. This
   is the "honest provenance" invariant in concrete form. *Note:* this
   only holds when a single-SHA index is told to honestly diff against
   0-state for the root case but treat every other isolated SHA as
   `RangePredating` for un-diffable artifacts. The Layer-1 passes must
   surface the "I can't prove this SHA is concrete" signal; the
   history layer reads it and chooses the tag.

### Helper: `scripts/compare-pinakes.py` extension

Extend the existing comparison script to also dump per-SHA
`(derived_at_kind, derived_at_sha)` histograms so a human can eyeball
the four pinakes side-by-side when the assertions fail. New section
`## 4. Provenance tag breakdown` appended to the report.

## Ordered implementation sequence (3–5 PRs)

The PRs land sequentially on `feature/honest-provenance` and each is
shippable in isolation as far as `cargo build && cargo test` go. PR
boundaries are chosen so each PR has a self-contained reviewable scope
and the system stays buildable between them. Mother queues these as
separate jobs; a downstream PR will not be queued until the upstream
one merges.

### PR 1 — Schema migration + provenance types (mechanical, no behaviour change)

Scope:

- `crates/callimachus-core/migrations/013_honest_provenance.sql`
  (everything in §Schema changes).
- `crates/callimachus-core/src/types/provenance.rs` (the `Provenance`
  enum + helpers).
- `StorageBackend` trait gains the new methods (with naive
  implementations: `entity_list_at_sha` falls back to
  `entity_list_at_version`; `archive_to_history` calls the existing
  `*_history_insert` methods; `refine_provenance` is a no-op stub that
  returns `Unchanged`; `tombstone_insert` writes to the new table;
  Layer-2 cache get/put work fully).
- `SqliteBackend` and `PostgresBackend` implementations of the above.
- `BackfillStorageWrapper` updates to delegate the new methods.
- Existing tests pass unchanged; this PR introduces no new behaviour.

Cody/Redd: Redd writes a migration round-trip test (build pinakes on
old schema fixture, run migration, assert the new columns exist and
old data is preserved). Cody implements.

### PR 2 — History layer + walker rewrite + cascade unification

Scope:

- New module `crates/callimachus-core/src/indexing/history_layer.rs`
  (the sole writer of provenance + tombstones).
- `history_walk.rs` rewrite: drop `copy_unchanged_artifacts` calls,
  drop `unchanged_entity_ids`, route all archival through
  `history_layer::commit`.
- `cascade.rs` rewrite: archive-only, no longer writes themes
  directly, routes through `history_layer`.
- `corpus_set_backfill_cursor` / `corpus_get_backfill_cursor`
  StorageBackend methods + sqlite/postgres impl + walker uses them.
- Remove `StorageBackend::copy_unchanged_artifacts` (the trait method
  and every implementation).
- Tests: extend the existing `walk_short_history_populates_history_tables`
  and `backfill_*` tests to assert no duplicate rows and to assert the
  cursor advances. Add a `walk_resumes_past_partial_sha` test that
  injects a `FailingProvider` (new test helper that succeeds N times
  then fails M).

Cody/Redd: Redd-first TDD on the resume test and the no-duplicates
assertion. Cody implements the walker rewrite once the failing tests
exist.

### PR 3 — Layer-2 cache + stable-sampling + file-shape hash

Scope:

- `callimachus-llm` `LlmProvider` trait gains optional
  `temperature` / `seed` params; Anthropic + DryRun + OpenAI embed
  providers honour them per §Pass changes.
- All five Layer-2 passes (`purpose_pass`, `contract_pass`,
  `summarize_pass`, `theme_pass`, `embed_pass`) consult
  `layer2_cache_get` before LLM calls and `layer2_cache_put` after.
- Structure pass computes `file_shape_hash` and `entity_id_list` for
  each chunk.
- `IndexOptions::stable_sampling` flag and end-to-end plumbing.
- Tests: per-pass cache-hit-skips-LLM tests with `DryRunProvider`
  that counts call invocations.

Cody/Redd: Redd writes the cache-hit count assertions; Cody implements
the cache plumbing and stable-sampling.

### PR 4 — Embeddings history + tombstones + CLI `migrate-fresh`

Scope:

- `embed_pass` writes embeddings with provenance via the history layer
  (closing the fourth bug).
- Tombstone writes wired into the history layer for the "removed in
  this diff" path.
- `entities_at_sha` / `chunks_at_sha` / etc. SQL views or queries
  implementing the death-aware presence predicate; head queries
  rewired to use them.
- `calli history migrate-fresh` subcommand (per §Migration).
- Tests: `migrate-fresh` round-trip test on the toy fixture; embeddings
  history assertions.

Cody/Redd: Redd writes round-trip and tombstone-aware-query tests;
Cody implements.

### PR 5 — Convergence test + docs + cleanup

Scope:

- `crates/callimachus-core/tests/honest_provenance_convergence.rs`
  (both the `#[ignore]`d live-LLM test and the DryRun CI test).
- Delete the legacy `derived_at_version TEXT` columns in a new
  migration `014_drop_legacy_provenance.sql`.
- Update `docs/codebase-analysis.md` and `CLAUDE.md` to mention the
  new model.
- Resolve / archive the four open bug reports into
  `.claude/bugs/open/resolved/` once green.

Cody/Redd: Redd authors the convergence test; Cody handles cleanup.

## Acceptance criteria

A reviewer should be able to check off each of these before approving
the final PR of the sequence.

- `cargo build --release` succeeds; `calli` binary runs without
  panicking.
- `cargo test` (default, no `--ignored`) passes everywhere, including
  the new `four_paths_converge_dry_run` test.
- `cargo test --ignored convergence` (live LLM) passes on a machine
  with valid Anthropic credentials, run against the toy fixture.
- The four bug reports in `.claude/bugs/open/` have been moved to
  `.claude/bugs/open/resolved/` with a date prefix.
- `calli history migrate-fresh <corpus>` rebuilds an existing pinakes
  cleanly under the new schema and the resulting `data/callimachus.pinakes`
  is regenerated and committed.
- Storage size on the real callimachus corpus, fully-backfilled, is
  within 2× of 76.8 MB (the PRD target).
- The `StorageBackend` trait no longer contains `copy_unchanged_artifacts`.
- `history_walk.rs` contains no call sites for copy-forward.
- For every history table, the uniqueness index `UNIQUE(corpus_id, id,
  derived_at_kind, derived_at_sha)` exists.
- PR body for the final PR mentions the four bug reports it closes.

## Out of scope

- Cross-corpus provenance composition (the larger PRD; depends on this).
- Multi-model artifact storage beyond exposing temperature/seed.
- General history compaction / range-alias pruning.
- Performance benchmarks beyond storage size + a regression test for
  per-commit LLM call count on the toy fixture.
- Adapter trait changes (the source adapters stay provenance-agnostic).
- Postgres-backend optimisation passes (the trait methods are
  implemented; deeper postgres-specific tuning is follow-up work).

```yaml
suggested_config:
  cody:
    model: opus
    effort: high
    rationale: "Five-PR refactor touching schema, walker, cascade, pipeline, passes, and LLM trait; correctness-critical, fragile interactions."
  redd:
    model: sonnet
    effort: high
    rationale: "TDD is load-bearing here — resume bug, no-duplicates, and convergence properties only show up via tests. High effort warranted."
  marty:
    model: sonnet
    effort: medium
    rationale: "Refactor will leave dead code (copy_unchanged_artifacts, old at_version methods) and overlapping helpers; medium cleanup pass per PR."
  perri:
    model: opus
    effort: high
    rationale: "Provenance defect would compound silently across every future corpus; reviewer must catch subtle monotonicity / uniqueness violations."
```
