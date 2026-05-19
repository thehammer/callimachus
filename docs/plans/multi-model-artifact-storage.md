# Multi-model artifact storage

## Context

Callimachus builds a queryable index over codebases. Its indexing pipeline produces four
kinds of LLM-generated artifacts per entity:

- **Purposes** — `entity_purposes` (why an entity exists)
- **Contracts** — `entity_contracts` (semantic contract: assumptions, risks, intent gap)
- **Summaries** — `summaries` (behavioural summary, also used for chunks/corpus)
- **Themes** — `themes` (corpus-level architectural invariants)

Each table already stores a `model TEXT` column recording which LLM produced the row.
But `entity_purposes` and `entity_contracts` are keyed on `entity_id` alone, `summaries`
is keyed on a UUID with no uniqueness constraint on `(target, model)`, and all four
stores use `INSERT OR REPLACE`. Running a higher-tier model on an entity therefore
silently overwrites the lower-tier output: you can't retain both, compare them, or let
the query layer prefer the best available.

This plan widens the cardinality of those tables from "one artifact per entity" to
"one artifact per (entity, model)", adds a `model_tier` column so the read path can
serve the best available artifact without callers specifying a model, and updates the
idempotency checks in each pass so a Sonnet run after a Haiku run *adds* a row instead
of skipping.

Feature request: `.claude/feature-requests/multi-model-artifact-storage.md`.

## Target

- **Repo:** `callimachus`
- **Branch:** `feature/multi-model-artifact-storage`
- **Base:** `origin/main`

## Files to change

### Schema

- `crates/callimachus-core/migrations/010_multi_model_artifacts.sql` — **new**.
  Rebuild `entity_purposes`, `entity_contracts`, and `summaries` with the new keys and
  the `model_tier` column. Backfill NULL `model` → `'unknown'`. Also adds `model_tier`
  to `themes` for consistency (no PK change needed — themes already use a UUID and
  themes are corpus-level, not per-entity, so duplication is fine).

  Note on numbering: the next free migration number is **010** (existing migrations end
  at `008_kind_taxonomy.sql`). The feature request mentioned "010" loosely — use 010.

  SQL outline (SQLite — `INSERT OR REPLACE` works on UNIQUE indexes too, but the
  cleanest approach is rebuild-and-copy):

  ```sql
  -- entity_purposes: composite PK (entity_id, model), add model_tier.
  CREATE TABLE entity_purposes_new (
      entity_id     TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      corpus_id     TEXT NOT NULL REFERENCES corpora(id) ON DELETE CASCADE,
      purpose       TEXT NOT NULL,
      model         TEXT NOT NULL,
      model_tier    TEXT NOT NULL DEFAULT 'unknown',
      generated_at  TEXT NOT NULL,
      PRIMARY KEY (entity_id, model)
  );
  INSERT INTO entity_purposes_new (entity_id, corpus_id, purpose, model, model_tier, generated_at)
      SELECT entity_id, corpus_id, purpose, COALESCE(model, 'unknown'), 'unknown', generated_at
      FROM entity_purposes;
  DROP TABLE entity_purposes;
  ALTER TABLE entity_purposes_new RENAME TO entity_purposes;
  CREATE INDEX idx_entity_purposes_corpus ON entity_purposes (corpus_id);
  CREATE INDEX idx_entity_purposes_tier   ON entity_purposes (entity_id, model_tier);

  -- entity_contracts: same treatment, preserve all columns from 006.
  CREATE TABLE entity_contracts_new (
      entity_id        TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
      corpus_id        TEXT NOT NULL REFERENCES corpora(id) ON DELETE CASCADE,
      is_public        INTEGER NOT NULL DEFAULT 0,
      is_must_use      INTEGER NOT NULL DEFAULT 0,
      is_deprecated    INTEGER NOT NULL DEFAULT 0,
      is_fallible      INTEGER NOT NULL DEFAULT 0,
      is_nullable      INTEGER NOT NULL DEFAULT 0,
      is_mutating      INTEGER NOT NULL DEFAULT 0,
      is_diverging     INTEGER NOT NULL DEFAULT 0,
      has_panic_risk   INTEGER NOT NULL DEFAULT 0,
      has_unsafe       INTEGER NOT NULL DEFAULT 0,
      is_incomplete    INTEGER NOT NULL DEFAULT 0,
      panic_call_count INTEGER NOT NULL DEFAULT 0,
      debt_markers     TEXT NOT NULL DEFAULT '[]',
      assumptions      TEXT NOT NULL DEFAULT '[]',
      risks            TEXT NOT NULL DEFAULT '[]',
      intent_gap       TEXT,
      caller_notes     TEXT,
      model            TEXT NOT NULL,
      model_tier       TEXT NOT NULL DEFAULT 'unknown',
      generated_at     TEXT NOT NULL,
      PRIMARY KEY (entity_id, model)
  );
  INSERT INTO entity_contracts_new SELECT
      entity_id, corpus_id, is_public, is_must_use, is_deprecated, is_fallible,
      is_nullable, is_mutating, is_diverging, has_panic_risk, has_unsafe,
      is_incomplete, panic_call_count, debt_markers, assumptions, risks,
      intent_gap, caller_notes, COALESCE(model, 'unknown'), 'unknown', generated_at
      FROM entity_contracts;
  DROP TABLE entity_contracts;
  ALTER TABLE entity_contracts_new RENAME TO entity_contracts;
  CREATE INDEX idx_entity_contracts_corpus ON entity_contracts (corpus_id);
  CREATE INDEX idx_entity_contracts_panic  ON entity_contracts (corpus_id, has_panic_risk);
  CREATE INDEX idx_entity_contracts_public ON entity_contracts (corpus_id, is_public);
  CREATE INDEX idx_entity_contracts_tier   ON entity_contracts (entity_id, model_tier);

  -- summaries: keep UUID PK, add UNIQUE(corpus_id, target_kind, target_id, model) + model_tier.
  -- Existing rows may have NULL model; backfill to 'unknown' first, then add column + unique.
  UPDATE summaries SET model = 'unknown' WHERE model IS NULL;
  -- SQLite cannot add NOT NULL to existing column without rebuild; rebuild.
  CREATE TABLE summaries_new (
      id TEXT PRIMARY KEY,
      corpus_id TEXT NOT NULL,
      target_kind TEXT NOT NULL,
      target_id TEXT NOT NULL,
      depth TEXT NOT NULL,
      text TEXT NOT NULL,
      model TEXT NOT NULL,
      model_tier TEXT NOT NULL DEFAULT 'unknown',
      generated_at TEXT NOT NULL,
      FOREIGN KEY (corpus_id) REFERENCES corpora(id) ON DELETE CASCADE,
      UNIQUE (corpus_id, target_kind, target_id, model)
  );
  -- If duplicates exist (same target+model), keep the latest generated_at.
  INSERT INTO summaries_new
      SELECT id, corpus_id, target_kind, target_id, depth, text, model, 'unknown', generated_at
      FROM summaries s1
      WHERE generated_at = (
          SELECT MAX(generated_at) FROM summaries s2
          WHERE s2.corpus_id = s1.corpus_id
            AND s2.target_kind = s1.target_kind
            AND s2.target_id = s1.target_id
            AND s2.model = s1.model
      );
  DROP TABLE summaries;
  ALTER TABLE summaries_new RENAME TO summaries;
  CREATE INDEX idx_summaries_corpus ON summaries (corpus_id);
  CREATE INDEX idx_summaries_target ON summaries (corpus_id, target_kind, target_id);

  -- -----------------------------------------------------------------------
  -- Historical backfill: `llm.name()` returned "anthropic-api" (the provider
  -- name) rather than the actual model name. Every artifact produced before
  -- this migration therefore has model = 'anthropic-api'. We know the actual
  -- model was 'claude-sonnet-4-5' (the DEFAULT_MODEL constant in
  -- callimachus-llm/src/anthropic.rs; no model override was ever shipped in
  -- config). Correct all four tables in one pass.
  -- -----------------------------------------------------------------------
  UPDATE entity_purposes  SET model = 'claude-sonnet-4-5' WHERE model = 'anthropic-api';
  UPDATE entity_contracts SET model = 'claude-sonnet-4-5' WHERE model = 'anthropic-api';
  UPDATE summaries        SET model = 'claude-sonnet-4-5' WHERE model = 'anthropic-api';
  UPDATE themes           SET model = 'claude-sonnet-4-5' WHERE model = 'anthropic-api';

  -- Derive model_tier from the now-corrected model column.
  UPDATE entity_purposes  SET model_tier = CASE
      WHEN model LIKE '%opus%'   THEN 'opus'
      WHEN model LIKE '%sonnet%' THEN 'sonnet'
      WHEN model LIKE '%haiku%'  THEN 'haiku'
      ELSE 'unknown' END;
  UPDATE entity_contracts SET model_tier = CASE
      WHEN model LIKE '%opus%'   THEN 'opus'
      WHEN model LIKE '%sonnet%' THEN 'sonnet'
      WHEN model LIKE '%haiku%'  THEN 'haiku'
      ELSE 'unknown' END;
  UPDATE summaries        SET model_tier = CASE
      WHEN model LIKE '%opus%'   THEN 'opus'
      WHEN model LIKE '%sonnet%' THEN 'sonnet'
      WHEN model LIKE '%haiku%'  THEN 'haiku'
      ELSE 'unknown' END;
  UPDATE themes           SET model_tier = CASE
      WHEN model LIKE '%opus%'   THEN 'opus'
      WHEN model LIKE '%sonnet%' THEN 'sonnet'
      WHEN model LIKE '%haiku%'  THEN 'haiku'
      ELSE 'unknown' END;

  -- themes: add model_tier and backfill model. No PK change (corpus-level, multiple is fine).
  ALTER TABLE themes ADD COLUMN model_tier TEXT NOT NULL DEFAULT 'unknown';
  UPDATE themes SET model = 'unknown' WHERE model IS NULL;
  ```

  The migration must run inside a transaction (`rusqlite_migration` handles this).
  Foreign keys must be temporarily disabled around the table-rebuilds (use
  `PRAGMA foreign_keys = OFF` at the top and `ON` at the bottom — but verify the
  migration runner does not already wrap things).

- `crates/callimachus-core/src/storage/db.rs:7-17` — append the new migration to the
  `Migrations::new(vec![...])` list:

  ```rust
  M::up(include_str!("../../migrations/010_multi_model_artifacts.sql")),
  ```

### Types — make `model` required

- `crates/callimachus-core/src/types/purpose.rs:9` — change `pub model: Option<String>`
  to `pub model: String`.
- `crates/callimachus-core/src/types/contract.rs:27` — same change. Note: `EntityContract`
  derives `Default`. Since `String::default()` is `""`, that's fine for the derive, but
  callers using `..EntityContract::default()` (see `contract_pass.rs:113`) must now
  always set `model` explicitly *before* the spread or the empty default will silently
  collide on `(entity_id, "")`. The existing `contract_pass.rs` callsite already sets
  `model: Some(llm.name().to_string())` — change to `model: llm.name().to_string()`,
  no spread issue.
- `crates/callimachus-core/src/types/summary.rs:47` — change to `pub model: String`.
- `crates/callimachus-core/src/types/theme.rs:11` — change to `pub model: String`.

### Add `model_tier` field and helper

- `crates/callimachus-llm/src/provider.rs` — add a free function:

  ```rust
  /// Maps an LLM model name (as returned by `LlmClient::name()`) to a coarse
  /// tier label used by storage to order artifacts by quality.
  /// Returns one of: "haiku", "sonnet", "opus", "unknown".
  pub fn model_tier(model_name: &str) -> &'static str {
      let lc = model_name.to_lowercase();
      if lc.contains("opus") { "opus" }
      else if lc.contains("sonnet") { "sonnet" }
      else if lc.contains("haiku") { "haiku" }
      else { "unknown" }
  }
  ```

  Export it from `crates/callimachus-llm/src/lib.rs`.

- Add a `pub model_tier: String` field to each of `EntityPurpose`, `EntityContract`,
  `Summary`, `Theme`. Populate it at construction sites by calling
  `callimachus_llm::model_tier(&model).to_string()`.

### Storage layer — multi-model reads and writes

- `crates/callimachus-core/src/storage/purpose_store.rs` — rewrite:
  - `upsert` SQL: `INSERT OR REPLACE INTO entity_purposes (entity_id, corpus_id, purpose, model, model_tier, generated_at) VALUES (?1..?6)`.
    The composite PK `(entity_id, model)` means re-running the same model replaces; a
    different model appends.
  - Replace `get(db, corpus_id, entity_id)` with **two** functions:
    - `get_best(db, corpus_id, entity_id) -> Option<EntityPurpose>` — selects the
      highest-tier row:
      ```sql
      SELECT ... FROM entity_purposes
      WHERE corpus_id = ?1 AND entity_id = ?2
      ORDER BY CASE model_tier
                 WHEN 'opus' THEN 3
                 WHEN 'sonnet' THEN 2
                 WHEN 'haiku' THEN 1
                 ELSE 0
               END DESC,
               generated_at DESC
      LIMIT 1
      ```
    - `get_for_model(db, corpus_id, entity_id, model) -> Option<EntityPurpose>` —
      exact match on `(entity_id, model)`.
  - Update `row_to_purpose` to read the new `model_tier` column and the now-non-null
    `model` column.

- `crates/callimachus-core/src/storage/contract_store.rs` — identical pattern:
  `get_best` (best-tier wins) and `get_for_model`. Update SQL column lists and
  `row_to_contract` to include `model_tier`. Keep `list` and
  `list_with_inconsistencies` returning every row (multiple per entity is now
  expected) — *but* add `list_best_per_entity` for callers (e.g. query service) that
  want one row per entity. Default `list_with_inconsistencies` to also dedupe to
  best-per-entity, since debt analysis should not double-report the same entity.

- `crates/callimachus-core/src/storage/summary_store.rs`:
  - `upsert`: `INSERT OR REPLACE` keyed by the new UNIQUE constraint
    `(corpus_id, target_kind, target_id, model)`. The `id` UUID PK still applies but
    `INSERT OR REPLACE` on a conflict-with-unique replaces the conflicting row's `id`
    as a side-effect. To preserve `id` stability, prefer:
    ```sql
    INSERT INTO summaries (...) VALUES (...)
    ON CONFLICT(corpus_id, target_kind, target_id, model) DO UPDATE SET
        text = excluded.text,
        model_tier = excluded.model_tier,
        generated_at = excluded.generated_at,
        depth = excluded.depth
    ```
  - Replace `get(...)` with `get_best(corpus_id, target_kind, target_id)` and
    `get_for_model(corpus_id, target_kind, target_id, model)`. Same `ORDER BY` ranking
    as above.

- `crates/callimachus-core/src/storage/theme_store.rs` — read `model_tier`, write it
  from the type field. No new methods required (themes are corpus-level, callers list
  all of them).

### `StorageBackend` trait — surface the new methods

- `crates/callimachus-core/src/storage/backend.rs:164-181` — update the Purpose and
  Contract sections:

  ```rust
  // Purpose
  fn purpose_upsert(&self, p: &EntityPurpose) -> Result<()>;
  /// Best-tier artifact for the entity (None ⇒ no row).
  fn purpose_get(&self, corpus_id: &str, entity_id: &str) -> Result<Option<EntityPurpose>>;
  fn purpose_get_for_model(&self, corpus_id: &str, entity_id: &str, model: &str)
      -> Result<Option<EntityPurpose>>;
  fn purpose_list(&self, corpus_id: &str) -> Result<Vec<EntityPurpose>>;

  // Contract
  fn contract_upsert(&self, c: &EntityContract) -> Result<()>;
  fn contract_get(&self, corpus_id: &str, entity_id: &str) -> Result<Option<EntityContract>>;
  fn contract_get_for_model(&self, corpus_id: &str, entity_id: &str, model: &str)
      -> Result<Option<EntityContract>>;
  fn contract_list(&self, corpus_id: &str) -> Result<Vec<EntityContract>>;
  fn contract_list_inconsistencies(&self, corpus_id: &str) -> Result<Vec<EntityContract>>;
  ```

  Keep `purpose_get`/`contract_get` returning the best-tier artifact so the MCP/HTTP
  surface is unchanged. Add a parallel `summary_get_for_model`:

  ```rust
  fn summary_get_for_model(&self, corpus_id: &str, target_kind: &SummaryTargetKind,
      target_id: &str, model: &str) -> Result<Option<Summary>>;
  ```

- `crates/callimachus-core/src/storage/sqlite.rs:371-410` — wire the new trait methods
  to `*_store::get_best` / `get_for_model` functions.

- `crates/callimachus-core/src/storage/postgres.rs` — add stub impls for the new trait
  methods returning `Err(unimplemented())`, matching the existing pattern.

### Indexing pipeline — idempotency by `(entity, model)`

- `crates/callimachus-core/src/indexing/purpose_pass.rs:41` — change:

  ```rust
  if !opts.full && db.purpose_get(&corpus.id, &entity.id)?.is_some() { ... skip ... }
  ```

  to:

  ```rust
  let model_name = llm.name();
  if !opts.full && db.purpose_get_for_model(&corpus.id, &entity.id, model_name)?.is_some() {
      // Already have an artifact from this exact model — skip.
      continue;
  }
  ```

  This is the key behaviour change: running Sonnet over a corpus that has been
  indexed with Haiku will now *add* Sonnet rows, not skip the entities. Running
  Haiku again after Haiku still skips (idempotent).

  Around line 100, change `model: Some(llm.name().to_string())` to
  `model: llm.name().to_string()` and set `model_tier: model_tier(llm.name()).to_string()`.

- `crates/callimachus-core/src/indexing/contract_pass.rs:41` — same treatment using
  `db.contract_get_for_model(...)`. Update the `EntityContract { .. }` literal at
  line 113-124 to set `model` and `model_tier`. Note the `..EntityContract::default()`
  spread — that's fine; explicit fields override.

- `crates/callimachus-core/src/indexing/summarize_pass.rs:197,236` — change
  `model: Some(llm.name().to_string())` → `model: llm.name().to_string()` and add
  `model_tier`. If a skip-if-exists check exists here too, update it to
  `summary_get_for_model`. (Read the file to confirm — pattern should mirror purpose
  and contract passes.)

- `crates/callimachus-core/src/indexing/theme_pass.rs:69` — same field update.

### Adapter and provider callsites — flip `Option<String>` to `String`

The following sites currently set `model: None`. Each must now provide a non-empty
model name. The right value depends on context:

- `crates/callimachus-llm/src/anthropic.rs:264`, `claude_code.rs:183`,
  `dry_run.rs:87,103,118,133`, `resolve.rs:36,45,171` — these are inside provider
  implementations; replace with `self.name().to_string()` (or the appropriate model
  identifier the provider knows about). For dry-run, use the literal `"dry-run"`.
- `crates/callimachus-llm/src/anthropic.rs:264` is inside a usage/cost record — pass
  the actual model id string already in scope.
- `crates/adapters/callimachus-adapter-book/src/{summarizer.rs:15,resolver.rs:54,extractor.rs:40}`,
  `crates/adapters/callimachus-adapter-wiki/src/summarizer.rs:24,56,92`,
  `crates/adapters/callimachus-adapter-code/src/summarizer.rs:158` — these adapters
  construct artifacts without LLM provenance. Use the literal `"unknown"`.
- `crates/adapters/callimachus-adapter-code/src/{summarizer.rs:63,114,adapter.rs:274,429,527}`
  already have model strings — drop the `Some(...)` wrapper.
- `crates/callimachus-cli/src/commands/export.rs:345` — uses literal `model: None`;
  this is a record being constructed from possibly missing data. Use `"unknown"`.
- `crates/callimachus-core/src/query/service.rs:435,1431` — same; use `"unknown"`.

For every construction site, also populate `model_tier`. A `From<&str>` impl or a
helper constructor `EntityPurpose::new_with_model(...)` may be cleaner than touching
every literal — at Cody's discretion. If a helper is added, document it at the type
definition.

### Query service — best-available reads stay transparent

- `crates/callimachus-core/src/query/service.rs:708,719,871` — these all call
  `db.contract_get(...)` / `db.purpose_get(...)`. Because we preserved the
  no-argument signature (`get` returns best-tier), **no changes needed here**. The
  MCP/HTTP surface is unchanged, which the feature request requires. Verify by
  inspection that no caller relies on a specific model id being returned.

### Tests

- `crates/callimachus-core/tests/multi_model_artifacts.rs` — **new** integration
  test file. Required cases:

  1. **Round-trip — two contracts, two models.** Create an entity. Upsert one
     contract with `model = "claude-haiku-4-5-20251001"`, then upsert another with
     `model = "claude-sonnet-4-5-20250929"`. Assert `contract_list(corpus_id)` returns
     **two** rows for that entity. Assert `contract_get(corpus_id, entity_id)`
     returns the Sonnet one (higher tier).
  2. **Best-tier read on opus + sonnet + haiku.** Insert all three; `purpose_get`
     returns the opus row.
  3. **Same-model upsert replaces, does not duplicate.** Two `purpose_upsert` calls
     with identical `(entity_id, model)` produce a single row.
  4. **`get_for_model` exact match.** After (1), `contract_get_for_model(..., "haiku-...")`
     returns the haiku row; mismatched model returns `None`.
  5. **`unknown` tier ranks lowest.** Insert one row with `model = "unknown"` and one
     with `"claude-haiku-..."`. `get` returns the haiku row.
  6. **Idempotency-by-model in pipeline.** Run `purpose_pass` twice over the same
     in-memory corpus with the same LLM mock → only one row per entity. Run again
     with a different mocked LLM name → second row added.

- `crates/callimachus-core/tests/phase12_pipeline.rs` — existing tests at lines 238,
  257, 281 currently call `db.purpose_get(...)` and `db.contract_get(...)` expecting
  a single artifact. These should continue to pass unchanged (best-tier read still
  returns Some). Update only if assertions inspect `.model.is_some()` — change to
  `.model != ""` or remove (model is now non-optional).

- `crates/callimachus-llm` — unit test for `model_tier()` covering each branch:
  `"claude-opus-..."`, `"claude-sonnet-..."`, `"claude-haiku-..."`, `"gpt-4"`,
  `"unknown"`, `""`, mixed case.

### Migration regression — keep the bundled demo db loadable

- `data/callimachus.db` is the bundled demo index. After this branch lands, anyone
  opening it via the MCP server triggers migration 010 against the on-disk file.
  Cody should:
  1. Run the CLI against `data/callimachus.db` after the change to confirm migration
     succeeds without error.
  2. Re-bundle `data/callimachus.db` in the same commit (the migration is destructive
     to the schema; once applied, downgrading is one-way). Document in the PR body
     that the bundled db has been re-saved post-migration.

## Approach

1. Write migration 010 and register it in `db.rs`. Confirm
   `cargo test -p callimachus-core` passes (existing tests load `open_in_memory`,
   which runs migrations).
2. Update the four type structs (`purpose.rs`, `contract.rs`, `summary.rs`, `theme.rs`)
   to change `Option<String>` → `String` for `model` and add the `model_tier` field.
   This will produce a wave of compile errors — work through them.
3. Add the `model_tier(&str) -> &'static str` helper in `callimachus-llm`. Export.
4. Update all `model: Some(...) / model: None` callsites listed above. For sites
   without real LLM provenance, use `"unknown"` plus `model_tier: "unknown".into()`.
5. Update each `*_store.rs`: rewrite SQL, add `get_best` and `get_for_model`,
   update `row_to_*` to read `model_tier`.
6. Update `StorageBackend` trait, `SqliteBackend` impl, and `PostgresBackend` stub.
   Keep the existing `purpose_get` / `contract_get` / `summary_get` signatures as
   "best-tier" reads so the MCP/HTTP surface is unchanged.
7. Update the three pipeline passes (`purpose_pass`, `contract_pass`,
   `summarize_pass`) to use `*_get_for_model` for the skip-if-exists check.
8. Write the new integration test file `multi_model_artifacts.rs`.
9. Re-run the CLI's indexing pass against the bundled `data/callimachus.db` to
   regenerate it under the new schema; commit the resulting binary.
10. Run `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`,
    `cargo test --workspace`.

## Acceptance criteria

- `cargo test --workspace` passes, including the new `multi_model_artifacts.rs` file.
- `cargo clippy --all-targets --all-features -- -D warnings` is clean.
- `cargo fmt --check` is clean.
- Migration 010 applies cleanly to a database created from migrations 001–008 with
  pre-existing data (some rows with NULL `model`, some with set values). Rows with
  NULL `model` are backfilled to `"unknown"` and tier `"unknown"`.
- `data/callimachus.db` opens via `calli --db data/callimachus.db inspect entities callimachus`
  without error after the branch is checked out.
- MCP `entity_get` and HTTP query endpoints behave identically to before for a
  single-model corpus (best-tier read of a single row is that row).
- Running a Haiku indexing pass then a Sonnet indexing pass over the same corpus
  produces **two** rows per affected entity in `entity_purposes` and
  `entity_contracts` (verify via `sqlite3 data/test.db 'SELECT entity_id, model, model_tier FROM entity_purposes'`).
- The PR body explains the migration and notes that `data/callimachus.db` has been
  regenerated.

## Out of scope

- Tiered model routing (deciding *which* entities to enrich with which tier). That
  is a separate, companion feature noted in the feature request.
- Surfacing per-model artifacts through the MCP/HTTP API (e.g. a `?model=sonnet`
  parameter on `entity_get`). The schema and storage layer support it; exposing it
  to callers is deferred until there is a UX need.
- Cost reporting / comparison between models. `pricing.rs` already exists; wiring
  it to a "delta between tiers" report is a follow-up.
- PostgreSQL implementation of any new method — stubs only, matching the existing
  pattern in `postgres.rs`.
- Cleaning up older NULL-model rows beyond the `'unknown'` backfill. If a real
  cleanup is wanted, file it as a separate plan.
- Changing `themes` cardinality semantics. Themes already support multiple rows
  per corpus; we only add the `model_tier` column for consistency.

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: high
    rationale: "Multi-file schema migration with type-system ripple effects across adapters, indexing passes, and storage trait; correctness-sensitive (data on disk)."
  redd:
    model: sonnet
    effort: high
    rationale: "Migration correctness and idempotency-by-model behaviour need careful test coverage; existing phase12 tests must keep passing while new round-trip cases are added."
  marty:
    model: sonnet
    effort: medium
    rationale: "Likely opportunity to consolidate the four near-identical *_get_for_model implementations and the model_tier ORDER BY clause behind a small helper."
  perri:
    model: sonnet
    effort: high
    rationale: "Schema migration touches the bundled demo db and shared StorageBackend trait; a missed regression here breaks every downstream consumer of the index."
```
