# PR 5 — Convergence test + docs + cleanup

**Scope:** the fifth and final PR of the honest-provenance refactor. Lands
the headline acceptance test (full 4-path A/B/M/REF convergence on a
3-commit toy fixture), drops the legacy `derived_at_version TEXT` columns
from migration 012, updates documentation, and archives the four resolved
bug reports.

**Source plan:** `docs/plans/honest-provenance-implementation.md`.
**PRD:** `docs/plans/honest-provenance.md`.
**Predecessors:** PR #36 (substrate), PR #38 (history layer + walker rewrite),
the PR-3 cache PR (just landed), the PR-4 embeddings + tombstones + migrate-fresh PR.

Read both planning docs before starting. This file scopes the work.

## Goal of PR 5

1. **Ship the four-path convergence test** as the headline acceptance proof
   for the entire refactor. A, B, M, REF paths on a small fixture must
   produce identical deterministic content and satisfy the new model's
   invariants. Two flavours: a `DryRunProvider` CI-running test and a
   live-LLM `#[ignore]`d variant.
2. **Drop the legacy `derived_at_version TEXT` columns** that PR 1 retained
   as a backward-compat facade. By this point every query path reads
   `derived_at_sha`; the legacy column is dead weight.
3. **Update docs** so future readers understand the new model.
4. **Archive the four resolved bug reports** into
   `.claude/bugs/resolved/` (with a `YYYY-MM-DD-` date prefix per
   convention in `~/.claude/CLAUDE.md`).

## Concrete deliverables

1. **Convergence test:** `crates/callimachus-core/tests/honest_provenance_convergence.rs`.

   Three-commit deterministic git fixture (replicating
   `/Users/hammer/Code/pinakes-toy/`):
   - C1: introduce `greet()`.
   - C2: add `farewell()` (greet unchanged).
   - C3: modify `greet()` and add `shout()` (farewell unchanged).

   For each test (DryRun CI + live-LLM `#[ignore]`d):
   - Build four pinakes via separate paths: REF (single-shot at C3),
     A (backward backfill from C3 to C1), B (forward HEAD-mode walks
     C1→C2→C3), M (middle-out: C2 then forward to C3 then backfill to C1).
   - **Hard assertions** (must pass deterministically):
     - **Layer 1 deterministic match:** chunks by content_hash, entities
       by `(canonical_name, kind)`, edges by `(from, to, kind)`. All
       four pinakes match at HEAD.
     - **No duplicate history rows:** for every
       `(corpus_id, id, derived_at_kind, derived_at_sha)` triple in each
       `*_history` table, COUNT(*) = 1. This is the structural
       middle-out fix from PR 2.
     - **Range-alias refinement is monotonic:** walking backward, no
       entity's tag ever moves from `Concrete(X)` to `RangePredating(Y)`.
     - **Honest provenance at REF:** `greet` indexed only at C3 has
       `RangePredating(C3)`, not `Concrete(C3)`.
   - **Soft observations** (logged, not asserted): per-pinakes row counts
     by table, LLM token consumption (live-LLM variant only),
     entity-purpose diffing across the four paths.

2. **Migration `015_drop_legacy_provenance.sql`** (or next available
   number after PR 4's migration):
   - `ALTER TABLE ... DROP COLUMN derived_at_version` on every head
     table that has it (the eight tables touched by PR 1's substrate).
   - Note: SQLite's `DROP COLUMN` is available since 3.35 (March 2021),
     so this should work on current rusqlite. If the project pins an
     older SQLite, fall back to the table-rebuild idiom (CREATE NEW,
     INSERT INTO new SELECT ..., DROP OLD, RENAME) — same approach as
     migration 010.
   - Rebuild any `*_history` uniqueness indexes that used `COALESCE`
     against the legacy column to be column-only.
   - PR 1's migration 013 had this written into its header note as the
     planned cleanup; this PR honours that.

3. **Documentation updates:**
   - `CLAUDE.md` — section on the provenance model. Brief description of
     tagged-union provenance and the `history_layer` writer pattern.
     Pointer to the PRD for full design.
   - `docs/codebase-analysis.md` (if present) — same.
   - Inline doc comments on `Provenance`, `history_layer::commit`,
     `RefineOutcome` should already exist from PRs 1-2; this PR just
     audits them for completeness.

4. **Archive resolved bug reports.** Move all four to
   `.claude/bugs/resolved/` with the YYYY-MM-DD- prefix (today's date):
   - `head-mode-theme-archival-missing.md`
   - `embeddings-no-history-archival.md`
   - `middle-out-path-produces-divergent-duplicate-history.md`
   - `history-backfill-resume-stuck-on-partial-shas.md`

   Each move adds a one-line resolution note at the top:
   `**Resolved:** YYYY-MM-DD by the honest-provenance refactor (PRs #36, #38, #39, #N, #N+1).`

## Tests (Redd-first TDD for the convergence test)

The convergence test IS the deliverable. Authored by Redd with the
4-path orchestration helper extracted as `tests/common/four_paths.rs`
so future tests can reuse it.

- **DryRun convergence** (CI-running, deterministic):
  - All hard assertions pass.
  - DryRun outputs are deterministic by construction, so prose
    comparisons across the four paths assert byte equality.

- **Live-LLM convergence** (`#[ignore]`d):
  - Run with `cargo test --ignored convergence`.
  - Same hard assertions.
  - Soft assertions: per-pinakes purpose-text cosine similarity
    (over an inline TF-IDF or token-overlap function — no external
    embedding service required).
  - Reports LLM token usage and per-path wall-clock to stderr for
    operators reading test output.

- **Existing tests stay green:** especially the PR 2 `walk_resumes_*`
  and `walk_produces_no_duplicate_*` regressions; PR 3 cache-hit-skips
  tests.

## Non-deliverables

- No new behaviour. PR 5 is validation + cleanup only.
- No model changes. The model is locked from PR 1; only legacy column
  removal happens here.

## Acceptance criteria

- **Run `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings` before opening the PR.** PR 1 and PR 3 both hit CI failures on un-fmt'd new test files.
- `cargo build --release` succeeds.
- `cargo test --workspace` passes (DryRun convergence test included).
- `cargo test --workspace -- --ignored convergence` passes against a
  live LLM (run locally; not in CI).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- Four bug reports moved to `.claude/bugs/resolved/`.
- CLAUDE.md mentions the new model with a pointer to the PRD.
- The PR description celebrates the completion of the honest-provenance
  refactor with a brief summary of what changed across PRs 1-5 and what
  problems it solved.

## Routing config

```yaml
suggested_config:
  cody:
    model: sonnet
    effort: medium
    rationale: "Mechanical work: drop legacy columns, move bug reports, doc updates. The convergence test is Redd's deliverable; Cody handles cleanup."
  redd:
    model: sonnet
    effort: high
    rationale: "Convergence test is the headline proof of the refactor. Four paths, two flavours (DryRun CI + live-LLM ignored), four hard assertions including the no-duplicate-rows + honest-provenance-at-REF claims."
  perri:
    model: sonnet
    effort: high
    rationale: "Final reviewer for the whole refactor. Verify the four hard assertions are tight, legacy column drop is safe, archived bugs link back correctly."
  marty:
    model: sonnet
    effort: low
    rationale: "downgrade: PR 5 is cleanup + a single test file; very little refactor surface for Marty to act on."
```

## References

- Full implementation plan: `docs/plans/honest-provenance-implementation.md`
- PRD: `docs/plans/honest-provenance.md`
- Toy fixture (reference for the test fixture shape): `/Users/hammer/Code/pinakes-toy/`
- Comparison toolkit (reference for the diff logic): `scripts/compare-pinakes.py`
- Resolved bugs being archived:
  - `.claude/bugs/open/middle-out-path-produces-divergent-duplicate-history.md`
  - `.claude/bugs/open/history-backfill-resume-stuck-on-partial-shas.md`
  - `.claude/bugs/open/head-mode-theme-archival-missing.md`
  - `.claude/bugs/open/embeddings-no-history-archival.md`
