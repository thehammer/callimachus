# PR 2 — History layer + walker rewrite + cascade unification

**Scope:** the second of 5 PRs implementing the honest-provenance refactor.
This is the **behaviour-change PR** that follows PR 1's substrate. After this
lands, walker and cascade route every provenance write through a single
`history_layer` module — eliminating the middle-out duplicate-row bug and the
resume-stuck bug structurally.

**Source plan:** `docs/plans/honest-provenance-implementation.md` (full 5-PR plan).
**PRD:** `docs/plans/honest-provenance.md` (the WHY).
**Predecessor (already merged):** PR #36 — schema migration + Provenance types.

Read both planning docs before starting. This file scopes the implementation
work; it does not re-litigate design.

## Goal of PR 2

1. **Introduce `history_layer.rs`** as the only writer of provenance and
   tombstones. The pipeline orchestrator, walker, and cascade all route
   through it.
2. **Rewrite the walker** to drop `copy_unchanged_artifacts` and all the
   diff-based "re-stamp unchanged entities forward" logic. Walking is now
   "process this commit's diff, update tags on the affected subtree, leave
   everything else alone."
3. **Rewrite the cascade** to be archive-only, routing every write through
   `history_layer`. Cascade no longer touches themes directly.
4. **Persist a per-corpus backfill cursor** so resume after interruption
   advances past completed iterations cleanly. Resolves the resume-stuck bug.
5. **Remove `StorageBackend::copy_unchanged_artifacts`** entirely — the trait
   method and every implementation. This was PR #34's mitigation for the
   broken model; under honest provenance, it's not needed.

## Concrete deliverables

1. **New module:** `crates/callimachus-core/src/indexing/history_layer.rs`.

   - Exposes a single `commit(...)` entry point (or similar) that takes
     the diff between two SHAs (or between empty and a SHA, for the root)
     plus the artifacts the pipeline produced for the dirty subtree, and:
     - Writes provenance-stamped rows to head tables for new/modified
       artifacts (using `Concrete(this_sha)`).
     - Archives the about-to-be-superseded rows into `*_history`.
     - Writes tombstones for entities/chunks/edges that exist in the
       previous state but not this one.
     - Refines `RangePredating(later_sha)` tags into `Concrete(this_sha)`
       where applicable.
   - Uses the new trait methods from PR 1 (`archive_to_history`,
     `refine_provenance`, `tombstone_insert`).
   - **One writer per (artifact, SHA)** is the invariant. No double-writes
     possible by construction.

2. **Walker rewrite:** `crates/callimachus-core/src/indexing/history_walk.rs`.

   - Drop calls to `copy_unchanged_artifacts`.
   - Drop the `unchanged_entity_ids` plumbing.
   - Each iteration: compute the diff between current and adjacent
     already-processed neighbour, build a `ChangeManifest` with only the
     changed files dirty, run the pipeline, then call
     `history_layer::commit` with the diff + produced artifacts.
   - Forward walker: neighbour is the previous (older) commit just processed,
     or empty/0-state at root.
   - Backward walker: neighbour is the next (newer) commit just processed,
     or HEAD's tables on the first backward step.
   - Update `walk_history_convergence` test (from PR #34) — it should still
     pass once the rewrite is complete. If it doesn't, it's a bug in the
     rewrite, not the test.

3. **Cascade rewrite:** `crates/callimachus-core/src/indexing/cascade.rs`.

   - Pass dirty-file information + about-to-be-superseded artifact ids to
     `history_layer::commit` instead of calling `archive_*` methods
     directly.
   - Cascade no longer touches themes directly (themes are corpus-level;
     `history_layer` decides whether they need archiving based on whether
     any file in the corpus changed).
   - Resolves the `head-mode-theme-archival-missing` bug — themes now
     supersede on dirty-source commits through `history_layer`.

4. **Backfill cursor:** persistent per-corpus position.

   - New trait methods on `StorageBackend`:
     - `corpus_set_backfill_cursor(corpus_id: &str, sha: &str) -> Result<()>`
     - `corpus_get_backfill_cursor(corpus_id: &str) -> Result<Option<String>>`
   - New `corpora.backfill_cursor TEXT` column (add via inline `ALTER TABLE`
     at the top of the migration that ships with this PR, or via a
     migration 014).
   - Walker writes the cursor after each iteration completes successfully;
     reads it on start to skip already-processed commits.
   - On resume from a partial state, the cursor unambiguously says "next
     iteration is SHA X" — no inference from on-disk history needed.

5. **Remove `copy_unchanged_artifacts`:** Drop the trait method, every
   implementation in `sqlite.rs`, `postgres.rs`, `backfill.rs`, plus all
   callers. The commit message should call out the removal clearly so
   future readers understand the lineage.

## Tests (Redd-first TDD)

- **`walk_resumes_past_partial_sha`** (new): the regression test for the
  resume-stuck bug.
  - Construct an 8-commit fixture (reuse the convergence-test fixture from
    PR #34 if useful).
  - Use a `FailingProvider` test helper that succeeds for the first N LLM
    calls then fails for the next M. Start a backfill; let it fail mid-
    iteration; capture the partial `*_history` state.
  - Resume the backfill with a non-failing provider. Assert that the
    walker advances past the partial SHA on the next iteration and
    eventually reaches root with full coverage.
  - This is the bug today — it must fail before this PR's changes, pass
    after.

- **`walk_produces_no_duplicate_history_rows`** (new): the regression test
  for the middle-out duplicate-row bug.
  - Build a 3-commit fixture similar to `/Users/hammer/Code/pinakes-toy/`
    (C1, C2, C3) using a deterministic LLM provider (`DryRunProvider`).
  - Run the middle-out path: ingest at C2, forward to C3, backward backfill
    to C1.
  - Assert: for every `(corpus_id, id, derived_at_kind, derived_at_sha)`
    triple in each `*_history` table, COUNT(*) = 1.
  - This is the bug today — it must fail before this PR's changes, pass
    after.

- **Existing test family stays green.** Particularly the
  `history_walk_convergence` from PR #34: forward and backward walks must
  still produce identical history-row content (modulo LLM noise; the test
  uses DryRunProvider so it's deterministic).

- **Theme-archive test** (new): the regression for
  `head-mode-theme-archival-missing`. After a HEAD-mode incremental at a
  commit that changes any source file, assert that the prior themes
  appear in `themes_history` with `superseded_at_sha = new_sha`.

- **`FailingProvider` helper** (new): in `crates/callimachus-llm/src/`
  next to the existing `DryRunProvider`. Constructor takes `succeed_n`
  and `fail_m` counts. Wraps an inner provider. Used by the resume test.

## Non-deliverables (explicitly defer)

- Layer-2 cache reads/writes inside pass code — PR 3.
- Stable-sampling — PR 3.
- File-shape hash computation in the chunk pass — PR 3.
- Embeddings cache hookup — PR 3.
- Tombstone-aware reads (death-event filtering in queries) — PR 4.
- `calli history migrate-fresh` CLI — PR 4.
- Convergence test (full 4-path A/B/M/REF) with live LLM — PR 5.
- Dropping `derived_at_version TEXT` columns — PR 5.

## Acceptance criteria

- `cargo build --release` succeeds.
- `cargo test --workspace` passes — including:
  - `walk_resumes_past_partial_sha`
  - `walk_produces_no_duplicate_history_rows`
  - All PR #34 convergence tests
  - All PR 1 honest_provenance tests
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- The PR description documents:
  - The removal of `copy_unchanged_artifacts` and why it's no longer needed.
  - The two bugs structurally fixed (resume-stuck, middle-out duplicates) +
    the third bug that dissolves (head-mode-theme-archival-missing).
  - That walker and cascade now route through a single writer
    (`history_layer`).

## Routing config

```yaml
suggested_config:
  cody:
    model: opus
    effort: high
    rationale: "Walker + cascade rewrite is the heart of the refactor. Two structural bug fixes ride on getting the diff-based commit semantics exactly right; subtle interactions with PR 1's substrate."
  redd:
    model: sonnet
    effort: high
    rationale: "Two new regression tests are the proof: resume-stuck and no-duplicate-history. FailingProvider helper + 3-commit fixture; tests must fail before changes and pass after."
  perri:
    model: sonnet
    effort: high
    rationale: "Reviewer must verify history_layer is the sole writer of provenance + tombstones, copy_unchanged_artifacts is fully removed, and no path bypasses history_layer for *_history writes."
  marty:
    model: sonnet
    effort: medium
    rationale: "Refactor opportunity: consolidate any duplicated walker forward/backward logic into shared helpers once both work."
```

## References

- Full implementation plan: `docs/plans/honest-provenance-implementation.md`
- PRD: `docs/plans/honest-provenance.md`
- Predecessor PR (merged): PR #36, branch `feature/honest-provenance-pr1-schema`
- Bug reports resolved by this PR:
  - `.claude/bugs/open/middle-out-path-produces-divergent-duplicate-history.md`
  - `.claude/bugs/open/history-backfill-resume-stuck-on-partial-shas.md`
  - `.claude/bugs/open/head-mode-theme-archival-missing.md`
- PR #34's walker (the one being rewritten): `docs/plans/diff-based-history-walker.md`
