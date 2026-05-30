# PR 1 — Honest provenance: schema migration + provenance types

**Scope:** the first of 5 sequenced PRs implementing the honest-provenance
refactor. This PR is **mechanical and adds no observable behaviour change** —
it lands the foundation that subsequent PRs build on.

**Source plan:** the full 5-PR plan is at
`/Users/hammer/Code/callimachus/docs/plans/honest-provenance-implementation.md`.
That document is the canonical reference for schema details, type definitions,
and the broader design rationale. **Read it first.** This PR file scopes the
work you'll do today.

**PRD:** `/Users/hammer/Code/callimachus/docs/plans/honest-provenance.md`
(read for the WHY).

## Goal of PR 1

Land a new migration (`013_honest_provenance.sql`) that introduces the
schema for tagged-union provenance, the unified `*_history` shape with a
uniqueness constraint to prevent the middle-out duplicate-row bug, the
Layer-2 cache table, the artifact tombstone table, embeddings provenance,
content_hash on entities, and file_shape_hash on chunks. Plus the
`Provenance` enum and trait surface in Rust. **All wired through a
backward-compatible facade so existing tests pass unchanged.**

Subsequent PRs (out of scope for this job):
- PR 2 — Walker rewrite + cascade unification (kills `copy_unchanged_artifacts`)
- PR 3 — Layer-2 cache plumbing through LLM passes + stable-sampling
- PR 4 — Tombstones runtime + `calli history migrate-fresh` CLI
- PR 5 — Convergence test + cleanup of legacy `derived_at_version TEXT`

**Do NOT do PR 2-5 in this PR.** Leave the new trait methods as naive
implementations that fall back to existing behaviour (described in the
"Trait surface" section of the source plan). The point of PR 1 is that
nothing observable changes — only the substrate.

## Concrete deliverables

1. **Migration file:** `crates/callimachus-core/migrations/013_honest_provenance.sql`.

   Implements every item under `§Schema changes` in the source plan:
   - Part 1: `derived_at_sha TEXT` and `derived_predates_sha TEXT` on every
     head table (entities, edges, entity_purposes, entity_contracts,
     entity_blocks, summaries, themes, embeddings, chunks). Old
     `derived_at_version TEXT` stays for backward compat (PR 5 drops it).
   - Part 2: `content_hash TEXT` on entities; `file_shape_hash TEXT` and
     `entity_id_list TEXT` on chunks.
   - Part 3: Unified `*_history` mirror with `UNIQUE(corpus_id, id,
     derived_at_kind, derived_at_sha)` constraint. This is the structural
     fix for the middle-out duplicate-row bug.
   - Part 4: `embeddings` gains provenance columns + `embeddings_history`
     mirror table (resolves the embeddings-no-history-archival bug).
   - Part 5: `layer2_cache` table per the source plan's spec.
   - Part 6: `artifact_tombstones` table.
   - Part 7: Drop `location_uri` from edge's natural identity (keep as
     metadata column; remove from any uniqueness/index that uses it).
   - Part 8: Update pre-existing rows: copy `derived_at_version` →
     `derived_at_sha` for all head and history rows. Set
     `derived_predates_sha = NULL` for existing rows (they're presumed
     concrete from prior runs).

2. **Provenance type module:** `crates/callimachus-core/src/types/provenance.rs`.

   The `Provenance` enum (`Concrete(String)` | `RangePredating(String)`),
   constructors, serialisation to/from the two SQL columns,
   `is_valid_at(target_sha)` helper, refinement helper
   (`refine(self, observed_sha) -> Provenance`).

3. **Trait additions:** `crates/callimachus-core/src/storage/backend.rs`.

   New methods on `StorageBackend`:
   - `entity_list_at_sha(corpus_id, target_sha) -> Vec<Entity>` —
     **naive impl**: delegate to existing `entity_list_at_version`.
     The proper SHA-aware reconstruction lands in PR 2.
   - `archive_to_history(table_kind, corpus_id, id, derived_at,
     superseded_at_sha)` — **naive impl**: dispatch to the existing
     `archive_*` methods. Real unification lands in PR 2.
   - `refine_provenance(corpus_id, id, kind, observed_sha) -> RefineResult` —
     **stub**: return `RefineResult::Unchanged`. Real implementation in PR 2.
   - `tombstone_insert(...)` — **fully implemented**: writes to the new
     `artifact_tombstones` table.
   - `layer2_cache_get(key) -> Option<CachedArtifact>` and
     `layer2_cache_put(key, value)` — **fully implemented**: read/write
     the new `layer2_cache` table. No callers yet (PR 3 wires these up).

4. **SqliteBackend implementations:** `crates/callimachus-core/src/storage/sqlite.rs`.

   All trait method bodies. Migration registration. Indexes.

5. **PostgresBackend implementations:** `crates/callimachus-core/src/storage/postgres.rs`.

   Same trait methods; same migration applied via the Postgres migrator.
   May not have an applied migration in this PR if Postgres tests don't
   run by default; document the migration parity in the file's docstring.

6. **`BackfillStorageWrapper` updates:** `crates/callimachus-core/src/storage/backfill.rs`.

   Delegate the new trait methods through to the inner backend. The
   wrapper continues to redirect head-table writes to `*_history` as
   before; new methods just pass through.

## Tests (Redd writes first)

- **Migration round-trip test.** Build a small pinakes on the old schema
  fixture (use one of the existing test fixtures), apply migration 013,
  assert:
  - All new columns exist with the documented types.
  - Existing rows have `derived_at_sha` populated (copied from
    `derived_at_version`).
  - Existing rows have `derived_predates_sha = NULL`.
  - The `UNIQUE(corpus_id, id, derived_at_kind, derived_at_sha)` index
    rejects a duplicate insert.
  - The new tables (`layer2_cache`, `artifact_tombstones`,
    `embeddings_history`) exist and are queryable.

- **`Provenance` type unit tests.** Roundtripping through SQL columns,
  `is_valid_at` semantics, refinement monotonicity (refining a
  `RangePredating(X)` with observed_sha `Y` where `Y` is an ancestor of
  `X` produces `Concrete(Y)`; refining a `Concrete(_)` is a no-op).

- **Trait naive-impl smoke tests.** Confirm `entity_list_at_sha`
  delegates correctly to `entity_list_at_version`. Confirm
  `tombstone_insert` and `layer2_cache_{get,put}` round-trip.

- **Existing test suite stays green.** This is the critical acceptance
  criterion. Run `cargo test --workspace` and confirm zero new failures.

## Non-deliverables (explicitly defer)

- Walker rewrite — PR 2.
- Cascade unification — PR 2.
- Removing the `copy_unchanged_artifacts` method — PR 2.
- Layer-2 cache reads/writes inside pass code — PR 3.
- Stable-sampling — PR 3.
- File-shape hash computation in the chunk pass — PR 3.
- Embeddings cache hookup — PR 3.
- Tombstone runtime writes from cascade/walker — PR 4.
- `calli history migrate-fresh` CLI — PR 4.
- Convergence test — PR 5.
- Dropping `derived_at_version TEXT` — PR 5.

Leaving these for their dedicated PRs preserves reviewability and lets us
ship the foundation cleanly.

## Acceptance criteria

- `cargo build --release` succeeds.
- `cargo test --workspace` passes (no new failures, no flakes).
- `cargo clippy --workspace --all-targets -- -D warnings` is clean
  (CI gates on this).
- The new migration applies cleanly to a fresh pinakes AND to a
  pre-migration pinakes built from existing fixtures.
- The PR description documents:
  - What's added (with column-level schema diff).
  - What's deferred to PR 2-5 (link this scoping doc).
  - The migration round-trip test as the headline guarantee.

## Routing

- **Cody:** opus tier, high effort. Schema migrations are correctness-
  critical and touch storage layer trait code.
- **Redd:** sonnet tier, high effort. Migration round-trip test +
  `Provenance` unit tests + smoke tests for the naive trait methods.
- **Perri:** review the migration SQL carefully, especially the
  `INSERT INTO new_table SELECT ...` style rebuilds. Watch for FK
  drops/re-adds, index parity, idempotency.
- **Marty:** optional polish pass on the new module structure.

## Routing config

```yaml
suggested_config:
  cody:
    model: opus
    effort: high
    rationale: "Schema migration across every artifact table + new trait surface in 3 backends; uniqueness must catch middle-out duplicates; backward-compat facade must keep existing tests green."
  redd:
    model: sonnet
    effort: high
    rationale: "Migration round-trip test is the headline guarantee of no observable behaviour change. Plus Provenance type unit tests covering refinement monotonicity, the refactor's invariant."
  perri:
    model: sonnet
    effort: high
    rationale: "Reviewer must verify the SQL migration is idempotent, FK drops/re-adds are clean, indexes have parity with the old shape, and the backward-compat facade genuinely makes existing tests pass unchanged."
  marty:
    model: sonnet
    effort: medium
    rationale: "Optional polish on the new `types/provenance.rs` module structure once the substance lands."
```

## References

- Full implementation plan: `docs/plans/honest-provenance-implementation.md`
- PRD: `docs/plans/honest-provenance.md`
- Bug reports this work resolves (note: only the schema-level fixes land
  in PR 1; behaviour fixes land in PRs 2-4):
  - `.claude/bugs/open/middle-out-path-produces-divergent-duplicate-history.md`
    (uniqueness constraint added in this PR; cascade/walker reconciliation
    in PR 2)
  - `.claude/bugs/open/embeddings-no-history-archival.md`
    (history table added in this PR; runtime archival in PR 3/4)
  - `.claude/bugs/open/head-mode-theme-archival-missing.md`
    (themes table gets the unified provenance; runtime fix in PR 2)
  - `.claude/bugs/open/history-backfill-resume-stuck-on-partial-shas.md`
    (no schema work; this dissolves in PR 2 with the walker rewrite)
