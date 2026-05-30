# Three-Pinakes Comparison: A vs B vs Middle-Out

**Captured:** 2026-05-26
**Status:** in-progress
**Area:** callimachus / experiment

## The three pinakes

| Tag | File | Generation method |
|---|---|---|
| **A** | `data/callimachus-A.pinakes` | Fresh ingest at HEAD → `history backfill --from <root>` (backward walk over 50 first-parent transitions, theme + default passes) |
| **B** | `data/callimachus-B.pinakes` | Forward walk: `git checkout` each of the 51 first-parent commits in chronological order, run `calli index` against worktree at each step |
| **M** | `data/callimachus.pinakes` | Recovered May-17 snapshot → incremental `calli index` at HEAD → `history backfill --from <root>` |

All three target the same source repo (Callimachus itself, 51 first-parent commits from
`33bd29c` → `9d799f6`). All three should converge to "the pinakes that describes the
codebase at HEAD, with history populated for every prior commit."

## What "match" means here

Two layers of comparison, with very different epistemic status:

### Deterministic layer — must match exactly
These are derived from source code by deterministic parsers. If they differ, that's
a bug in the pipeline, not an interesting finding.

- **Chunks** — by `id` (content hash). Set equality across A/B/M.
- **Entities** — by `id`. Set equality.
- **Edges** — by `(source, target, kind)`. Set equality.
- **Entity blocks** — by `(entity_id, block_index)`. Set equality.

If these don't match at HEAD, we have a bug to file.

### Non-deterministic layer — should largely match, won't match verbatim
These are LLM artifacts. Different generation order, different time, possibly
different model versions, possibly stochastic decoding. Expect convergence in
meaning, divergence in wording.

- **Entity purposes** — short NL string per (entity, model). Compare via embedding cosine.
- **Entity contracts** — boolean flags + NL lists (risks, assumptions, debt_markers).
  Booleans should largely agree; NL lists compared via embedding cosine over concatenated text.
- **Summaries** — NL string per target. Embedding cosine.
- **Themes** — title + statement per pass. Two comparisons:
  - Title-level: bag-of-titles per pinakes, Jaccard or Hungarian-matched on embedding similarity.
  - Statement-level: pairwise embedding cosine over matched themes.

### Historical layer — should match where it exists, exists where it should
History tables (`*_history`) are populated by B's forward walk (commit-by-commit as
it advances) and by A/M's backward backfill. Compare:

- **History row count per SHA** — for each prior first-parent SHA, count
  `entities_history`, `edges_history`, `themes_history`, etc. Should be in the
  same ballpark across A/B/M. Wide divergence = different sense of "what
  changed at this commit."
- **History coverage** — A/M only backfill the *theme* pass on phase 6 (per
  the work just shipped). B's forward walk runs *all* passes at each commit
  and so should produce richer non-theme history. Note this is asymmetric
  by design; it's a feature not a bug, but document it.

## Comparison toolkit (to build while pipelines run)

Probably a small `scripts/compare-pinakes.py` (or Rust binary if we want to
ship it):

### Phase 1 — deterministic diff
- Open all three pinakes read-only.
- For each table in `{chunks, entities, edges, entity_blocks}`:
  - Pull all PKs from each.
  - Report: `|A ∩ B ∩ M|`, `|A \ (B ∪ M)|`, `|B \ (A ∪ M)|`, `|M \ (A ∪ B)|`, pairwise diffs.
  - Expectation: all set-diffs empty. Any non-empty is a bug report.

### Phase 2 — boolean-flag diff (contracts)
- For each entity, compare the 11 boolean flags across A/B/M.
- Report: per-flag disagreement counts.
- Expectation: low disagreement (<5%). High disagreement on a specific flag = LLM
  consistency issue worth investigating.

### Phase 3 — semantic diff (purposes, contracts NL, summaries, themes)
- Pull the artifact text from each pinakes.
- Use an embedding model (whatever the pipeline uses — `voyage-3` or similar) to embed.
- For each entity/target, compute pairwise cosine A↔B, A↔M, B↔M for the artifact.
- Aggregate: histogram of cosines, mean, P10, count below threshold (e.g. <0.85).
- Expectation: long tail at high cosine (>0.9), small low-cosine tail to inspect by hand.

### Phase 4 — theme alignment
- Themes don't have a stable PK across runs — they're generated freshly each time.
- For each (pinakes, SHA) pair, pull all themes at that SHA.
- Hungarian-match themes across A/B/M by embedding similarity of statements.
- Report: matched count, mean match cosine, unmatched themes from each side.

### Phase 5 — history coverage
- For each first-parent SHA in (HEAD..root), count `*_history` rows in each pinakes,
  broken down by table.
- Render as a grid: rows=SHAs (chronological), columns=`A_themes, A_entities, B_themes, B_entities, M_themes, M_entities`.
- Look for: B should have richer non-theme history (entity_purposes_history,
  entity_contracts_history, summaries_history) since it ran full passes each commit.

## Reporting

Goal: a single Markdown report at `docs/plans/three-pinakes-comparison-results.md` with:

1. Headline numbers: deterministic-match rate, semantic-match rate per artifact kind,
   theme-alignment rate.
2. Surprises section: anything that diverged where it shouldn't, anything that converged
   where we expected drift.
3. Cost: API spend per pinakes, wall-clock time.
4. Recommendation for default-path going forward (likely middle-out: cheap, preserves
   prior work, gets to a current pinakes with full history with minimal new spend).

## Open questions while we wait

- **Embedding source for the comparison.** The pipelines already embed chunks (see
  `embeddings` table); can we reuse those, or do we need to embed the NL artifacts
  separately? Worth checking before building Phase 3.
- **Theme ID stability.** Are theme rows in `themes` keyed by content-derived UUID,
  or randomly assigned? If random, Hungarian-match is the only option. If derived,
  we might get exact matches on identical statements.
- **B's history population.** Confirm that `calli index` against a moving worktree
  writes `*_history` rows on each subsequent run (or only on backfill). If it doesn't,
  B's "history" will only reflect the final HEAD state, undermining the comparison.
  Check this in the logs as B progresses past commit 2.

## Status board (auto-update during the run)

- M: kicked off — incremental at HEAD, then backward backfill (default+theme passes)
- A: kicked off — fresh ingest at HEAD, then backward backfill (default+theme passes)
- B: kicked off — forward walk over 51 first-parent commits in worktree

Logs:
- `/tmp/calli-M-middle-out.log`
- `/tmp/calli-A-fresh.log`
- `/tmp/calli-B-forward-walk.log`
