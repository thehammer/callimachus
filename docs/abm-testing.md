# A/B/M testing — multi-path provenance validation

This doc describes how Callimachus validates that its history-aware
indexing produces consistent representations regardless of the
**direction** the index was built. The technique is named for its
three generation paths — A (backward), B (forward), M (middle-out).

## Why we have this

The pinakes index is a queryable graph derived from a corpus's git
history. The same logical state at a given commit SHA should be
recoverable regardless of whether the index was built:

- by indexing HEAD and walking backward,
- by indexing the root commit and walking forward, or
- by starting at some midpoint and expanding outward.

When the three paths produce different artifact sets at the same SHA,
the data model has a bug. Direction-independence is therefore the
load-bearing invariant for the [honest-provenance model](./plans/honest-provenance.md);
A/B/M is the test that exercises it.

## The four pinakes

For any corpus with N first-parent commits (root → HEAD):

| Path | How it's built | What it tests |
|---|---|---|
| **REF** | Single-shot `calli ingest` + `calli index --pass all` at HEAD. No history pass, no backfill. | Reference snapshot — the "ground truth" current state. |
| **A** | `calli ingest` at HEAD → `calli history backfill --from <root>`. | The backward backfill walker (post PR #34, post PR #38). |
| **B** | Worktree at root, then for each of N first-parent commits: `git checkout <sha>` + `calli index --pass all`. | The forward HEAD-mode cascade path. |
| **M** | `calli ingest` at a midpoint commit, walk forward to HEAD with `calli index --pass all` per commit, then `calli history backfill --from <root>`. | The mixed-direction "middle-out" path — exercises both cascade and backfill walker in one pinakes. |

After all four are built, the comparison toolkit
(`scripts/compare-pinakes.py`) diffs them along several axes.

## What we expect to match

### Hard expectations (Layer 1 — deterministic)

Source-derived artifacts at HEAD must match across all four pinakes:

- **chunks** by `content_hash`
- **entities** of kind `file`, `function`, `module`, `class`, `interface`
  by `(canonical_name, kind)`
- **edges** by `(from_entity_id, to_entity_id, kind)` (when location_uri
  is excluded — drop is queued for a future PR)

Mismatch in any of these is a bug, not noise. Layer 1 is a pure function
of `git_tree(SHA)`; the LLM is not involved.

### No-duplicate-rows invariant

For every `*_history` table, every `(corpus_id, id, derived_at_kind,
derived_at_sha)` triple must have at most one row. This is enforced by
the schema's `UNIQUE` indexes (added in PR #36) and is the structural
fix for the middle-out duplicate-row bug.

### Soft expectations (Layer 2 — LLM-derived)

The LLM-derived artifacts — purposes, contracts, summaries, themes,
embeddings — are not byte-identical functions of source. Each call has
sampling noise. Expectations:

- **Cluster on meaning, not on prose.** Two purposes for the same entity
  should describe the same function/intent even when worded differently.
- **Stable within a fixed surrounding context.** Same chunk + same file
  shape + same model → ~60-100% consistent prose (we measured ~30%
  residual noise on the toy fixture).
- **Drift across surrounding contexts is real.** The same chunk in a
  different file shape produces measurably different purposes — the
  [context-leak finding](./plans/honest-provenance.md) is why Layer 2
  cache keys include `file_shape_hash`.

## Known divergence: theme accumulation

Themes are a partial exception to Layer 1 / Layer 2 cleanness. They are
corpus-level LLM artifacts: a function of the entity-set, not of any
single chunk. Forward HEAD-mode walks (B) **accumulate themes across
commits** because the LLM produces slightly different titles each
commit, so new theme IDs are minted at each iteration without
superseding the old ones. See
[`.claude/bugs/open/themes-accumulate-across-commits-via-title-drift.md`](../.claude/bugs/open/themes-accumulate-across-commits-via-title-drift.md).

Until that bug is fixed, expect:

- A's `themes` count = themes from a single LLM call at HEAD
- B's `themes` count = union of all theme titles across N commits (much higher)
- M's `themes` count = somewhere in between

The fix is in `theme_pass`: define theme generation as an *authoritative
writer* that supersedes any existing theme not present in its output,
rather than an additive writer.

## How to run it

### Automated convergence test (CI-running)

`crates/callimachus-core/tests/honest_provenance_convergence.rs` runs the
4-path comparison against a built-in 3-commit fixture using the
`DryRunProvider` for deterministic LLM output.

```bash
cargo test honest_provenance_convergence
```

This is the regression test. It runs in CI on every PR.

A live-LLM variant exists in the same file with `#[ignore]`:

```bash
ANTHROPIC_API_KEY=… cargo test --ignored honest_provenance_convergence_live_llm
```

### Manual experiment against any git corpus

The **toy fixture** at `/Users/hammer/Code/pinakes-toy/` is the smallest
reproducible test — 3 commits, 3 functions. Below the theme-pass
threshold (`MIN_ENTITIES_FOR_THEMES = 20`) so doesn't surface
theme-accumulation behaviour.

For real corpora, follow the **webster pattern**: see
`scripts/run-webster-abm.sh` (in this repo) for a worked example that
builds all three pinakes for the webster browser-extension corpus and
prints per-phase timings. Adapt the script's `WEBSTER` and `WALK` paths
to point at the corpus you want to validate.

After all three pinakes are built, run the comparison toolkit:

```bash
python3 scripts/compare-pinakes.py \
    --corpus <corpus_id> \
    --repo <path-to-source-git-repo> \
    --pinakes "REF=data/<name>-REF.pinakes" \
    --pinakes "A=data/<name>-A.pinakes" \
    --pinakes "B=data/<name>-B.pinakes" \
    --pinakes "M=data/<name>-M.pinakes"
```

The toolkit emits a Markdown report with:
- HEAD deterministic diff (chunks / entities / edges / themes)
- HEAD entity-kind breakdown
- Per-SHA history coverage

## Results we've validated

| Corpus | Date | Commits | Layer 1 match | Notes |
|---|---|---|---|---|
| pinakes-toy | 2026-05-29 (pre-refactor) | 3 | inconsistent | Surfaced the middle-out duplicate-row bug (M had 2× rows at C2 with divergent LLM output) |
| pinakes-toy | 2026-05-29 (post-refactor) | 3 | ✅ exact | A, B, M all agree on chunks/entities/edges; zero duplicates |
| webster | 2026-05-30 (post-refactor) | 44 | ✅ exact | All 192 chunks, 78 functions, 42 modules, 28 files match across A/B/M. Theme accumulation diverged (filed as bug). |

The original A/B/M experiment that motivated the honest-provenance
refactor (callimachus self-index, 52 commits) used the **pre-refactor**
code and produced the 9× storage divergence and middle-out duplication
that the refactor structurally fixed. We did not re-run that comparison
against the post-refactor model because webster on the post-refactor
model is the cleaner test (smaller, faster, easier to interpret), and
the toy fixture already automated the regression test.

## Open follow-ups

- **PR 6** — drop the legacy `derived_at_version TEXT` columns and
  switch the query layer from exact-SHA-match to honest tagged-union
  reads (`Concrete(sha)` vs `RangePredating(sha)`). Until this lands,
  the per-SHA history coverage table in the toolkit reflects the
  legacy semantics rather than the honest model. Plan not yet
  written.
- **Theme accumulation** — see
  [`.claude/bugs/open/themes-accumulate-across-commits-via-title-drift.md`](../.claude/bugs/open/themes-accumulate-across-commits-via-title-drift.md).
- **Comparison toolkit theme-stripped mode** — currently the toolkit
  reports raw entity/edge counts; the unique-per-pinakes counts are
  inflated by theme-related entities/edges. A `--exclude-kind theme`
  flag would let the report focus on structural divergence.

## References

- PRD: [`docs/plans/honest-provenance.md`](./plans/honest-provenance.md)
- Implementation plan: [`docs/plans/honest-provenance-implementation.md`](./plans/honest-provenance-implementation.md)
- Original analysis plan: [`docs/plans/three-pinakes-comparison.md`](./plans/three-pinakes-comparison.md)
- Toolkit: `scripts/compare-pinakes.py`
- Webster runner (worked example): `scripts/run-webster-abm.sh`
- Toy fixture: `/Users/hammer/Code/pinakes-toy/`
- Convergence test: `crates/callimachus-core/tests/honest_provenance_convergence.rs`
